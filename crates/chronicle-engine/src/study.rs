use std::time::Duration as StdDuration;

use chronicle_domain::{
    CaptureCadence, DurableAcknowledgement, EventEnvelope, EventPayload, ImageArtifactId,
    NoEvidenceReason, ObservationContent, ScreenshotProjectedState, ScreenshotRetention,
    StudyHealthState, StudyHealthSummary,
};
use chronicle_store::{
    CanonicalJournal, FaultInjector, LockManager, ManagedRoot, Projector, ScreenshotStore,
    SqliteStore, StoreGeneration,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    CadenceStamp, CaptureAdmission, CaptureAdmissionReason, ChunkerConfig, EngineError,
    HeartbeatAcknowledgementProof, IngestEngine, IngestOutcome, IngestRequest, Result,
    RuntimeConfigState, RuntimeController, RuntimeFaultInjector, RuntimeGapReconcileRequest,
    RuntimeGapReconcileResult, StartupReconcileRequest, StartupReconcileResult,
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
    runtime: RuntimeController,
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
        let runtime = RuntimeController::open(root.clone())?;
        Ok(Self {
            root,
            ingest,
            screenshots,
            study,
            runtime,
        })
    }

    pub fn runtime_state(&self, now: DateTime<Utc>) -> Result<RuntimeConfigState> {
        // Validate and expose study expiry through the existing policy owner,
        // while keeping the persisted runtime settings in one config document.
        let _ = self.study.health_summary(now)?;
        self.runtime.state()
    }

    pub fn set_recording_preference(&mut self, enabled: bool) -> Result<()> {
        self.runtime.set_recording_preference(enabled)
    }

    pub fn set_cadence(&mut self, cadence: CaptureCadence) -> Result<()> {
        self.runtime.set_cadence(cadence)
    }

    pub fn set_screenshot_retention(&mut self, retention: ScreenshotRetention) -> Result<()> {
        self.runtime.set_screenshot_retention(retention)
    }

    pub fn capture_admission(&mut self, now: DateTime<Utc>) -> Result<CaptureAdmission> {
        let runtime = self.runtime.state()?;
        if !runtime.session_active {
            return Ok(CaptureAdmission {
                allowed: false,
                reason: CaptureAdmissionReason::RuntimeInactive,
            });
        }
        if !runtime.recording_preference {
            return Ok(CaptureAdmission {
                allowed: false,
                reason: CaptureAdmissionReason::UserPaused,
            });
        }
        match self.study.require_capture(now) {
            Ok(()) => {
                let storage = self.screenshots.storage_health()?;
                let reason = match storage.state {
                    chronicle_store::ScreenshotStorageState::BlockedFreeSpace => {
                        CaptureAdmissionReason::StorageFreeSpace
                    }
                    chronicle_store::ScreenshotStorageState::BlockedImageQuota => {
                        CaptureAdmissionReason::StorageImageQuota
                    }
                    chronicle_store::ScreenshotStorageState::Healthy
                    | chronicle_store::ScreenshotStorageState::Warning => {
                        CaptureAdmissionReason::Allowed
                    }
                };
                Ok(CaptureAdmission {
                    allowed: reason == CaptureAdmissionReason::Allowed,
                    reason,
                })
            }
            Err(EngineError::StudyNotStarted) => Ok(CaptureAdmission {
                allowed: false,
                reason: CaptureAdmissionReason::StudyNotStarted,
            }),
            Err(EngineError::StudyExpired) => Ok(CaptureAdmission {
                allowed: false,
                reason: CaptureAdmissionReason::StudyExpired,
            }),
            Err(error) => Err(error),
        }
    }

    pub fn startup_reconcile(
        &mut self,
        request: StartupReconcileRequest,
    ) -> Result<StartupReconcileResult> {
        self.startup_reconcile_with_faults(request, FaultInjector::none())
    }

    pub fn startup_reconcile_with_faults(
        &mut self,
        request: StartupReconcileRequest,
        event_faults: FaultInjector,
    ) -> Result<StartupReconcileResult> {
        self.reconcile_heartbeat_intent()?;
        self.reconcile_pending_runtime_gap()?;
        let prepared = self.runtime.prepare_startup(&request)?;
        let has_pending_events = !prepared.gap_events.is_empty();
        for event in prepared.gap_events {
            let ingest_now = request.now.max(event.recorded_at);
            self.ingest.ingest_with_faults(
                IngestRequest {
                    event,
                    cadence: None,
                },
                ingest_now,
                event_faults,
                FaultInjector::none(),
            )?;
        }
        if has_pending_events {
            self.runtime
                .commit_startup(&request.session_id, &prepared.gap_event_ids)?;
        }
        Ok(StartupReconcileResult {
            gap_event_ids: prepared.gap_event_ids,
        })
    }

    pub fn prepare_termination(&mut self, session_id: &str, now: DateTime<Utc>) -> Result<()> {
        self.reconcile_heartbeat_intent()?;
        self.reconcile_pending_runtime_gap()?;
        self.runtime.prepare_termination(session_id, now)
    }

    pub fn reconcile_runtime_gap(
        &mut self,
        request: RuntimeGapReconcileRequest,
    ) -> Result<RuntimeGapReconcileResult> {
        self.reconcile_runtime_gap_with_faults(
            request,
            FaultInjector::none(),
            RuntimeFaultInjector::none(),
        )
    }

    pub fn reconcile_runtime_gap_with_faults(
        &mut self,
        request: RuntimeGapReconcileRequest,
        event_faults: FaultInjector,
        checkpoint_faults: RuntimeFaultInjector,
    ) -> Result<RuntimeGapReconcileResult> {
        self.reconcile_heartbeat_intent()?;
        self.reconcile_pending_runtime_gap()?;
        let prepared = self.runtime.prepare_runtime_gap(&request)?;
        for event in &prepared.gap_events {
            self.ingest.ingest_with_faults(
                IngestRequest {
                    event: event.clone(),
                    cadence: None,
                },
                request.now,
                event_faults,
                FaultInjector::none(),
            )?;
        }
        if let Some(pending) = &prepared.pending {
            self.runtime
                .commit_runtime_gap(pending, &prepared.gap_event_ids, checkpoint_faults)?;
        }
        Ok(RuntimeGapReconcileResult {
            gap_event_ids: prepared.gap_event_ids,
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
        self.ingest_with_faults(request, now, FaultInjector::none(), FaultInjector::none())
    }

    pub fn ingest_with_faults(
        &mut self,
        request: IngestRequest,
        now: DateTime<Utc>,
        event_faults: FaultInjector,
        chunk_faults: FaultInjector,
    ) -> Result<IngestOutcome> {
        self.ingest_with_runtime_faults(
            request,
            now,
            event_faults,
            chunk_faults,
            RuntimeFaultInjector::none(),
        )
    }

    pub fn ingest_with_runtime_faults(
        &mut self,
        request: IngestRequest,
        now: DateTime<Utc>,
        event_faults: FaultInjector,
        chunk_faults: FaultInjector,
        heartbeat_faults: RuntimeFaultInjector,
    ) -> Result<IngestOutcome> {
        self.reconcile_heartbeat_intent()?;
        self.reconcile_pending_runtime_gap()?;
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
        let acknowledgement_event_id = request.event.event_id.clone();
        let heartbeat_intent =
            self.runtime
                .prepare_heartbeat_intent(vec![HeartbeatAcknowledgementProof::new(
                    acknowledgement_event_id.clone(),
                    now,
                )])?;
        let outcome = self
            .ingest
            .ingest_with_faults(request, now, event_faults, chunk_faults)?;
        if matches!(
            outcome.acknowledgement,
            DurableAcknowledgement::Durable
                | DurableAcknowledgement::JournalDurableProjectionPending
        ) {
            self.runtime.resolve_heartbeat_intent(
                &heartbeat_intent,
                std::slice::from_ref(&acknowledgement_event_id),
                heartbeat_faults,
            )?;
        }
        Ok(outcome)
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
        self.retain_screenshot_with_runtime_faults(
            observation,
            encoded_image,
            completion,
            cadence,
            now,
            faults,
            RuntimeFaultInjector::none(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn retain_screenshot_with_runtime_faults(
        &mut self,
        observation: &EventEnvelope,
        encoded_image: &[u8],
        completion: &EventEnvelope,
        cadence: CadenceStamp,
        now: DateTime<Utc>,
        faults: FaultInjector,
        heartbeat_faults: RuntimeFaultInjector,
    ) -> Result<RetainedImageAcknowledgement> {
        self.reconcile_heartbeat_intent()?;
        self.reconcile_pending_runtime_gap()?;
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
        let retention = self
            .screenshots
            .prepare_retain(observation, encoded_image, completion)?;
        let heartbeat_intent = self.runtime.prepare_heartbeat_intent(vec![
            HeartbeatAcknowledgementProof::new(
                observation.event_id.clone(),
                observation.recorded_at,
            ),
            HeartbeatAcknowledgementProof::new(completion.event_id.clone(), now),
        ])?;
        self.ingest
            .prepare_transactional_image(observation, &cadence)?;
        retention.commit(faults)?;
        self.runtime.resolve_heartbeat_intent(
            &heartbeat_intent,
            std::slice::from_ref(&completion.event_id),
            heartbeat_faults,
        )?;
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
        let storage_limits = self.screenshots.storage_limits();
        let storage_available_bytes = self.screenshots.storage_available_bytes_probe();
        self.screenshots = ScreenshotStore::new(
            self.root.clone(),
            CanonicalJournal::new(self.root.clone()),
            Projector::new(sqlite),
        )?
        .with_storage_limits(storage_limits)?
        .with_storage_available_bytes_probe(storage_available_bytes);
        let _ = self
            .ingest
            .reconcile_transactional_image(now, FaultInjector::none())?;
        Ok(report)
    }

    pub fn screenshot_storage_health(&self) -> Result<chronicle_store::ScreenshotStorageHealth> {
        self.screenshots.storage_health().map_err(Into::into)
    }

    pub fn set_screenshot_storage_limits(
        &mut self,
        limits: chronicle_store::ScreenshotStorageLimits,
    ) -> Result<()> {
        self.screenshots
            .set_storage_limits(limits)
            .map_err(Into::into)
    }

    pub fn set_screenshot_storage_available_bytes_probe(
        &mut self,
        probe: chronicle_store::StorageAvailableBytesProbe,
    ) {
        self.screenshots.set_storage_available_bytes_probe(probe);
    }

    fn reconcile_heartbeat_intent(&mut self) -> Result<()> {
        let Some(intent) = self.runtime.heartbeat_intent()? else {
            return Ok(());
        };
        let proof_event_ids = intent
            .proofs
            .iter()
            .map(|proof| proof.event_id.clone())
            .collect::<Vec<_>>();
        let canonical_presence = self.ingest.canonical_event_presence(&proof_event_ids)?;
        let canonical_event_ids = proof_event_ids
            .into_iter()
            .zip(canonical_presence)
            .filter_map(|(event_id, canonical)| canonical.then_some(event_id))
            .collect::<Vec<_>>();
        self.runtime.resolve_heartbeat_intent(
            &intent,
            &canonical_event_ids,
            RuntimeFaultInjector::none(),
        )
    }

    fn reconcile_pending_runtime_gap(&mut self) -> Result<()> {
        let Some(pending) = self.runtime.pending_runtime_gap()? else {
            return Ok(());
        };
        let event = pending.gap_event.clone();
        let event_id = event.event_id.clone();
        self.ingest.ingest_with_faults(
            IngestRequest {
                event: event.clone(),
                cadence: None,
            },
            pending.reconciled_at,
            FaultInjector::none(),
            FaultInjector::none(),
        )?;
        self.runtime.commit_runtime_gap(
            &pending,
            std::slice::from_ref(&event_id),
            RuntimeFaultInjector::none(),
        )
    }

    fn require_live_event(&self, event: &EventEnvelope, now: DateTime<Utc>) -> Result<()> {
        event.validate().map_err(EngineError::Aggregation)?;
        if event.recorded_at > now {
            return Err(EngineError::Configuration(
                "live recording event cannot be from the future".to_owned(),
            ));
        }
        if matches!(event.payload, EventPayload::ObservationAttempt(_)) {
            if !self.runtime.session_active()? {
                return Err(EngineError::Configuration(
                    "recording runtime session is inactive".to_owned(),
                ));
            }
            let user_paused = matches!(
                &event.payload,
                EventPayload::ObservationAttempt(attempt)
                    if matches!(
                        &attempt.content,
                        ObservationContent::NoEvidence(content)
                            if content.reason == NoEvidenceReason::UserPaused
                    )
            );
            match (self.runtime.recording_preference()?, user_paused) {
                (false, false) => {
                    return Err(EngineError::Configuration(
                        "recording is paused; only a user-paused factual attempt is allowed"
                            .to_owned(),
                    ));
                }
                (true, true) => {
                    return Err(EngineError::Configuration(
                        "user-paused factual attempt is stale because recording is not paused"
                            .to_owned(),
                    ));
                }
                (false, true) | (true, false) => {}
            }
            self.study.require_capture(event.observed_at)?;
        }
        // The second check is the post-capture/wake boundary. Crossing the
        // half-open end while capture is in flight rejects pixels before write.
        self.study.require_capture(now)
    }
}
