use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{
    ArtifactId, ArtifactRevisionId, ArtifactStatus, ArtifactType, AttemptStatus, AuthorIdentity,
    ChunkGap, ChunkGapKind, ChunkId, ChunkRevision, ChunkRevisionId, ClientId, ContentClass,
    ContractError, DerivedArtifactRevision, DeviceId, DisclosureLimits, EventId, EventKind,
    EvidenceReferences, EvidenceSeconds, EvidenceSource, EvidenceState, GrantId, GrantState,
    GrantTimeScope, ImageArtifactId, NoEvidenceContent, NoEvidenceReason, OcrEvidence, OcrState,
    PermittedWindowContext, PresenceSeconds, PresenceState, ProtectedContent, ReceiptId,
    RecordingGap, RequestId, ScreenshotDeletionCause, ScreenshotLifecycle,
    ScreenshotLifecycleAction, ScreenshotProjectedState, UtcRange, parse_versioned,
};
use serde_json::Value;

const QUERY_FORBIDDEN_KEYS: &[&str] = &[
    "managed_relative_path",
    "path",
    "bytes",
    "image_bytes",
    "screenshot_bytes",
    "encoded_image",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityFilter {
    pub range: UtcRange,
    pub application_bundle_id: Option<String>,
    pub window_text: Option<String>,
    pub authorized_domain: Option<String>,
    pub evidence_states: Vec<EvidenceState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueryOperationKind {
    Status,
    Schemas,
    ListChunks,
    ReadChunk,
    GetEvent,
    GetArtifact,
    SearchActivity,
    InspectMoment,
    Statistics,
    ComparePeriods,
    SupportingEvidence,
    BuildContextPacket,
    ListDerived,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum QueryOperation {
    Status,
    Schemas,
    ListChunks {
        filter: ActivityFilter,
        page: PageRequest,
    },
    ReadChunk {
        chunk_id: ChunkId,
    },
    GetEvent {
        event_id: EventId,
    },
    GetArtifact {
        artifact_id: ArtifactId,
        revision_id: Option<ArtifactRevisionId>,
    },
    SearchActivity {
        filter: ActivityFilter,
        query: String,
        include_ocr: bool,
        page: PageRequest,
    },
    InspectMoment {
        at: DateTime<Utc>,
    },
    Statistics {
        filter: ActivityFilter,
    },
    ComparePeriods {
        first: UtcRange,
        second: UtcRange,
    },
    SupportingEvidence {
        chunk_id: ChunkId,
        page: PageRequest,
    },
    BuildContextPacket {
        filter: ActivityFilter,
        include_ocr: bool,
        max_bytes: u64,
    },
    ListDerived {
        range: UtcRange,
        page: PageRequest,
    },
}

impl QueryOperation {
    pub const fn kind(&self) -> QueryOperationKind {
        match self {
            Self::Status => QueryOperationKind::Status,
            Self::Schemas => QueryOperationKind::Schemas,
            Self::ListChunks { .. } => QueryOperationKind::ListChunks,
            Self::ReadChunk { .. } => QueryOperationKind::ReadChunk,
            Self::GetEvent { .. } => QueryOperationKind::GetEvent,
            Self::GetArtifact { .. } => QueryOperationKind::GetArtifact,
            Self::SearchActivity { .. } => QueryOperationKind::SearchActivity,
            Self::InspectMoment { .. } => QueryOperationKind::InspectMoment,
            Self::Statistics { .. } => QueryOperationKind::Statistics,
            Self::ComparePeriods { .. } => QueryOperationKind::ComparePeriods,
            Self::SupportingEvidence { .. } => QueryOperationKind::SupportingEvidence,
            Self::BuildContextPacket { .. } => QueryOperationKind::BuildContextPacket,
            Self::ListDerived { .. } => QueryOperationKind::ListDerived,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRequest {
    pub schema_version: String,
    pub request_id: RequestId,
    pub client_id: ClientId,
    pub grant_id: GrantId,
    pub store_generation: u64,
    pub operation: QueryOperation,
}

impl QueryRequest {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        let request: Self = parse_versioned(json)?;
        request.validate().map_err(ContractError::Validation)?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.store_generation == 0 {
            return Err("query request requires a nonzero store generation".to_owned());
        }
        match &self.operation {
            QueryOperation::ListChunks { filter, page }
            | QueryOperation::SearchActivity { filter, page, .. } => {
                filter.range.validate()?;
                validate_page(page)
            }
            QueryOperation::Statistics { filter } => filter.range.validate(),
            QueryOperation::BuildContextPacket {
                filter, max_bytes, ..
            } => {
                filter.range.validate()?;
                (*max_bytes > 0)
                    .then_some(())
                    .ok_or_else(|| "context packet max_bytes must be greater than zero".to_owned())
            }
            QueryOperation::ComparePeriods { first, second } => {
                first.validate()?;
                second.validate()
            }
            QueryOperation::SupportingEvidence { page, .. } => validate_page(page),
            QueryOperation::ListDerived { range, page } => {
                range.validate()?;
                validate_page(page)
            }
            QueryOperation::Status
            | QueryOperation::Schemas
            | QueryOperation::ReadChunk { .. }
            | QueryOperation::GetEvent { .. }
            | QueryOperation::GetArtifact { .. }
            | QueryOperation::InspectMoment { .. } => Ok(()),
        }
    }
}

fn validate_page(page: &PageRequest) -> Result<(), String> {
    (page.limit > 0)
        .then_some(())
        .ok_or_else(|| "page limit must be greater than zero".to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueryCapability {
    Metadata,
    Ocr,
    DerivedRead,
    DerivedWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSummary {
    pub grant_id: GrantId,
    pub client_id: ClientId,
    pub receipt_id: ReceiptId,
    pub state: GrantState,
    pub time_scope: GrantTimeScope,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub content_classes: Vec<ContentClass>,
    pub capabilities: Vec<QueryCapability>,
    pub limits: DisclosureLimits,
    pub remaining_cumulative_bytes: u64,
    pub disclosed_bytes: u64,
    pub store_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub returned_items: u32,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageMetadata {
    pub artifact_id: ImageArtifactId,
    pub state: ScreenshotProjectedState,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryCoverage {
    pub range: UtcRange,
    pub evidence_seconds: EvidenceSeconds,
    pub presence_seconds: PresenceSeconds,
    pub gaps: Vec<ChunkGap>,
}

impl QueryCoverage {
    pub fn validate(&self) -> Result<(), String> {
        self.range.validate()?;
        let duration = (self.range.end - self.range.start).num_seconds();
        if duration < 0
            || u64::try_from(duration).ok() != Some(u64::from(self.evidence_seconds.total()))
        {
            return Err("query evidence coverage must partition its UTC range".to_owned());
        }
        if self.presence_seconds.total() != self.evidence_seconds.captured {
            return Err("query presence must partition captured coverage".to_owned());
        }
        let mut prior_end = None;
        let mut gap_seconds = std::collections::HashMap::<ChunkGapKind, u32>::new();
        for gap in &self.gaps {
            if gap.start < self.range.start || gap.end > self.range.end || gap.start >= gap.end {
                return Err("query gaps must be positive intervals inside coverage".to_owned());
            }
            if prior_end.is_some_and(|end| gap.start < end) {
                return Err("query gaps must be ordered and non-overlapping".to_owned());
            }
            let duration = gap.end - gap.start;
            let seconds = duration.num_seconds();
            if seconds <= 0 || chrono::Duration::seconds(seconds) != duration {
                return Err("query gaps must use whole-second durations".to_owned());
            }
            let seconds = u32::try_from(seconds)
                .map_err(|_| "query gap duration exceeds v1 bounds".to_owned())?;
            let total = gap_seconds.entry(gap.kind).or_default();
            *total = total
                .checked_add(seconds)
                .ok_or_else(|| "query gap duration overflow".to_owned())?;
            prior_end = Some(gap.end);
        }
        for kind in [
            ChunkGapKind::Protected,
            ChunkGapKind::Paused,
            ChunkGapKind::Unavailable,
            ChunkGapKind::Error,
            ChunkGapKind::MissingObservation,
        ] {
            if gap_seconds.get(&kind).copied().unwrap_or_default()
                != self.evidence_seconds.for_gap_kind(kind)
            {
                return Err(
                    "query gap intervals must exactly reconcile non-captured evidence seconds"
                        .to_owned(),
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryStatus {
    pub recording_available: bool,
    pub projection_current: bool,
    pub latest_recorded_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaDescriptor {
    pub name: String,
    pub major_version: u16,
    pub schema_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryScope {
    pub requested_ranges: Vec<UtcRange>,
    pub effective_ranges: Vec<UtcRange>,
    pub content_classes: Vec<ContentClass>,
    pub ocr_included: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryProvenance {
    pub query_engine_version: String,
    pub schema_build_id: String,
    pub projection_build_id: String,
    pub sqlite_version: String,
    pub sqlite_source_id: String,
    pub source_event_ids: Vec<EventId>,
    pub source_chunk_revision_ids: Vec<ChunkRevisionId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum QueryObservationContent {
    Captured {
        context: PermittedWindowContext,
        content_hash: String,
        ocr: Option<OcrEvidence>,
        image: Option<ImageMetadata>,
    },
    Unchanged {
        context: PermittedWindowContext,
        content_hash: String,
        previous_event_id: EventId,
        reused_ocr_event_id: Option<EventId>,
        image: Option<ImageMetadata>,
    },
    Protected(ProtectedContent),
    NoEvidence(NoEvidenceContent),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryObservation {
    pub cadence_seconds: u32,
    pub attempt_status: AttemptStatus,
    pub evidence_state: EvidenceState,
    pub presence_state: PresenceState,
    pub idle_seconds: Option<u32>,
    pub ocr_state: OcrState,
    pub content: QueryObservationContent,
}

impl QueryObservation {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.cadence_seconds, 30 | 60) {
            return Err("query observation cadence must be 30 or 60 seconds".to_owned());
        }
        match (self.presence_state, self.idle_seconds) {
            (PresenceState::Idle, Some(seconds)) if seconds > 0 => {}
            (PresenceState::Idle, _) => {
                return Err("query idle presence requires positive idle seconds".to_owned());
            }
            (_, Some(_)) => {
                return Err("query idle seconds require idle presence".to_owned());
            }
            (_, None) => {}
        }
        let axes_match = match (self.attempt_status, self.evidence_state, &self.content) {
            (
                AttemptStatus::Completed,
                EvidenceState::CapturedNew,
                QueryObservationContent::Captured { .. },
            )
            | (
                AttemptStatus::Completed,
                EvidenceState::CapturedUnchanged,
                QueryObservationContent::Unchanged { .. },
            )
            | (
                AttemptStatus::Skipped,
                EvidenceState::Protected,
                QueryObservationContent::Protected(_),
            ) => true,
            (
                AttemptStatus::Skipped,
                EvidenceState::Paused,
                QueryObservationContent::NoEvidence(content),
            ) => matches!(
                content.reason,
                NoEvidenceReason::UserPaused | NoEvidenceReason::StudyExpired
            ),
            (
                AttemptStatus::Skipped,
                EvidenceState::Unavailable,
                QueryObservationContent::NoEvidence(content),
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
                QueryObservationContent::NoEvidence(content),
            ) => content.reason == NoEvidenceReason::CaptureApiFailure,
            _ => false,
        };
        if !axes_match {
            return Err("query observation axes and content disagree".to_owned());
        }
        let presence_matches = matches!(
            (&self.content, self.presence_state),
            (
                QueryObservationContent::Captured { .. }
                    | QueryObservationContent::Unchanged { .. },
                PresenceState::Active | PresenceState::Idle | PresenceState::Unknown,
            ) | (
                QueryObservationContent::NoEvidence(NoEvidenceContent {
                    reason: NoEvidenceReason::Locked,
                }),
                PresenceState::Locked,
            ) | (
                QueryObservationContent::NoEvidence(NoEvidenceContent {
                    reason: NoEvidenceReason::Asleep,
                }),
                PresenceState::Asleep,
            ) | (
                QueryObservationContent::Protected(_)
                    | QueryObservationContent::NoEvidence(NoEvidenceContent {
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
            return Err("query observation presence and outcome disagree".to_owned());
        }
        match &self.content {
            QueryObservationContent::Captured {
                context,
                content_hash,
                ocr,
                ..
            } => {
                validate_query_context(context)?;
                if content_hash.is_empty() {
                    return Err("query captured content requires a hash".to_owned());
                }
                let has_ocr = ocr.is_some();
                let ocr_matches = match self.ocr_state {
                    OcrState::Complete | OcrState::Empty | OcrState::Partial => has_ocr,
                    OcrState::Failed => !has_ocr,
                    OcrState::NotRun => false,
                };
                if !ocr_matches {
                    return Err("query OCR state and payload disagree".to_owned());
                }
                if let Some(ocr) = ocr {
                    if ocr
                        .confidence
                        .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
                    {
                        return Err("query OCR confidence is outside zero to one".to_owned());
                    }
                    let text_matches = match self.ocr_state {
                        OcrState::Empty => ocr.text.is_empty(),
                        OcrState::Complete | OcrState::Partial => !ocr.text.is_empty(),
                        OcrState::Failed | OcrState::NotRun => false,
                    };
                    if !text_matches {
                        return Err("query OCR state and text disagree".to_owned());
                    }
                }
            }
            QueryObservationContent::Unchanged {
                context,
                content_hash,
                ..
            } => {
                validate_query_context(context)?;
                if content_hash.is_empty() || self.ocr_state != OcrState::NotRun {
                    return Err(
                        "query unchanged content requires a hash and not-run OCR".to_owned()
                    );
                }
            }
            QueryObservationContent::Protected(content) => {
                if content.privacy_policy_version.is_empty() || self.ocr_state != OcrState::NotRun {
                    return Err(
                        "query protected content requires policy version and no OCR".to_owned()
                    );
                }
            }
            QueryObservationContent::NoEvidence(_) => {
                if self.ocr_state != OcrState::NotRun {
                    return Err("query no-evidence content cannot carry OCR".to_owned());
                }
            }
        }
        Ok(())
    }
}

fn validate_query_context(context: &PermittedWindowContext) -> Result<(), String> {
    if context.application_bundle_id.is_empty()
        || context.process_name.is_empty()
        || context
            .window_title
            .as_ref()
            .is_some_and(|title| title.is_empty())
        || context
            .authorized_domain
            .as_ref()
            .is_some_and(|domain| domain.adapter.is_empty() || domain.domain.is_empty())
    {
        return Err("query window context contains an empty identity field".to_owned());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum QueryEventPayload {
    ObservationAttempt(Box<QueryObservation>),
    RecordingGap(RecordingGap),
    ScreenshotLifecycle(ScreenshotLifecycle),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryEvent {
    pub event_id: EventId,
    pub device_id: DeviceId,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub display_timezone: String,
    pub source: EvidenceSource,
    pub kind: EventKind,
    pub payload: QueryEventPayload,
}

impl QueryEvent {
    fn validate(&self) -> Result<(), String> {
        if self.recorded_at < self.observed_at
            || self.display_timezone.is_empty()
            || self.source.adapter.is_empty()
            || self.source.version.is_empty()
        {
            return Err("query event timestamps or source are invalid".to_owned());
        }
        match (&self.kind, &self.payload) {
            (EventKind::ObservationAttempt, QueryEventPayload::ObservationAttempt(attempt))
                if self.scheduled_at.is_some() =>
            {
                attempt.validate()
            }
            (EventKind::RecordingGap, QueryEventPayload::RecordingGap(gap))
                if self.scheduled_at.is_none() && gap.start < gap.end =>
            {
                Ok(())
            }
            (EventKind::ScreenshotLifecycle, QueryEventPayload::ScreenshotLifecycle(lifecycle))
                if self.scheduled_at.is_none() =>
            {
                validate_query_lifecycle(lifecycle)
            }
            _ => Err("query event kind, schedule, and payload disagree".to_owned()),
        }
    }
}

fn validate_query_lifecycle(lifecycle: &ScreenshotLifecycle) -> Result<(), String> {
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
                ScreenshotDeletionCause::RetentionExpired | ScreenshotDeletionCause::UserRequested
            ),
            ScreenshotProjectedState::DeletePending
        ) | (
            ScreenshotLifecycleAction::DeleteCompleted,
            Some(ScreenshotDeletionCause::RetentionExpired),
            ScreenshotProjectedState::Expired
        ) | (
            ScreenshotLifecycleAction::DeleteCompleted,
            Some(ScreenshotDeletionCause::UserRequested),
            ScreenshotProjectedState::UserDeleted
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
        return Err("query lifecycle action, cause, and state disagree".to_owned());
    }
    match lifecycle.action {
        ScreenshotLifecycleAction::DeleteRequested
            if lifecycle.requested_at.is_some() && lifecycle.completed_at.is_none() =>
        {
            Ok(())
        }
        ScreenshotLifecycleAction::DeleteCompleted
            if lifecycle.requested_at.is_some()
                && lifecycle.completed_at.is_some()
                && lifecycle.completed_at >= lifecycle.requested_at =>
        {
            Ok(())
        }
        ScreenshotLifecycleAction::WriteCompleted
        | ScreenshotLifecycleAction::Missing
        | ScreenshotLifecycleAction::WriteFailed
            if lifecycle.requested_at.is_none() && lifecycle.completed_at.is_some() =>
        {
            Ok(())
        }
        _ => Err("query lifecycle timestamps disagree with the action".to_owned()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSummary {
    pub chunk_id: ChunkId,
    pub revision_id: ChunkRevisionId,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub evidence_seconds: EvidenceSeconds,
    pub presence_seconds: PresenceSeconds,
    pub late_input: bool,
}

impl ChunkSummary {
    fn validate(&self) -> Result<(), String> {
        if self.start >= self.end
            || self.evidence_seconds.total()
                != u32::try_from((self.end - self.start).num_seconds()).unwrap_or(u32::MAX)
            || self.presence_seconds.total() != self.evidence_seconds.captured
        {
            return Err("chunk summary coverage does not match its interval".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryArtifact {
    pub artifact_id: ArtifactId,
    pub revision_id: ArtifactRevisionId,
    pub prior_revision_id: Option<ArtifactRevisionId>,
    pub artifact_type: ArtifactType,
    pub author: AuthorIdentity,
    pub created_at: DateTime<Utc>,
    pub status: ArtifactStatus,
    pub payload: serde_json::Value,
    pub evidence: EvidenceReferences,
    pub confidence: Option<f32>,
    pub store_generation: u64,
}

impl QueryArtifact {
    fn validate(&self) -> Result<(), String> {
        if self.store_generation == 0
            || (self.evidence.event_ids.is_empty() && self.evidence.chunk_ids.is_empty())
            || contains_forbidden_key(&self.payload, QUERY_FORBIDDEN_KEYS)
            || self
                .confidence
                .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
            || self
                .prior_revision_id
                .as_ref()
                .is_some_and(|prior| prior == &self.revision_id)
        {
            return Err("query artifact revision is not a valid immutable revision".to_owned());
        }
        Ok(())
    }
}

impl From<DerivedArtifactRevision> for QueryArtifact {
    fn from(value: DerivedArtifactRevision) -> Self {
        Self {
            artifact_id: value.artifact_id,
            revision_id: value.revision_id,
            prior_revision_id: value.prior_revision_id,
            artifact_type: value.artifact_type,
            author: value.author,
            created_at: value.created_at,
            status: value.status,
            payload: value.payload,
            evidence: value.evidence,
            confidence: value.confidence,
            store_generation: value.store_generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum QueryResult {
    Status(QueryStatus),
    Schemas {
        schemas: Vec<SchemaDescriptor>,
    },
    ChunkList {
        chunks: Vec<ChunkSummary>,
    },
    Chunk {
        chunk: Box<ChunkRevision>,
        images: Vec<ImageMetadata>,
    },
    Event {
        event: Box<QueryEvent>,
    },
    Artifact {
        artifact: Box<QueryArtifact>,
    },
    Search {
        events: Vec<QueryEvent>,
    },
    Moment {
        events: Vec<QueryEvent>,
    },
    Statistics {
        factual_totals: Vec<FactualTotal>,
    },
    Comparison {
        first: QueryCoverage,
        second: QueryCoverage,
    },
    SupportingEvidence {
        events: Vec<QueryEvent>,
    },
    ContextPacket {
        chunks: Vec<ChunkRevision>,
        events: Vec<QueryEvent>,
    },
    DerivedList {
        artifacts: Vec<QueryArtifact>,
    },
}

impl QueryResult {
    pub const fn operation_kind(&self) -> QueryOperationKind {
        match self {
            Self::Status(_) => QueryOperationKind::Status,
            Self::Schemas { .. } => QueryOperationKind::Schemas,
            Self::ChunkList { .. } => QueryOperationKind::ListChunks,
            Self::Chunk { .. } => QueryOperationKind::ReadChunk,
            Self::Event { .. } => QueryOperationKind::GetEvent,
            Self::Artifact { .. } => QueryOperationKind::GetArtifact,
            Self::Search { .. } => QueryOperationKind::SearchActivity,
            Self::Moment { .. } => QueryOperationKind::InspectMoment,
            Self::Statistics { .. } => QueryOperationKind::Statistics,
            Self::Comparison { .. } => QueryOperationKind::ComparePeriods,
            Self::SupportingEvidence { .. } => QueryOperationKind::SupportingEvidence,
            Self::ContextPacket { .. } => QueryOperationKind::BuildContextPacket,
            Self::DerivedList { .. } => QueryOperationKind::ListDerived,
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Status(_) => Ok(()),
            Self::Schemas { schemas } => {
                if schemas.iter().any(|schema| {
                    schema.name.is_empty()
                        || schema.major_version == 0
                        || schema.schema_id.is_empty()
                }) {
                    return Err("query schema descriptors must be explicit".to_owned());
                }
                Ok(())
            }
            Self::ChunkList { chunks } => {
                for chunk in chunks {
                    chunk.validate()?;
                }
                Ok(())
            }
            Self::Chunk { chunk, .. } => validate_query_chunk(chunk),
            Self::Event { event } => event.validate(),
            Self::Artifact { artifact } => artifact.validate(),
            Self::Search { events }
            | Self::Moment { events }
            | Self::SupportingEvidence { events } => {
                for event in events {
                    event.validate()?;
                }
                Ok(())
            }
            Self::Statistics { factual_totals } => {
                if factual_totals.iter().any(|total| total.key.is_empty()) {
                    return Err("factual totals require non-empty dimension keys".to_owned());
                }
                Ok(())
            }
            Self::Comparison { first, second } => {
                first.validate()?;
                second.validate()
            }
            Self::ContextPacket { chunks, events } => {
                for chunk in chunks {
                    validate_query_chunk(chunk)?;
                }
                for event in events {
                    event.validate()?;
                }
                Ok(())
            }
            Self::DerivedList { artifacts } => {
                for artifact in artifacts {
                    artifact.validate()?;
                }
                Ok(())
            }
        }
    }

    fn contains_ocr(&self) -> bool {
        match self {
            Self::Chunk { chunk, .. } => !chunk.ocr_extracts.is_empty(),
            Self::Event { event } => query_event_has_ocr(event),
            Self::Search { events }
            | Self::Moment { events }
            | Self::SupportingEvidence { events } => events.iter().any(query_event_has_ocr),
            Self::ContextPacket { chunks, events } => {
                chunks.iter().any(|chunk| !chunk.ocr_extracts.is_empty())
                    || events.iter().any(query_event_has_ocr)
            }
            Self::Status(_)
            | Self::Schemas { .. }
            | Self::ChunkList { .. }
            | Self::Artifact { .. }
            | Self::Statistics { .. }
            | Self::Comparison { .. }
            | Self::DerivedList { .. } => false,
        }
    }

    fn required_content_classes(&self) -> HashSet<ContentClass> {
        let mut required = HashSet::new();
        match self {
            Self::Status(_) | Self::Schemas { .. } => {}
            Self::Artifact { .. } | Self::DerivedList { .. } => {
                required.insert(ContentClass::Derived);
            }
            Self::ChunkList { .. }
            | Self::Chunk { .. }
            | Self::Event { .. }
            | Self::Search { .. }
            | Self::Moment { .. }
            | Self::Statistics { .. }
            | Self::Comparison { .. }
            | Self::SupportingEvidence { .. }
            | Self::ContextPacket { .. } => {
                required.insert(ContentClass::Metadata);
            }
        }
        if self.contains_ocr() {
            required.insert(ContentClass::Ocr);
        }
        required
    }

    fn top_level_item_count(&self) -> Option<usize> {
        match self {
            Self::ChunkList { chunks } => Some(chunks.len()),
            Self::Search { events } | Self::SupportingEvidence { events } => Some(events.len()),
            Self::DerivedList { artifacts } => Some(artifacts.len()),
            _ => None,
        }
    }

    fn validate_effective_ranges(&self, ranges: &[UtcRange]) -> Result<(), String> {
        let validate_event = |event: &QueryEvent| {
            if !instant_in_ranges(event.observed_at, ranges)
                || event
                    .scheduled_at
                    .is_some_and(|scheduled| !instant_in_ranges(scheduled, ranges))
            {
                return Err("returned event falls outside the effective query scope".to_owned());
            }
            if let QueryEventPayload::RecordingGap(gap) = &event.payload
                && !range_in_ranges(gap.start, gap.end, ranges)
            {
                return Err("returned event interval falls outside effective scope".to_owned());
            }
            Ok(())
        };
        let validate_artifact = |artifact: &QueryArtifact| {
            instant_in_ranges(artifact.created_at, ranges)
                .then_some(())
                .ok_or_else(|| "returned artifact falls outside effective query scope".to_owned())
        };
        match self {
            Self::Status(_) | Self::Schemas { .. } | Self::Statistics { .. } => Ok(()),
            Self::ChunkList { chunks } => {
                for chunk in chunks {
                    if !range_in_ranges(chunk.start, chunk.end, ranges) {
                        return Err(
                            "returned chunk summary falls outside effective scope".to_owned()
                        );
                    }
                }
                Ok(())
            }
            Self::Chunk { chunk, .. } => {
                range_in_ranges(chunk.window.start, chunk.window.end, ranges)
                    .then_some(())
                    .ok_or_else(|| "returned chunk falls outside effective scope".to_owned())
            }
            Self::Event { event } => validate_event(event),
            Self::Artifact { artifact } => validate_artifact(artifact),
            Self::Search { events }
            | Self::Moment { events }
            | Self::SupportingEvidence { events } => {
                for event in events {
                    validate_event(event)?;
                }
                Ok(())
            }
            Self::Comparison { first, second } => {
                if range_in_ranges(first.range.start, first.range.end, ranges)
                    && range_in_ranges(second.range.start, second.range.end, ranges)
                {
                    Ok(())
                } else {
                    Err("comparison coverage falls outside effective scope".to_owned())
                }
            }
            Self::ContextPacket { chunks, events } => {
                for chunk in chunks {
                    if !range_in_ranges(chunk.window.start, chunk.window.end, ranges) {
                        return Err("context chunk falls outside effective scope".to_owned());
                    }
                }
                for event in events {
                    validate_event(event)?;
                }
                Ok(())
            }
            Self::DerivedList { artifacts } => {
                for artifact in artifacts {
                    validate_artifact(artifact)?;
                }
                Ok(())
            }
        }
    }
}

fn query_event_has_ocr(event: &QueryEvent) -> bool {
    matches!(
        &event.payload,
        QueryEventPayload::ObservationAttempt(attempt)
            if matches!(
                &attempt.content,
                QueryObservationContent::Captured { ocr: Some(_), .. }
            )
    )
}

fn instant_in_ranges(at: DateTime<Utc>, ranges: &[UtcRange]) -> bool {
    ranges
        .iter()
        .any(|range| range.start <= at && at < range.end)
}

fn range_in_ranges(start: DateTime<Utc>, end: DateTime<Utc>, ranges: &[UtcRange]) -> bool {
    start < end
        && ranges
            .iter()
            .any(|range| range.start <= start && end <= range.end)
}

fn validate_query_chunk(chunk: &ChunkRevision) -> Result<(), String> {
    let mut version_parts = chunk.schema_version.split('.');
    let supported_version = version_parts.next() == Some("1")
        && version_parts
            .next()
            .is_some_and(|minor| minor.parse::<u16>().is_ok())
        && version_parts.next().is_none();
    if !supported_version {
        return Err("query chunk uses an unsupported schema version".to_owned());
    }
    chunk.validate()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactualTotal {
    pub dimension: crate::DimensionKind,
    pub key: String,
    pub estimated_seconds: u32,
    pub supporting_chunk_ids: Vec<ChunkId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryResponse {
    pub schema_version: String,
    pub request_id: RequestId,
    pub operation: QueryOperationKind,
    pub generated_at: DateTime<Utc>,
    pub store_generation: u64,
    pub grant: GrantSummary,
    pub scope: QueryScope,
    pub page: Option<PageInfo>,
    pub stable_cutoff: DateTime<Utc>,
    pub coverage: Option<QueryCoverage>,
    pub provenance: QueryProvenance,
    pub result: QueryResult,
}

impl QueryResponse {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        reject_query_path_and_bytes(json)?;
        let response: Self = parse_versioned(json)?;
        response.validate().map_err(ContractError::Validation)?;
        Ok(response)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.store_generation == 0
            || self.grant.store_generation != self.store_generation
            || self.grant.remaining_cumulative_bytes > self.grant.limits.max_cumulative_bytes
            || self.grant.disclosed_bytes > self.grant.limits.max_cumulative_bytes
            || self.grant.remaining_cumulative_bytes
                > self
                    .grant
                    .limits
                    .max_cumulative_bytes
                    .saturating_sub(self.grant.disclosed_bytes)
        {
            return Err("response and grant generations or limits disagree".to_owned());
        }
        if self.operation != self.result.operation_kind() {
            return Err("query operation and result type disagree".to_owned());
        }
        if self.grant.state != GrantState::Active
            || self.generated_at < self.grant.created_at
            || self.generated_at >= self.grant.expires_at
            || self.stable_cutoff > self.generated_at
            || self.grant.limits.max_page_items == 0
            || self.grant.limits.max_response_bytes == 0
            || self.grant.limits.max_cumulative_bytes < self.grant.limits.max_response_bytes
        {
            return Err("responses require a grant active at generation time".to_owned());
        }
        validate_unique(&self.grant.content_classes, "grant content classes")?;
        validate_unique(&self.grant.capabilities, "grant capabilities")?;
        match &self.grant.time_scope {
            GrantTimeScope::Absolute { range } => range.validate()?,
            GrantTimeScope::RollingHorizon { seconds } if *seconds > 0 => {}
            GrantTimeScope::RollingHorizon { .. } => {
                return Err("grant rolling horizon must be positive".to_owned());
            }
        }
        if self.scope.ocr_included
            && (!self.scope.content_classes.contains(&ContentClass::Ocr)
                || !self.grant.capabilities.contains(&QueryCapability::Ocr))
        {
            return Err("OCR output requires both scope and grant capability".to_owned());
        }
        for range in self
            .scope
            .requested_ranges
            .iter()
            .chain(&self.scope.effective_ranges)
        {
            range.validate()?;
        }
        validate_unique(&self.scope.content_classes, "scope content classes")?;
        if self.scope.content_classes.is_empty()
            || self
                .scope
                .content_classes
                .iter()
                .any(|class| !self.grant.content_classes.contains(class))
            || self.scope.content_classes.iter().any(|class| {
                let capability = match class {
                    ContentClass::Metadata => QueryCapability::Metadata,
                    ContentClass::Ocr => QueryCapability::Ocr,
                    ContentClass::Derived => QueryCapability::DerivedRead,
                };
                !self.grant.capabilities.contains(&capability)
            })
            || self.provenance.query_engine_version.is_empty()
            || self.provenance.schema_build_id.is_empty()
            || self.provenance.projection_build_id.is_empty()
            || self.provenance.sqlite_version.is_empty()
            || self.provenance.sqlite_source_id.is_empty()
        {
            return Err("response scope and provenance must be explicit".to_owned());
        }
        for effective in &self.scope.effective_ranges {
            if !self.scope.requested_ranges.iter().any(|requested| {
                requested.start <= effective.start && effective.end <= requested.end
            }) {
                return Err("effective scope must be clipped from a requested range".to_owned());
            }
            let allowed = match &self.grant.time_scope {
                GrantTimeScope::Absolute { range } => {
                    range.start <= effective.start && effective.end <= range.end
                }
                GrantTimeScope::RollingHorizon { seconds } => {
                    let seconds = i64::try_from(*seconds).unwrap_or(i64::MAX);
                    effective.start >= self.generated_at - chrono::Duration::seconds(seconds)
                        && effective.end <= self.generated_at
                }
            };
            if !allowed {
                return Err("effective scope exceeds the disclosure grant".to_owned());
            }
        }
        if let Some(coverage) = &self.coverage {
            coverage.validate()?;
            if !self
                .scope
                .effective_ranges
                .iter()
                .any(|range| range.start <= coverage.range.start && coverage.range.end <= range.end)
            {
                return Err("coverage exceeds the effective query scope".to_owned());
            }
        }
        let outer_coverage_absent = matches!(
            self.operation,
            QueryOperationKind::Status
                | QueryOperationKind::Schemas
                | QueryOperationKind::ComparePeriods
        );
        if outer_coverage_absent != self.coverage.is_none() {
            return Err(
                "single-range factual responses require outer coverage; informational and comparison responses do not"
                    .to_owned(),
            );
        }
        if !matches!(
            self.operation,
            QueryOperationKind::Status | QueryOperationKind::Schemas
        ) && self.scope.effective_ranges.is_empty()
        {
            return Err("factual query responses require an effective UTC scope".to_owned());
        }
        let paged = matches!(
            self.operation,
            QueryOperationKind::ListChunks
                | QueryOperationKind::SearchActivity
                | QueryOperationKind::SupportingEvidence
                | QueryOperationKind::ListDerived
        );
        if paged != self.page.is_some() {
            return Err("pagination metadata and query operation disagree".to_owned());
        }
        if self
            .page
            .as_ref()
            .is_some_and(|page| page.returned_items > self.grant.limits.max_page_items)
        {
            return Err("response page exceeds the grant item limit".to_owned());
        }
        validate_unique(&self.provenance.source_event_ids, "provenance event IDs")?;
        validate_unique(
            &self.provenance.source_chunk_revision_ids,
            "provenance chunk revision IDs",
        )?;
        self.result.validate()?;
        self.result
            .validate_effective_ranges(&self.scope.effective_ranges)?;
        for class in self.result.required_content_classes() {
            if !self.scope.content_classes.contains(&class) {
                return Err("returned content class is outside the effective scope".to_owned());
            }
        }
        if let Some(page) = &self.page {
            let actual = self
                .result
                .top_level_item_count()
                .ok_or_else(|| "paged response result has no top-level list".to_owned())?;
            if usize::try_from(page.returned_items).ok() != Some(actual) {
                return Err("page returned_items must equal the returned list length".to_owned());
            }
        }
        if self.result.contains_ocr() && !self.scope.ocr_included {
            return Err("response contains OCR outside its declared scope".to_owned());
        }
        Ok(())
    }
}

fn validate_unique<T: Eq + std::hash::Hash>(items: &[T], label: &str) -> Result<(), String> {
    let unique: HashSet<_> = items.iter().collect();
    (unique.len() == items.len())
        .then_some(())
        .ok_or_else(|| format!("{label} must be unique"))
}

fn reject_query_path_and_bytes(json: &str) -> Result<(), ContractError> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| ContractError::InvalidJson(error.to_string()))?;
    if contains_forbidden_key(&value, QUERY_FORBIDDEN_KEYS) {
        return Err(ContractError::Validation(
            "query responses cannot expose filesystem paths or image bytes".to_owned(),
        ));
    }
    Ok(())
}

fn contains_forbidden_key(value: &Value, forbidden: &[&str]) -> bool {
    match value {
        Value::Object(object) => {
            object.keys().any(|key| forbidden.contains(&key.as_str()))
                || object
                    .values()
                    .any(|child| contains_forbidden_key(child, forbidden))
        }
        Value::Array(array) => array
            .iter()
            .any(|child| contains_forbidden_key(child, forbidden)),
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryExchange {
    pub request: QueryRequest,
    pub response: QueryResponse,
}

impl QueryExchange {
    pub fn validate(&self) -> Result<(), String> {
        self.request.validate()?;
        self.response.validate()?;
        if self.request.request_id != self.response.request_id
            || self.request.store_generation != self.response.store_generation
            || self.request.client_id != self.response.grant.client_id
            || self.request.grant_id != self.response.grant.grant_id
            || self.request.operation.kind() != self.response.operation
        {
            return Err(
                "query request and response are not the same authorized exchange".to_owned(),
            );
        }
        let expected_ranges: Option<Vec<&UtcRange>> = match &self.request.operation {
            QueryOperation::ListChunks { filter, .. }
            | QueryOperation::SearchActivity { filter, .. }
            | QueryOperation::Statistics { filter }
            | QueryOperation::BuildContextPacket { filter, .. } => Some(vec![&filter.range]),
            QueryOperation::ComparePeriods { first, second } => Some(vec![first, second]),
            QueryOperation::ListDerived { range, .. } => Some(vec![range]),
            QueryOperation::Status
            | QueryOperation::Schemas
            | QueryOperation::ReadChunk { .. }
            | QueryOperation::GetEvent { .. }
            | QueryOperation::GetArtifact { .. }
            | QueryOperation::InspectMoment { .. }
            | QueryOperation::SupportingEvidence { .. } => None,
        };
        if let Some(expected_ranges) = expected_ranges
            && (expected_ranges.len() != self.response.scope.requested_ranges.len()
                || expected_ranges
                    .iter()
                    .zip(&self.response.scope.requested_ranges)
                    .any(|(expected, actual)| *expected != actual))
        {
            return Err("response scope does not match the request range".to_owned());
        }
        let requested_ocr = match &self.request.operation {
            QueryOperation::SearchActivity { include_ocr, .. }
            | QueryOperation::BuildContextPacket { include_ocr, .. } => Some(*include_ocr),
            _ => None,
        };
        if requested_ocr.is_some_and(|requested| requested != self.response.scope.ocr_included) {
            return Err("response OCR scope does not match the request".to_owned());
        }
        let request_page = match &self.request.operation {
            QueryOperation::ListChunks { page, .. }
            | QueryOperation::SearchActivity { page, .. }
            | QueryOperation::SupportingEvidence { page, .. }
            | QueryOperation::ListDerived { page, .. } => Some(page),
            _ => None,
        };
        if let (Some(request_page), Some(response_page)) = (request_page, &self.response.page)
            && response_page.returned_items > request_page.limit
        {
            return Err("response page exceeds the requested item limit".to_owned());
        }
        let identity_matches = match (&self.request.operation, &self.response.result) {
            (QueryOperation::ReadChunk { chunk_id }, QueryResult::Chunk { chunk, .. }) => {
                chunk_id == &chunk.chunk_id
            }
            (QueryOperation::GetEvent { event_id }, QueryResult::Event { event }) => {
                event_id == &event.event_id
            }
            (
                QueryOperation::GetArtifact {
                    artifact_id,
                    revision_id,
                },
                QueryResult::Artifact { artifact },
            ) => {
                artifact_id == &artifact.artifact_id
                    && revision_id
                        .as_ref()
                        .is_none_or(|revision| revision == &artifact.revision_id)
            }
            _ => true,
        };
        if !identity_matches {
            return Err("query result identity does not match its requested ID".to_owned());
        }
        Ok(())
    }
}
