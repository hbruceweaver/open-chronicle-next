use std::{collections::HashSet, time::Duration};

use chronicle_domain::{
    CaptureCadence, DeviceId, EventEnvelope, EventId, EventKind, EventPayload, EvidenceSource,
    GapReason, RecordingGap,
};
use chronicle_store::{LockManager, ManagedRoot, StoreGeneration};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{EngineError, Result};

const CONFIG_PATH: &str = "config.json";
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HEARTBEAT_ACKNOWLEDGEMENT_PROOFS: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeFaultInjector {
    fail_before_checkpoint_write: bool,
}

impl RuntimeFaultInjector {
    pub const fn before_checkpoint_write() -> Self {
        Self {
            fail_before_checkpoint_write: true,
        }
    }

    pub const fn none() -> Self {
        Self {
            fail_before_checkpoint_write: false,
        }
    }

    fn check_checkpoint_write(self) -> Result<()> {
        if self.fail_before_checkpoint_write {
            Err(EngineError::Configuration(
                "injected failure before runtime checkpoint write".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RuntimeConfigState {
    pub recording_preference: bool,
    pub cadence: CaptureCadence,
    pub session_id: Option<String>,
    pub session_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureAdmissionReason {
    Allowed,
    RuntimeInactive,
    UserPaused,
    StudyNotStarted,
    StudyExpired,
    StorageFreeSpace,
    StorageImageQuota,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaptureAdmission {
    pub allowed: bool,
    pub reason: CaptureAdmissionReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct StartupReconcileRequest {
    pub session_id: String,
    pub device_id: DeviceId,
    pub display_timezone: String,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StartupReconcileResult {
    pub gap_event_ids: Vec<EventId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RuntimeSession {
    session_id: String,
    started_at: DateTime<Utc>,
    last_heartbeat_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

impl RuntimeSession {
    fn new(session_id: String, now: DateTime<Utc>) -> Self {
        Self {
            session_id,
            started_at: now,
            last_heartbeat_at: now,
            closed_at: None,
            extensions: Map::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingStartupReconciliation {
    new_session: RuntimeSession,
    gap_events: Vec<EventEnvelope>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LastStartupReconciliation {
    new_session_id: String,
    gap_event_ids: Vec<EventId>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HeartbeatAcknowledgementProof {
    pub event_id: EventId,
    pub heartbeat_at: DateTime<Utc>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

impl HeartbeatAcknowledgementProof {
    pub fn new(event_id: EventId, heartbeat_at: DateTime<Utc>) -> Self {
        Self {
            event_id,
            heartbeat_at,
            extensions: Map::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HeartbeatAcknowledgementIntent {
    pub proofs: Vec<HeartbeatAcknowledgementProof>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

#[derive(Debug)]
pub(crate) struct PreparedStartupReconciliation {
    pub gap_events: Vec<EventEnvelope>,
    pub gap_event_ids: Vec<EventId>,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeController {
    root: ManagedRoot,
    locks: LockManager,
    generation: StoreGeneration,
}

impl RuntimeController {
    pub fn open(root: ManagedRoot) -> Result<Self> {
        let generation = StoreGeneration::load(&root)?;
        Ok(Self {
            locks: LockManager::new(root.clone(), LOCK_TIMEOUT),
            root,
            generation,
        })
    }

    pub fn state(&self) -> Result<RuntimeConfigState> {
        let document = self.read_document()?;
        let session = current_session(&document)?;
        Ok(RuntimeConfigState {
            recording_preference: recording_preference(&document)?,
            cadence: capture_cadence(&document)?,
            session_id: session.as_ref().map(|session| session.session_id.clone()),
            session_active: session.is_some_and(|session| session.closed_at.is_none()),
        })
    }

    pub fn recording_preference(&self) -> Result<bool> {
        recording_preference(&self.read_document()?)
    }

    pub fn session_active(&self) -> Result<bool> {
        Ok(current_session(&self.read_document()?)?
            .is_some_and(|session| session.closed_at.is_none()))
    }

    pub fn set_recording_preference(&self, enabled: bool) -> Result<()> {
        self.update_document(|document| {
            document.insert("recording_preference".to_owned(), Value::Bool(enabled));
            Ok(())
        })
    }

    pub fn set_cadence(&self, cadence: CaptureCadence) -> Result<()> {
        self.update_document(|document| {
            document.insert(
                "capture_cadence".to_owned(),
                serde_json::to_value(cadence)
                    .map_err(|error| EngineError::Configuration(error.to_string()))?,
            );
            Ok(())
        })
    }

    pub fn heartbeat_intent(&self) -> Result<Option<HeartbeatAcknowledgementIntent>> {
        let intent = get(&self.read_document()?, "heartbeat_acknowledgement_intent")?;
        if let Some(intent) = &intent {
            validate_heartbeat_intent(intent)?;
        }
        Ok(intent)
    }

    pub fn prepare_heartbeat_intent(
        &self,
        proofs: Vec<HeartbeatAcknowledgementProof>,
    ) -> Result<HeartbeatAcknowledgementIntent> {
        let candidate = HeartbeatAcknowledgementIntent {
            proofs,
            extensions: Map::new(),
        };
        validate_heartbeat_intent(&candidate)?;
        self.update_document_with_result(|document| {
            let existing = get::<HeartbeatAcknowledgementIntent>(
                document,
                "heartbeat_acknowledgement_intent",
            )?;
            if let Some(existing) = existing {
                validate_heartbeat_intent(&existing)?;
                if !heartbeat_proofs_match(&existing, &candidate) {
                    return Err(EngineError::Configuration(
                        "a different heartbeat acknowledgement intent is unresolved".to_owned(),
                    ));
                }
                return Ok(existing);
            }
            put(document, "heartbeat_acknowledgement_intent", &candidate)?;
            Ok(candidate)
        })
    }

    pub fn resolve_heartbeat_intent(
        &self,
        expected: &HeartbeatAcknowledgementIntent,
        canonical_event_ids: &[EventId],
        faults: RuntimeFaultInjector,
    ) -> Result<()> {
        validate_heartbeat_intent(expected)?;
        let canonical_event_count = canonical_event_ids.len();
        let canonical_event_ids = canonical_event_ids
            .iter()
            .map(EventId::as_str)
            .collect::<HashSet<_>>();
        if canonical_event_ids.len() != canonical_event_count
            || canonical_event_ids.iter().any(|event_id| {
                !expected
                    .proofs
                    .iter()
                    .any(|proof| proof.event_id.as_str() == *event_id)
            })
        {
            return Err(EngineError::Configuration(
                "canonical heartbeat proofs must be unique members of the intent".to_owned(),
            ));
        }
        self.update_document(|document| {
            let Some(intent) = get::<HeartbeatAcknowledgementIntent>(
                document,
                "heartbeat_acknowledgement_intent",
            )?
            else {
                return Ok(());
            };
            validate_heartbeat_intent(&intent)?;
            if !heartbeat_proofs_match(&intent, expected) {
                return Err(EngineError::Configuration(
                    "heartbeat acknowledgement intent changed during reconciliation".to_owned(),
                ));
            }
            let canonical_heartbeat = intent
                .proofs
                .iter()
                .filter(|proof| canonical_event_ids.contains(proof.event_id.as_str()))
                .map(|proof| proof.heartbeat_at)
                .max();
            if let Some(canonical_heartbeat) = canonical_heartbeat {
                let mut session = current_session(document)?.ok_or_else(|| {
                    EngineError::Configuration(
                        "canonical heartbeat intent has no runtime session".to_owned(),
                    )
                })?;
                faults.check_checkpoint_write()?;
                if canonical_heartbeat > session.last_heartbeat_at {
                    session.last_heartbeat_at = canonical_heartbeat;
                    put(document, "lifecycle_checkpoint", &session)?;
                }
            }
            document.remove("heartbeat_acknowledgement_intent");
            Ok(())
        })
    }

    pub fn prepare_startup(
        &self,
        request: &StartupReconcileRequest,
    ) -> Result<PreparedStartupReconciliation> {
        validate_startup_request(request)?;
        self.update_document_with_result(|document| {
            if let Some(mut pending) =
                get::<PendingStartupReconciliation>(document, "pending_startup_reconciliation")?
            {
                validate_pending_gap_events(&pending.gap_events)?;
                if pending.new_session.session_id != request.session_id {
                    let last_end =
                        pending.gap_events.last().and_then(gap_end).ok_or_else(|| {
                            EngineError::Configuration(
                                "pending startup reconciliation requires a gap event".to_owned(),
                            )
                        })?;
                    if request.now > last_end {
                        pending.gap_events.push(restart_gap_event(
                            last_end,
                            request.now,
                            GapReason::Quit,
                            request,
                        )?);
                    } else if request.now < last_end {
                        // A wall-clock rollback cannot be represented as a
                        // forward contiguous interval. Preserve prior bytes
                        // and append an explicit overlapping correction fact.
                        pending.gap_events.push(restart_gap_event(
                            request.now,
                            last_end,
                            GapReason::ClockCorrection,
                            request,
                        )?);
                    }
                    let extensions = std::mem::take(&mut pending.new_session.extensions);
                    pending.new_session =
                        RuntimeSession::new(request.session_id.clone(), request.now);
                    pending.new_session.extensions = extensions;
                    put(document, "pending_startup_reconciliation", &pending)?;
                }
                let gap_event_ids = pending
                    .gap_events
                    .iter()
                    .map(|event| event.event_id.clone())
                    .collect();
                return Ok(PreparedStartupReconciliation {
                    gap_events: pending.gap_events,
                    gap_event_ids,
                });
            }

            let current = current_session(document)?;
            if current
                .as_ref()
                .is_some_and(|session| session.session_id == request.session_id)
            {
                let gap_event_ids =
                    get::<LastStartupReconciliation>(document, "last_startup_reconciliation")?
                        .filter(|last| last.new_session_id == request.session_id)
                        .map(|last| last.gap_event_ids)
                        .unwrap_or_default();
                return Ok(PreparedStartupReconciliation {
                    gap_events: Vec::new(),
                    gap_event_ids,
                });
            }

            let new_session = RuntimeSession::new(request.session_id.clone(), request.now);
            let Some(prior) = current else {
                put(document, "lifecycle_checkpoint", &new_session)?;
                document.remove("pending_startup_reconciliation");
                return Ok(PreparedStartupReconciliation {
                    gap_events: Vec::new(),
                    gap_event_ids: Vec::new(),
                });
            };

            let prior_boundary = prior.closed_at.unwrap_or(prior.last_heartbeat_at);
            if request.now == prior_boundary {
                put(document, "lifecycle_checkpoint", &new_session)?;
                document.remove("pending_startup_reconciliation");
                return Ok(PreparedStartupReconciliation {
                    gap_events: Vec::new(),
                    gap_event_ids: Vec::new(),
                });
            }

            let (start, end, reason) = if request.now > prior_boundary {
                (prior_boundary, request.now, GapReason::Quit)
            } else {
                (request.now, prior_boundary, GapReason::ClockCorrection)
            };
            let gap_event = restart_gap_event(start, end, reason, request)?;
            let gap_event_id = gap_event.event_id.clone();
            put(
                document,
                "pending_startup_reconciliation",
                &PendingStartupReconciliation {
                    new_session,
                    gap_events: vec![gap_event.clone()],
                    extensions: Map::new(),
                },
            )?;
            Ok(PreparedStartupReconciliation {
                gap_events: vec![gap_event],
                gap_event_ids: vec![gap_event_id],
            })
        })
    }

    pub fn commit_startup(&self, session_id: &str, gap_event_ids: &[EventId]) -> Result<()> {
        self.update_document(|document| {
            let pending =
                get::<PendingStartupReconciliation>(document, "pending_startup_reconciliation")?
                    .ok_or_else(|| {
                        EngineError::Configuration(
                            "startup reconciliation completed without a pending intent".to_owned(),
                        )
                    })?;
            let pending_ids = pending
                .gap_events
                .iter()
                .map(|event| event.event_id.clone())
                .collect::<Vec<_>>();
            if pending.new_session.session_id != session_id || pending_ids != gap_event_ids {
                return Err(EngineError::Configuration(
                    "startup reconciliation does not match its durable intent".to_owned(),
                ));
            }
            put(document, "lifecycle_checkpoint", &pending.new_session)?;
            let extensions =
                get::<LastStartupReconciliation>(document, "last_startup_reconciliation")?
                    .map(|last| last.extensions)
                    .unwrap_or_default();
            put(
                document,
                "last_startup_reconciliation",
                &LastStartupReconciliation {
                    new_session_id: session_id.to_owned(),
                    gap_event_ids: gap_event_ids.to_vec(),
                    extensions,
                },
            )?;
            document.remove("pending_startup_reconciliation");
            Ok(())
        })
    }

    pub fn prepare_termination(&self, session_id: &str, now: DateTime<Utc>) -> Result<()> {
        validate_session_id(session_id)?;
        self.update_document(|document| {
            let mut session = current_session(document)?.ok_or_else(|| {
                EngineError::Configuration("no active runtime session to terminate".to_owned())
            })?;
            if session.session_id != session_id {
                return Err(EngineError::Configuration(
                    "termination session does not match the active session".to_owned(),
                ));
            }
            if now < session.started_at {
                return Err(EngineError::Configuration(
                    "termination cannot precede session start".to_owned(),
                ));
            }
            if session.closed_at.is_some() {
                return Ok(());
            }
            session.closed_at = Some(session.last_heartbeat_at.max(now));
            put(document, "lifecycle_checkpoint", &session)
        })
    }

    fn read_document(&self) -> Result<Map<String, Value>> {
        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let _configuration = shared.configuration()?;
        self.read_document_locked()
    }

    fn update_document(
        &self,
        update: impl FnOnce(&mut Map<String, Value>) -> Result<()>,
    ) -> Result<()> {
        self.update_document_with_result(|document| {
            update(document)?;
            Ok(())
        })
    }

    fn update_document_with_result<T>(
        &self,
        update: impl FnOnce(&mut Map<String, Value>) -> Result<T>,
    ) -> Result<T> {
        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let _configuration = shared.configuration()?;
        let mut document = self.read_document_locked()?;
        let result = update(&mut document)?;
        let bytes = serde_json::to_vec(&Value::Object(document))
            .map_err(|error| EngineError::Configuration(error.to_string()))?;
        self.root.atomic_write(CONFIG_PATH, &bytes)?;
        Ok(result)
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

fn recording_preference(document: &Map<String, Value>) -> Result<bool> {
    document
        .get("recording_preference")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                EngineError::Configuration("recording_preference must be a JSON boolean".to_owned())
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

fn capture_cadence(document: &Map<String, Value>) -> Result<CaptureCadence> {
    document
        .get("capture_cadence")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| EngineError::Configuration(error.to_string()))
        .map(|cadence| cadence.unwrap_or(CaptureCadence::SixtySeconds))
}

fn current_session(document: &Map<String, Value>) -> Result<Option<RuntimeSession>> {
    get(document, "lifecycle_checkpoint")
}

fn validate_heartbeat_intent(intent: &HeartbeatAcknowledgementIntent) -> Result<()> {
    if intent.proofs.is_empty() {
        return Err(EngineError::Configuration(
            "heartbeat acknowledgement intent requires at least one proof".to_owned(),
        ));
    }
    if intent.proofs.len() > MAX_HEARTBEAT_ACKNOWLEDGEMENT_PROOFS {
        return Err(EngineError::Configuration(format!(
            "heartbeat acknowledgement intent exceeds {MAX_HEARTBEAT_ACKNOWLEDGEMENT_PROOFS} proofs"
        )));
    }
    let mut event_ids = HashSet::with_capacity(intent.proofs.len());
    for proof in &intent.proofs {
        if !event_ids.insert(proof.event_id.as_str()) {
            return Err(EngineError::Configuration(
                "heartbeat acknowledgement intent contains duplicate proof event IDs".to_owned(),
            ));
        }
    }
    if intent
        .proofs
        .windows(2)
        .any(|pair| pair[1].heartbeat_at < pair[0].heartbeat_at)
    {
        return Err(EngineError::Configuration(
            "heartbeat acknowledgement proofs must be ordered by factual timestamp".to_owned(),
        ));
    }
    Ok(())
}

fn heartbeat_proofs_match(
    left: &HeartbeatAcknowledgementIntent,
    right: &HeartbeatAcknowledgementIntent,
) -> bool {
    left.proofs.len() == right.proofs.len()
        && left.proofs.iter().zip(&right.proofs).all(|(left, right)| {
            left.event_id == right.event_id && left.heartbeat_at == right.heartbeat_at
        })
}

fn get<T: for<'de> Deserialize<'de>>(
    document: &Map<String, Value>,
    key: &str,
) -> Result<Option<T>> {
    document
        .get(key)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| EngineError::Configuration(error.to_string()))
}

fn put<T: Serialize>(document: &mut Map<String, Value>, key: &str, value: &T) -> Result<()> {
    document.insert(
        key.to_owned(),
        serde_json::to_value(value)
            .map_err(|error| EngineError::Configuration(error.to_string()))?,
    );
    Ok(())
}

fn validate_startup_request(request: &StartupReconcileRequest) -> Result<()> {
    validate_session_id(&request.session_id)?;
    if request.display_timezone.is_empty() || request.display_timezone.len() > 128 {
        return Err(EngineError::Configuration(
            "startup display timezone must be non-empty and bounded".to_owned(),
        ));
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<()> {
    EventId::new(session_id.to_owned())
        .map(|_| ())
        .map_err(EngineError::Identifier)
}

fn gap_end(event: &EventEnvelope) -> Option<DateTime<Utc>> {
    match &event.payload {
        EventPayload::RecordingGap(gap) => Some(gap.end),
        EventPayload::ObservationAttempt(_) | EventPayload::ScreenshotLifecycle(_) => None,
    }
}

fn validate_pending_gap_events(events: &[EventEnvelope]) -> Result<()> {
    if events.is_empty() {
        return Err(EngineError::Configuration(
            "pending startup reconciliation requires at least one gap event".to_owned(),
        ));
    }
    for event in events {
        event.validate().map_err(EngineError::Aggregation)?;
        if !matches!(&event.payload, EventPayload::RecordingGap(_)) {
            return Err(EngineError::Configuration(
                "pending startup reconciliation contains a non-gap event".to_owned(),
            ));
        }
    }
    for pair in events.windows(2) {
        let prior_end = gap_end(&pair[0]).ok_or_else(|| {
            EngineError::Configuration(
                "pending startup reconciliation contains a non-gap event".to_owned(),
            )
        })?;
        let EventPayload::RecordingGap(next) = &pair[1].payload else {
            return Err(EngineError::Configuration(
                "pending startup reconciliation contains a non-gap event".to_owned(),
            ));
        };
        let valid = if next.reason == GapReason::ClockCorrection {
            next.end == prior_end && next.start < next.end
        } else {
            next.start == prior_end
        };
        if !valid {
            return Err(EngineError::Configuration(
                "pending startup gaps must be forward-contiguous or explicit clock corrections"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

fn restart_gap_event(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    reason: GapReason,
    request: &StartupReconcileRequest,
) -> Result<EventEnvelope> {
    let stable_identity = serde_json::to_vec(&(
        start,
        end,
        reason,
        request.device_id.as_str(),
        &request.display_timezone,
    ))
    .map_err(|error| EngineError::Configuration(error.to_string()))?;
    let digest = chronicle_store::checksum::checksum_bytes(&stable_identity);
    let event_id = EventId::new(format!("startup-gap-{digest}"))?;
    let event = EventEnvelope {
        schema_version: chronicle_domain::CONTRACT_VERSION.to_owned(),
        event_id,
        device_id: request.device_id.clone(),
        scheduled_at: None,
        observed_at: end,
        recorded_at: end,
        display_timezone: request.display_timezone.clone(),
        source: EvidenceSource {
            adapter: "app-lifecycle".to_owned(),
            version: "1.0".to_owned(),
        },
        kind: EventKind::RecordingGap,
        payload: EventPayload::RecordingGap(RecordingGap { start, end, reason }),
    };
    event.validate().map_err(EngineError::Aggregation)?;
    Ok(event)
}
