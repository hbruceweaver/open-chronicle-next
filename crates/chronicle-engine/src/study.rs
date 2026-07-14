use std::time::Duration as StdDuration;

use chronicle_domain::{
    DurableAcknowledgement, EventEnvelope, EventPayload, ImageArtifactId, ObservationContent,
    ScreenshotProjectedState, StudyHealthState, StudyHealthSummary,
};
use chronicle_store::{
    CanonicalJournal, FaultInjector, LockManager, ManagedRoot, Projector, ScreenshotStore,
    SqliteStore, StoreGeneration,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    CadenceStamp, ChunkerConfig, EngineError, IngestEngine, IngestOutcome, IngestRequest, Result,
};

const CONFIG_PATH: &str = "config.json";
const LOCK_TIMEOUT: StdDuration = StdDuration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyBoundary {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl StudyBoundary {
    fn validate(self) -> Result<Self> {
        if self.start >= self.end {
            return Err(EngineError::Configuration(
                "study start must precede study end".to_owned(),
            ));
        }
        Ok(self)
    }

    pub fn contains(self, now: DateTime<Utc>) -> bool {
        self.start <= now && now < self.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum RecordingMode {
    Personal,
    Study {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        #[serde(default)]
        expired_at: Option<DateTime<Utc>>,
    },
}

impl RecordingMode {
    fn boundary(self) -> Option<StudyBoundary> {
        match self {
            Self::Personal => None,
            Self::Study { start, end, .. } => Some(StudyBoundary { start, end }),
        }
    }
}

#[derive(Clone, Debug)]
struct StudyController {
    root: ManagedRoot,
    locks: LockManager,
    generation: StoreGeneration,
}

impl StudyController {
    fn open(root: ManagedRoot) -> Result<Self> {
        let generation = StoreGeneration::load(&root)?;
        Ok(Self {
            locks: LockManager::new(root.clone(), LOCK_TIMEOUT),
            root,
            generation,
        })
    }

    fn mode(&self) -> Result<RecordingMode> {
        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let _config = shared.configuration()?;
        self.read_mode_locked()
    }

    fn configure_study(&self, boundary: StudyBoundary) -> Result<StudyBoundary> {
        let boundary = boundary.validate()?;
        self.write_mode(RecordingMode::Study {
            start: boundary.start,
            end: boundary.end,
            expired_at: None,
        })?;
        Ok(boundary)
    }

    fn set_personal(&self) -> Result<()> {
        self.write_mode(RecordingMode::Personal)
    }

    fn extend(&self, new_end: DateTime<Utc>, now: DateTime<Utc>) -> Result<StudyBoundary> {
        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let _config = shared.configuration()?;
        let boundary = self.read_mode_locked()?.boundary().ok_or_else(|| {
            EngineError::Configuration("personal mode has no study to extend".to_owned())
        })?;
        if new_end <= now || new_end <= boundary.end {
            return Err(EngineError::Configuration(
                "study extension must end after both now and the prior end".to_owned(),
            ));
        }
        let extended = StudyBoundary {
            start: boundary.start,
            end: new_end,
        };
        self.write_mode_locked(RecordingMode::Study {
            start: extended.start,
            end: extended.end,
            expired_at: None,
        })?;
        Ok(extended)
    }

    fn require_capture(&self, now: DateTime<Utc>) -> Result<()> {
        match self.mode_at(now, true)? {
            RecordingMode::Personal => Ok(()),
            RecordingMode::Study {
                expired_at: Some(_),
                ..
            } => Err(EngineError::StudyExpired),
            RecordingMode::Study { start, .. } if now < start => Err(EngineError::StudyNotStarted),
            RecordingMode::Study { .. } => Ok(()),
        }
    }

    fn health_summary(&self, now: DateTime<Utc>) -> Result<StudyHealthSummary> {
        let summary = match self.mode_at(now, false)? {
            RecordingMode::Personal => StudyHealthSummary {
                state: StudyHealthState::Personal,
                start: None,
                end: None,
                expired_at: None,
            },
            RecordingMode::Study {
                start,
                end,
                expired_at,
            } => StudyHealthSummary {
                state: if expired_at.is_some() || now >= end {
                    StudyHealthState::Expired
                } else if now < start {
                    StudyHealthState::Scheduled
                } else {
                    StudyHealthState::Active
                },
                start: Some(start),
                end: Some(end),
                // Shared health is observational. `expired_at` means the
                // persisted app-internal latch time, so an elapsed but not yet
                // latched study reports Expired with no fabricated timestamp.
                expired_at,
            },
        };
        summary.validate().map_err(EngineError::Configuration)?;
        Ok(summary)
    }

    fn capture_allowed(&self, now: DateTime<Utc>) -> Result<bool> {
        match self.require_capture(now) {
            Ok(()) => Ok(true),
            Err(EngineError::StudyNotStarted | EngineError::StudyExpired) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn write_mode(&self, mode: RecordingMode) -> Result<()> {
        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let _config = shared.configuration()?;
        self.write_mode_locked(mode)
    }

    fn mode_at(&self, now: DateTime<Utc>, latch_expiry: bool) -> Result<RecordingMode> {
        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let _config = shared.configuration()?;
        let mut mode = self.read_mode_locked()?;
        if let RecordingMode::Study {
            start,
            end,
            expired_at: None,
        } = mode
            && now >= end
            && latch_expiry
        {
            mode = RecordingMode::Study {
                start,
                end,
                expired_at: Some(now),
            };
            self.write_mode_locked(mode)?;
        }
        Ok(mode)
    }

    fn read_mode_locked(&self) -> Result<RecordingMode> {
        let document = self.read_document_locked()?;
        document
            .get("recording_mode")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| EngineError::Configuration(error.to_string()))
            .map(|mode| mode.unwrap_or(RecordingMode::Personal))
            .and_then(|mode| {
                if let Some(boundary) = mode.boundary() {
                    boundary.validate()?;
                }
                if let RecordingMode::Study {
                    end,
                    expired_at: Some(expired_at),
                    ..
                } = mode
                    && expired_at < end
                {
                    return Err(EngineError::Configuration(
                        "study expiry latch cannot precede the study end".to_owned(),
                    ));
                }
                Ok(mode)
            })
    }

    fn write_mode_locked(&self, mode: RecordingMode) -> Result<()> {
        let mut document = self.read_document_locked()?;
        let mut encoded = serde_json::to_value(mode)
            .map_err(|error| EngineError::Configuration(error.to_string()))?;
        if let (Some(existing), Some(next)) = (
            document.get("recording_mode").and_then(Value::as_object),
            encoded.as_object_mut(),
        ) && existing.get("type") == next.get("type")
        {
            // Unknown keys belong to this recording-mode variant and survive
            // same-mode latching/extensions. A deliberate variant transition
            // starts from the new variant's owned shape instead.
            for (key, value) in existing {
                next.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        document.insert("recording_mode".to_owned(), encoded);
        let bytes = serde_json::to_vec(&Value::Object(document))
            .map_err(|error| EngineError::Configuration(error.to_string()))?;
        self.root.atomic_write(CONFIG_PATH, &bytes)?;
        Ok(())
    }

    fn read_document_locked(&self) -> Result<Map<String, Value>> {
        if !self.root.exists(CONFIG_PATH)? {
            return Ok(Map::new());
        }
        let value: Value = serde_json::from_slice(&self.root.read(CONFIG_PATH)?)
            .map_err(|error| EngineError::Configuration(error.to_string()))?;
        value.as_object().cloned().ok_or_else(|| {
            EngineError::Configuration("config.json must contain a JSON object".to_owned())
        })
    }
}

#[derive(Clone, Debug)]
pub struct RetainedImageAcknowledgement {
    pub acknowledgement: DurableAcknowledgement,
    pub artifact_id: ImageArtifactId,
    pub lifecycle_state: ScreenshotProjectedState,
    pub ingest: IngestOutcome,
}

/// App-internal serialized recording boundary. It is intentionally separate
/// from SharedServiceOperation so MCP cannot capture, pause, retain, or delete.
#[derive(Debug)]
pub struct RecordingCoordinator {
    root: ManagedRoot,
    ingest: IngestEngine,
    screenshots: ScreenshotStore,
    study: StudyController,
}

pub(crate) fn study_health_summary(
    root: ManagedRoot,
    now: DateTime<Utc>,
) -> Result<StudyHealthSummary> {
    StudyController::open(root)?.health_summary(now)
}

impl RecordingCoordinator {
    pub fn open(root: ManagedRoot, chunker: ChunkerConfig) -> Result<Self> {
        Self::open_at(root, chunker, Utc::now())
    }

    pub fn open_at(root: ManagedRoot, chunker: ChunkerConfig, now: DateTime<Utc>) -> Result<Self> {
        let ingest = IngestEngine::open_at(root.clone(), chunker, now)?;
        let sqlite = SqliteStore::open(root.clone())?;
        let screenshots = ScreenshotStore::new(
            root.clone(),
            CanonicalJournal::new(root.clone()),
            Projector::new(sqlite),
        )?;
        let study = StudyController::open(root.clone())?;
        Ok(Self {
            root,
            ingest,
            screenshots,
            study,
        })
    }

    pub fn configure_study(&mut self, boundary: StudyBoundary) -> Result<StudyBoundary> {
        self.study.configure_study(boundary)
    }

    pub fn use_personal_mode(&mut self) -> Result<()> {
        self.study.set_personal()
    }

    pub fn extend_study(
        &mut self,
        new_end: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<StudyBoundary> {
        self.study.extend(new_end, now)
    }

    pub fn study_boundary(&self) -> Result<Option<StudyBoundary>> {
        Ok(self.study.mode()?.boundary())
    }

    pub fn capture_allowed(&self, now: DateTime<Utc>) -> Result<bool> {
        self.study.capture_allowed(now)
    }

    pub fn ingest(&mut self, request: IngestRequest, now: DateTime<Utc>) -> Result<IngestOutcome> {
        self.require_live_event(&request.event, now)?;
        if matches!(
            &request.event.payload,
            EventPayload::ObservationAttempt(attempt)
                if matches!(&attempt.content, ObservationContent::Captured(content) if content.image.is_some())
        ) {
            return Err(EngineError::Configuration(
                "image-bearing observations must use retain_screenshot".to_owned(),
            ));
        }
        self.ingest.ingest(request, now)
    }

    pub fn retain_screenshot(
        &mut self,
        observation: &EventEnvelope,
        encoded_image: &[u8],
        completion: &EventEnvelope,
        cadence: CadenceStamp,
        now: DateTime<Utc>,
        faults: FaultInjector,
    ) -> Result<RetainedImageAcknowledgement> {
        self.require_live_event(observation, now)?;
        completion.validate().map_err(EngineError::Aggregation)?;
        if completion.recorded_at > now {
            return Err(EngineError::Configuration(
                "screenshot lifecycle completion cannot be from the future".to_owned(),
            ));
        }
        let artifact_id = match &observation.payload {
            EventPayload::ObservationAttempt(attempt) => match &attempt.content {
                ObservationContent::Captured(content) => content
                    .image
                    .as_ref()
                    .map(|image| image.artifact_id.clone()),
                ObservationContent::Unchanged(_)
                | ObservationContent::Protected(_)
                | ObservationContent::NoEvidence(_) => None,
            },
            EventPayload::RecordingGap(_) | EventPayload::ScreenshotLifecycle(_) => None,
        }
        .ok_or_else(|| {
            EngineError::Configuration("retained observation has no image intent".to_owned())
        })?;
        self.ingest
            .prepare_transactional_image(observation, &cadence)?;
        self.screenshots
            .retain(observation, encoded_image, completion, faults)?;
        let ingest = self
            .ingest
            .reconcile_transactional_image(now, FaultInjector::none())?;
        // ScreenshotStore returns only after observation sync, promotion and
        // directory sync, lifecycle completion sync, and projection.
        Ok(RetainedImageAcknowledgement {
            acknowledgement: DurableAcknowledgement::Durable,
            artifact_id,
            lifecycle_state: ScreenshotProjectedState::Retained,
            ingest,
        })
    }

    /// App-internal periodic repair hook. The macOS coordinator calls this on
    /// startup and on a bounded timer so a live fault does not require process
    /// restart to finish or remove a pending image transaction.
    pub fn reconcile_pending_images(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<chronicle_store::RecoveryReport> {
        let report = self.ingest.recover_projection(now)?;
        let sqlite = SqliteStore::open(self.root.clone())?;
        self.screenshots = ScreenshotStore::new(
            self.root.clone(),
            CanonicalJournal::new(self.root.clone()),
            Projector::new(sqlite),
        )?;
        let _ = self
            .ingest
            .reconcile_transactional_image(now, FaultInjector::none())?;
        Ok(report)
    }

    fn require_live_event(&self, event: &EventEnvelope, now: DateTime<Utc>) -> Result<()> {
        event.validate().map_err(EngineError::Aggregation)?;
        if event.recorded_at > now {
            return Err(EngineError::Configuration(
                "live recording event cannot be from the future".to_owned(),
            ));
        }
        if matches!(event.payload, EventPayload::ObservationAttempt(_)) {
            self.study.require_capture(event.observed_at)?;
        }
        // The second check is the post-capture/wake boundary. Crossing the
        // half-open end while capture is in flight rejects pixels before write.
        self.study.require_capture(now)
    }
}
