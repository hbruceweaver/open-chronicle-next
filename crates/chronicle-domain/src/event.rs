use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::ManagedRelativePath;
use crate::{ContractError, DeviceId, EventId, ImageArtifactId, parse_versioned};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UntrustedEvidenceMarker;

impl Serialize for UntrustedEvidenceMarker {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for UntrustedEvidenceMarker {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let marker = bool::deserialize(deserializer)?;
        marker
            .then_some(Self)
            .ok_or_else(|| serde::de::Error::custom("untrusted_evidence must be true"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    ObservationAttempt,
    RecordingGap,
    ScreenshotLifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptStatus {
    Completed,
    Skipped,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceState {
    CapturedNew,
    CapturedUnchanged,
    Protected,
    Paused,
    Unavailable,
    CaptureFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresenceState {
    Active,
    Idle,
    Locked,
    Asleep,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OcrState {
    Complete,
    Empty,
    Partial,
    Failed,
    NotRun,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OcrChange {
    New,
    Changed,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrEvidence {
    pub text: String,
    pub change: OcrChange,
    pub confidence: Option<f32>,
    pub engine: EvidenceSource,
    pub automatic_language_detection: bool,
    pub recognition_languages: Vec<String>,
    pub untrusted_evidence: UntrustedEvidenceMarker,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSource {
    pub adapter: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedDomainContext {
    pub adapter: String,
    pub domain: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermittedWindowContext {
    pub application_bundle_id: String,
    pub process_name: String,
    pub window_title: Option<String>,
    pub authorized_domain: Option<AuthorizedDomainContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
    pub scale_milli: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageIntentState {
    Pending,
}

/// Canonical local evidence records the validated managed relative location needed
/// for deterministic recovery. Query/MCP types deliberately project only the opaque
/// artifact ID and lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageReference {
    pub artifact_id: ImageArtifactId,
    pub managed_relative_path: ManagedRelativePath,
    pub content_hash: String,
    pub dimensions: ImageDimensions,
    pub expires_at: DateTime<Utc>,
    pub intent_state: ImageIntentState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedContent {
    pub context: PermittedWindowContext,
    pub content_hash: String,
    pub ocr: Option<OcrEvidence>,
    pub image: Option<ImageReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnchangedContent {
    pub context: PermittedWindowContext,
    pub content_hash: String,
    pub previous_event_id: EventId,
    pub reused_ocr_event_id: Option<EventId>,
    pub image_artifact_id: Option<ImageArtifactId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtectedReason {
    SecureInput,
    ApplicationExcluded,
    TitleExcluded,
    ChronicleSelf,
    ForegroundChanged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedContent {
    pub reason: ProtectedReason,
    pub privacy_policy_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoEvidenceReason {
    UserPaused,
    StudyExpired,
    PermissionDenied,
    Locked,
    Asleep,
    NoExactWindow,
    AmbiguousWindow,
    CaptureApiFailure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoEvidenceContent {
    pub reason: NoEvidenceReason,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum ObservationContent {
    Captured(CapturedContent),
    Unchanged(UnchangedContent),
    Protected(ProtectedContent),
    NoEvidence(NoEvidenceContent),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObservationAttempt {
    pub cadence_seconds: u32,
    pub attempt_status: AttemptStatus,
    pub evidence_state: EvidenceState,
    pub presence_state: PresenceState,
    pub idle_seconds: Option<u32>,
    pub ocr_state: OcrState,
    pub content: ObservationContent,
}

impl ObservationAttempt {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.cadence_seconds, 30 | 60) {
            return Err("cadence_seconds must be 30 or 60".to_owned());
        }
        match (self.presence_state, self.idle_seconds) {
            (PresenceState::Idle, Some(seconds)) if seconds > 0 => {}
            (PresenceState::Idle, _) => {
                return Err("idle presence requires a positive idle_seconds value".to_owned());
            }
            (_, Some(_)) => {
                return Err("idle_seconds is allowed only for idle presence".to_owned());
            }
            (_, None) => {}
        }
        let matches_axis = match (self.attempt_status, self.evidence_state, &self.content) {
            (
                AttemptStatus::Completed,
                EvidenceState::CapturedNew,
                ObservationContent::Captured(_),
            )
            | (
                AttemptStatus::Completed,
                EvidenceState::CapturedUnchanged,
                ObservationContent::Unchanged(_),
            )
            | (
                AttemptStatus::Skipped,
                EvidenceState::Protected,
                ObservationContent::Protected(_),
            ) => true,
            (
                AttemptStatus::Skipped,
                EvidenceState::Paused,
                ObservationContent::NoEvidence(content),
            ) => matches!(
                content.reason,
                NoEvidenceReason::UserPaused | NoEvidenceReason::StudyExpired
            ),
            (
                AttemptStatus::Skipped,
                EvidenceState::Unavailable,
                ObservationContent::NoEvidence(content),
            ) => matches!(
                content.reason,
                NoEvidenceReason::PermissionDenied
                    | NoEvidenceReason::Locked
                    | NoEvidenceReason::Asleep
                    | NoEvidenceReason::NoExactWindow
                    | NoEvidenceReason::AmbiguousWindow
            ),
            (
                AttemptStatus::Failed,
                EvidenceState::CaptureFailed,
                ObservationContent::NoEvidence(content),
            ) => content.reason == NoEvidenceReason::CaptureApiFailure,
            _ => false,
        };
        if !matches_axis {
            return Err("evidence_state and content type disagree".to_owned());
        }
        let presence_matches = matches!(
            (&self.content, self.presence_state),
            (
                ObservationContent::Captured(_) | ObservationContent::Unchanged(_),
                PresenceState::Active | PresenceState::Idle | PresenceState::Unknown,
            ) | (
                ObservationContent::NoEvidence(NoEvidenceContent {
                    reason: NoEvidenceReason::Locked,
                }),
                PresenceState::Locked,
            ) | (
                ObservationContent::NoEvidence(NoEvidenceContent {
                    reason: NoEvidenceReason::Asleep,
                }),
                PresenceState::Asleep,
            ) | (
                ObservationContent::Protected(_)
                    | ObservationContent::NoEvidence(NoEvidenceContent {
                        reason: NoEvidenceReason::UserPaused
                            | NoEvidenceReason::StudyExpired
                            | NoEvidenceReason::PermissionDenied
                            | NoEvidenceReason::NoExactWindow
                            | NoEvidenceReason::AmbiguousWindow
                            | NoEvidenceReason::CaptureApiFailure,
                    }),
                PresenceState::Active | PresenceState::Idle | PresenceState::Unknown,
            )
        );
        if !presence_matches {
            return Err("presence_state and evidence outcome disagree".to_owned());
        }
        match &self.content {
            ObservationContent::Captured(content) => {
                validate_window_context(&content.context)?;
                validate_content_hash(&content.content_hash)?;
                let has_ocr = content.ocr.is_some();
                let ocr_consistent = match self.ocr_state {
                    OcrState::Complete | OcrState::Empty | OcrState::Partial => has_ocr,
                    OcrState::Failed | OcrState::NotRun => !has_ocr,
                };
                if !ocr_consistent || self.ocr_state == OcrState::NotRun {
                    return Err("ocr_state and OCR payload disagree".to_owned());
                }
                if let Some(ocr) = &content.ocr {
                    if ocr.engine.adapter.is_empty() || ocr.engine.version.is_empty() {
                        return Err("OCR engine provenance fields must be non-empty".to_owned());
                    }
                    if ocr.recognition_languages.iter().any(String::is_empty)
                        || ocr
                            .recognition_languages
                            .iter()
                            .enumerate()
                            .any(|(index, language)| {
                                ocr.recognition_languages[..index].contains(language)
                            })
                    {
                        return Err(
                            "OCR recognition languages must be non-empty and unique".to_owned()
                        );
                    }
                    if ocr
                        .confidence
                        .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
                    {
                        return Err("OCR confidence must be between zero and one".to_owned());
                    }
                    let text_consistent = match self.ocr_state {
                        OcrState::Empty => ocr.text.is_empty(),
                        OcrState::Complete | OcrState::Partial => !ocr.text.is_empty(),
                        OcrState::Failed | OcrState::NotRun => false,
                    };
                    if !text_consistent {
                        return Err("ocr_state and OCR text emptiness disagree".to_owned());
                    }
                }
                if let Some(image) = &content.image {
                    let pixels = u64::from(image.dimensions.width)
                        .checked_mul(u64::from(image.dimensions.height))
                        .ok_or_else(|| "image dimensions overflow".to_owned())?;
                    if image.dimensions.width == 0
                        || image.dimensions.height == 0
                        || image.dimensions.scale_milli == 0
                        || image.dimensions.width.max(image.dimensions.height) > 2_560
                        || pixels > 8_000_000
                    {
                        return Err("image dimensions exceed the v1 capture bounds".to_owned());
                    }
                    if image.content_hash != content.content_hash {
                        return Err("image and captured-content hashes must match".to_owned());
                    }
                }
                Ok(())
            }
            ObservationContent::Unchanged(content) => {
                validate_window_context(&content.context)?;
                validate_content_hash(&content.content_hash)?;
                (self.ocr_state == OcrState::NotRun)
                    .then_some(())
                    .ok_or_else(|| "unchanged content must use ocr_state=not-run".to_owned())
            }
            ObservationContent::Protected(content) => {
                if content.privacy_policy_version.is_empty() {
                    return Err("protected content requires a privacy policy version".to_owned());
                }
                (self.ocr_state == OcrState::NotRun)
                    .then_some(())
                    .ok_or_else(|| "protected content must use ocr_state=not-run".to_owned())
            }
            ObservationContent::NoEvidence(_) => (self.ocr_state == OcrState::NotRun)
                .then_some(())
                .ok_or_else(|| "non-captured content must use ocr_state=not-run".to_owned()),
        }
    }
}

fn validate_window_context(context: &PermittedWindowContext) -> Result<(), String> {
    if context.application_bundle_id.is_empty() || context.process_name.is_empty() {
        return Err("captured window identity fields must be non-empty".to_owned());
    }
    if context
        .window_title
        .as_ref()
        .is_some_and(|title| title.is_empty())
    {
        return Err("present window titles must be non-empty".to_owned());
    }
    if let Some(domain) = &context.authorized_domain
        && (domain.adapter.is_empty() || domain.domain.is_empty())
    {
        return Err("authorized domain context must name its adapter and domain".to_owned());
    }
    Ok(())
}

fn validate_content_hash(content_hash: &str) -> Result<(), String> {
    (!content_hash.is_empty())
        .then_some(())
        .ok_or_else(|| "captured content hash must be non-empty".to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GapReason {
    Sleep,
    Quit,
    StorageOutage,
    PermissionLoss,
    ClockCorrection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingGap {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub reason: GapReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotLifecycleAction {
    WriteCompleted,
    DeleteRequested,
    DeleteCompleted,
    Missing,
    WriteFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotDeletionCause {
    RetentionExpired,
    UserRequested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotProjectedState {
    Retained,
    DeletePending,
    Expired,
    UserDeleted,
    Missing,
    WriteFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotLifecycle {
    pub artifact_id: ImageArtifactId,
    pub action: ScreenshotLifecycleAction,
    pub deletion_cause: Option<ScreenshotDeletionCause>,
    pub projected_state: ScreenshotProjectedState,
    pub requested_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub source_event_id: EventId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum EventPayload {
    ObservationAttempt(Box<ObservationAttempt>),
    RecordingGap(RecordingGap),
    ScreenshotLifecycle(ScreenshotLifecycle),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: String,
    pub event_id: EventId,
    pub device_id: DeviceId,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub display_timezone: String,
    pub source: EvidenceSource,
    pub kind: EventKind,
    pub payload: EventPayload,
}

impl EventEnvelope {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        reject_sensitive_skip_extensions(json)?;
        let event: Self = parse_versioned(json)?;
        event.validate().map_err(ContractError::Validation)?;
        Ok(event)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.recorded_at < self.observed_at {
            return Err("recorded_at must not precede observed_at".to_owned());
        }
        let kind_matches = matches!(
            (&self.kind, &self.payload),
            (
                EventKind::ObservationAttempt,
                EventPayload::ObservationAttempt(_)
            ) | (EventKind::RecordingGap, EventPayload::RecordingGap(_))
                | (
                    EventKind::ScreenshotLifecycle,
                    EventPayload::ScreenshotLifecycle(_)
                )
        );
        if !kind_matches {
            return Err("event kind and payload type disagree".to_owned());
        }
        if let EventPayload::ObservationAttempt(attempt) = &self.payload {
            let scheduled_at = self
                .scheduled_at
                .ok_or_else(|| "observation attempts require scheduled_at".to_owned())?;
            if self.observed_at < scheduled_at {
                return Err("observed_at must not precede scheduled_at".to_owned());
            }
            attempt.validate()?;
            if let ObservationContent::Captured(content) = &attempt.content
                && content
                    .image
                    .as_ref()
                    .is_some_and(|image| image.expires_at <= self.recorded_at)
            {
                return Err("image expiry must follow event recording".to_owned());
            }
        } else if self.scheduled_at.is_some() {
            return Err("gap and lifecycle records cannot be scheduled attempts".to_owned());
        }
        if let EventPayload::RecordingGap(gap) = &self.payload
            && gap.start >= gap.end
        {
            return Err("gap start must precede end".to_owned());
        }
        if let EventPayload::ScreenshotLifecycle(lifecycle) = &self.payload {
            let state_matches = matches!(
                (
                    lifecycle.action,
                    lifecycle.deletion_cause,
                    lifecycle.projected_state
                ),
                (
                    ScreenshotLifecycleAction::WriteCompleted,
                    None,
                    ScreenshotProjectedState::Retained
                ) | (
                    ScreenshotLifecycleAction::DeleteRequested,
                    Some(
                        ScreenshotDeletionCause::RetentionExpired
                            | ScreenshotDeletionCause::UserRequested
                    ),
                    ScreenshotProjectedState::DeletePending,
                ) | (
                    ScreenshotLifecycleAction::DeleteCompleted,
                    Some(ScreenshotDeletionCause::UserRequested),
                    ScreenshotProjectedState::UserDeleted,
                ) | (
                    ScreenshotLifecycleAction::DeleteCompleted,
                    Some(ScreenshotDeletionCause::RetentionExpired),
                    ScreenshotProjectedState::Expired
                ) | (
                    ScreenshotLifecycleAction::Missing,
                    None,
                    ScreenshotProjectedState::Missing
                ) | (
                    ScreenshotLifecycleAction::WriteFailed,
                    None,
                    ScreenshotProjectedState::WriteFailed
                )
            );
            if !state_matches {
                return Err("screenshot lifecycle action and state disagree".to_owned());
            }
            match lifecycle.action {
                ScreenshotLifecycleAction::DeleteRequested => {
                    if lifecycle.requested_at.is_none() || lifecycle.completed_at.is_some() {
                        return Err(
                            "delete-requested requires requested_at without completed_at"
                                .to_owned(),
                        );
                    }
                }
                ScreenshotLifecycleAction::DeleteCompleted => {
                    if lifecycle.requested_at.is_none() || lifecycle.completed_at.is_none() {
                        return Err(
                            "delete-completed requires requested_at and completed_at".to_owned()
                        );
                    }
                }
                ScreenshotLifecycleAction::WriteCompleted
                | ScreenshotLifecycleAction::Missing
                | ScreenshotLifecycleAction::WriteFailed
                    if lifecycle.requested_at.is_some() =>
                {
                    return Err(
                        "non-deletion lifecycle action cannot carry requested_at".to_owned()
                    );
                }
                _ if lifecycle.completed_at.is_none() => {
                    return Err(
                        "terminal screenshot lifecycle action requires completed_at".to_owned()
                    );
                }
                _ => {}
            }
            if let (Some(requested_at), Some(completed_at)) =
                (lifecycle.requested_at, lifecycle.completed_at)
                && completed_at < requested_at
            {
                return Err("delete completion cannot precede its request".to_owned());
            }
            if lifecycle.action == ScreenshotLifecycleAction::DeleteRequested
                && lifecycle.requested_at.is_some_and(|requested_at| {
                    requested_at < self.observed_at || requested_at > self.recorded_at
                })
            {
                return Err(
                    "delete request time must be within its event observation interval".to_owned(),
                );
            }
            if lifecycle.completed_at.is_some_and(|completed_at| {
                completed_at < self.observed_at || completed_at > self.recorded_at
            }) {
                return Err(
                    "lifecycle completion time must be within its event observation interval"
                        .to_owned(),
                );
            }
        }
        if self.display_timezone.is_empty()
            || self.source.adapter.is_empty()
            || self.source.version.is_empty()
        {
            return Err("event timezone and source fields must be non-empty".to_owned());
        }
        Ok(())
    }
}

fn reject_sensitive_skip_extensions(json: &str) -> Result<(), ContractError> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| ContractError::InvalidJson(error.to_string()))?;
    let content_type = value
        .pointer("/payload/data/content/type")
        .and_then(Value::as_str);
    if !matches!(content_type, Some("protected" | "no-evidence")) {
        return Ok(());
    }
    const FORBIDDEN: &[&str] = &[
        "application_bundle_id",
        "process_name",
        "window_title",
        "authorized_domain",
        "title",
        "ocr",
        "image",
        "screenshot",
        "image_bytes",
        "factual_detail",
    ];
    if contains_any_key(&value, FORBIDDEN) {
        return Err(ContractError::Validation(
            "protected/no-evidence payload contains a sensitive or freeform field".to_owned(),
        ));
    }
    Ok(())
}

fn contains_any_key(value: &Value, keys: &[&str]) -> bool {
    match value {
        Value::Object(object) => {
            object.keys().any(|key| keys.contains(&key.as_str()))
                || object.values().any(|child| contains_any_key(child, keys))
        }
        Value::Array(array) => array.iter().any(|child| contains_any_key(child, keys)),
        _ => false,
    }
}
