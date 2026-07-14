//! Narrow, versioned C ABI for the signed macOS application.
//!
//! Handles and output buffers are process-local registry tokens rather than
//! caller-visible Rust pointers. This makes stale/double close and free calls
//! ordinary typed failures instead of use-after-free undefined behavior.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use chronicle_domain::{
    ActivityFilter, ArtifactId, ArtifactRevisionId, CaptureCadence, ChunkGap, ChunkId,
    ChunkRevision, ChunkRevisionId, ClientId, DeviceId, DimensionKind, DisclosureGrant,
    DurationEstimate, EventEnvelope, EventId, EventPayload, EvidenceSeconds, EvidenceState,
    GrantId, GrantState, ImageArtifactId, ObservationContent, OcrState, PageInfo, PageRequest,
    PermittedWindowContext, PresenceSeconds, PresenceState, QueryArtifact, QueryCoverage,
    QueryEvent, QueryEventPayload, QueryObservationContent, ReceiptId, ScreenshotProjectedState,
    ScreenshotRetention, SharedServiceRequest, Transition, UtcRange, parse_versioned,
    validate_schema_version,
};
use chronicle_engine::{
    CadenceStamp, ChunkerConfig, EngineError, IngestRequest, RecordingCoordinator,
    RuntimeGapReason, RuntimeGapReconcileRequest, SharedService, SharedServiceError,
    StartupReconcileRequest, StudyBoundary,
};
use chronicle_store::{
    ActivitySearch, CaptureOwnerGuard, FactualStatistics, FaultInjector, LockManager, ManagedRoot,
    ProjectionAnchors, ProjectionHighWater, SearchHit, SqliteStore, StoreError, StoreGeneration,
    StoreQueries,
};
use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const ABI_SCHEMA_VERSION: &str = chronicle_domain::CONTRACT_VERSION;
const MAX_OPEN_REQUEST_BYTES: usize = 16 * 1024;
const MAX_CALL_REQUEST_BYTES: usize = chronicle_domain::MAX_SHARED_REQUEST_BYTES + 16 * 1024;
const MAX_INGEST_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_REQUEST_BYTES: usize = 8 * 1024;
const MAX_ENCODED_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_FACTUAL_REPORT_RANGE_SECONDS: i64 = 31 * 24 * 60 * 60;
const MAX_FACTUAL_REPORT_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TIMELINE_RANGE_SECONDS: i64 = 31 * 24 * 60 * 60;
const MAX_TIMELINE_PAGE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TIMELINE_SEARCH_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TIMELINE_DETAIL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ANALYSIS_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TIMELINE_PAGE_ITEMS: u32 = 100;
const MAX_SNAPSHOT_TOKEN_BYTES: usize = 2 * 1024;
const SNAPSHOT_TOKEN_VERSION: &str = "open-chronicle/timeline-snapshot/v1";

/// A registry-owned byte allocation. Callers must copy bytes before freeing.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ChronicleBuffer {
    pub token: u64,
    pub ptr: *const u8,
    pub len: usize,
}

impl ChronicleBuffer {
    const EMPTY: Self = Self {
        token: 0,
        ptr: ptr::null(),
        len: 0,
    };
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChronicleStatus {
    Ok = 0,
    InvalidArgument = 1,
    InvalidHandle = 2,
    Contract = 3,
    StaleGeneration = 4,
    NotFound = 5,
    NotRetained = 6,
    TooLarge = 7,
    Io = 8,
    Panic = 9,
    Internal = 10,
    InvalidBuffer = 11,
    CaptureOwnerActive = 12,
}

#[derive(Debug)]
struct FfiError {
    status: ChronicleStatus,
    code: &'static str,
    message: String,
    retryable: bool,
}

impl FfiError {
    fn new(status: ChronicleStatus, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable: false,
        }
    }

    fn stale(expected: u64, actual: u64) -> Self {
        Self::new(
            ChronicleStatus::StaleGeneration,
            "stale-generation",
            format!("store generation is stale; expected {expected}, found {actual}"),
        )
    }

    fn retryable(status: ChronicleStatus, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable: true,
        }
    }

    fn response(&self) -> Value {
        json!({
            "schema_version": ABI_SCHEMA_VERSION,
            "ok": false,
            "error": {
                "code": self.code,
                "message": self.message,
                "retryable": self.retryable,
            }
        })
    }
}

impl From<StoreError> for FfiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::StaleGeneration { expected, actual } => Self::stale(expected, actual),
            StoreError::CaptureOwnerActive => Self::new(
                ChronicleStatus::CaptureOwnerActive,
                "capture-owner-active",
                "another Open Chronicle application process owns capture",
            ),
            StoreError::ScreenshotFreeSpace { .. } => Self::retryable(
                ChronicleStatus::Io,
                "screenshot-free-space",
                "available storage cannot preserve the screenshot transaction floor",
            ),
            StoreError::ScreenshotImageQuota { .. } => Self::retryable(
                ChronicleStatus::Io,
                "screenshot-image-quota",
                "the managed screenshot quota would be exceeded",
            ),
            StoreError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => Self::new(
                ChronicleStatus::NotFound,
                "not-found",
                "requested Chronicle data was not found",
            ),
            StoreError::Io(_) => Self::new(
                ChronicleStatus::Io,
                "io-error",
                "Chronicle storage could not complete the operation",
            ),
            StoreError::Contract(_) | StoreError::InvalidPath(_) => Self::new(
                ChronicleStatus::Contract,
                "contract-error",
                "request violates the Chronicle storage contract",
            ),
            StoreError::CursorNotFound | StoreError::CursorScopeMismatch => Self::new(
                ChronicleStatus::Contract,
                "invalid-pagination-cursor",
                "pagination cursor does not belong to this Chronicle snapshot",
            ),
            StoreError::GrantAlreadyExists => Self::new(
                ChronicleStatus::Contract,
                "disclosure-grant-conflict",
                "an existing disclosure grant has a different contract",
            ),
            other => Self::new(
                ChronicleStatus::Internal,
                "store-error",
                format!("Chronicle storage rejected the operation: {other}"),
            ),
        }
    }
}

impl From<EngineError> for FfiError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Store(store) => store.into(),
            EngineError::StudyNotStarted => Self::new(
                ChronicleStatus::Contract,
                "study-not-started",
                "the configured study has not started",
            ),
            EngineError::StudyExpired => Self::new(
                ChronicleStatus::Contract,
                "study-expired",
                "the configured study has expired",
            ),
            EngineError::Identifier(_)
            | EngineError::Aggregation(_)
            | EngineError::Configuration(_)
            | EngineError::Cadence(_) => Self::new(
                ChronicleStatus::Contract,
                "ingest-contract-error",
                format!("Chronicle rejected the ingest request: {error}"),
            ),
        }
    }
}

impl From<SharedServiceError> for FfiError {
    fn from(error: SharedServiceError) -> Self {
        match error {
            SharedServiceError::StaleGeneration { expected, actual } => {
                Self::stale(expected, actual)
            }
            SharedServiceError::Store(store) => store.into(),
            SharedServiceError::NotFound => Self::new(
                ChronicleStatus::NotFound,
                "not-found",
                "requested Chronicle evidence was not found",
            ),
            other => Self::new(
                ChronicleStatus::Contract,
                "shared-service-error",
                format!("Chronicle shared service rejected the request: {other}"),
            ),
        }
    }
}

#[derive(Debug)]
struct CoreHandle {
    _capture_owner: CaptureOwnerGuard,
    root: ManagedRoot,
    sqlite: SqliteStore,
    service: SharedService,
    coordinator: RecordingCoordinator,
    opened_generation: u64,
}

#[derive(Debug)]
struct HandleSlot {
    state: Mutex<Option<CoreHandle>>,
}

static HANDLES: OnceLock<Mutex<BTreeMap<u64, Arc<HandleSlot>>>> = OnceLock::new();
static BUFFERS: OnceLock<Mutex<BTreeMap<u64, Box<[u8]>>>> = OnceLock::new();
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
static NEXT_BUFFER: AtomicU64 = AtomicU64::new(1);

fn handles() -> &'static Mutex<BTreeMap<u64, Arc<HandleSlot>>> {
    HANDLES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn buffers() -> &'static Mutex<BTreeMap<u64, Box<[u8]>>> {
    BUFFERS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn next_token(counter: &AtomicU64) -> Result<u64, FfiError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current != u64::MAX).then_some(current + 1)
        })
        .map_err(|_| {
            FfiError::new(
                ChronicleStatus::Internal,
                "token-exhausted",
                "Chronicle process token space is exhausted",
            )
        })
}

fn register_handle(core: CoreHandle) -> Result<u64, FfiError> {
    let token = next_token(&NEXT_HANDLE)?;
    lock_recover(handles()).insert(
        token,
        Arc::new(HandleSlot {
            state: Mutex::new(Some(core)),
        }),
    );
    Ok(token)
}

fn with_handle<T>(
    token: u64,
    operation: impl FnOnce(&mut CoreHandle) -> Result<T, FfiError>,
) -> Result<T, FfiError> {
    if token == 0 {
        return Err(FfiError::new(
            ChronicleStatus::InvalidHandle,
            "invalid-handle",
            "Chronicle handle is invalid or closed",
        ));
    }
    let slot = lock_recover(handles())
        .get(&token)
        .cloned()
        .ok_or_else(|| {
            FfiError::new(
                ChronicleStatus::InvalidHandle,
                "invalid-handle",
                "Chronicle handle is invalid or closed",
            )
        })?;
    let mut state = lock_recover(&slot.state);
    let core = state.as_mut().ok_or_else(|| {
        FfiError::new(
            ChronicleStatus::InvalidHandle,
            "invalid-handle",
            "Chronicle handle is invalid or closed",
        )
    })?;
    operation(core)
}

fn close_handle(token: u64) -> Result<(), FfiError> {
    let slot = lock_recover(handles()).remove(&token).ok_or_else(|| {
        FfiError::new(
            ChronicleStatus::InvalidHandle,
            "invalid-handle",
            "Chronicle handle is invalid or already closed",
        )
    })?;
    let mut state = lock_recover(&slot.state);
    if state.take().is_none() {
        return Err(FfiError::new(
            ChronicleStatus::InvalidHandle,
            "invalid-handle",
            "Chronicle handle is invalid or already closed",
        ));
    }
    Ok(())
}

fn store_buffer(bytes: Vec<u8>) -> Result<ChronicleBuffer, FfiError> {
    if bytes.is_empty() {
        return Err(FfiError::new(
            ChronicleStatus::Internal,
            "empty-output",
            "Chronicle attempted to return an empty owned response",
        ));
    }
    let token = next_token(&NEXT_BUFFER)?;
    let boxed = bytes.into_boxed_slice();
    let buffer = ChronicleBuffer {
        token,
        ptr: boxed.as_ptr(),
        len: boxed.len(),
    };
    lock_recover(buffers()).insert(token, boxed);
    Ok(buffer)
}

fn success(result: Value) -> Value {
    json!({
        "schema_version": ABI_SCHEMA_VERSION,
        "ok": true,
        "result": result,
    })
}

fn serialize_value<T: Serialize>(value: T) -> Result<Value, FfiError> {
    serde_json::to_value(value).map_err(|_| {
        FfiError::new(
            ChronicleStatus::Internal,
            "serialization-error",
            "Chronicle call response could not be serialized",
        )
    })
}

fn encode_value(value: &Value) -> Result<ChronicleBuffer, FfiError> {
    serde_json::to_vec(value)
        .map_err(|_| {
            FfiError::new(
                ChronicleStatus::Internal,
                "serialization-error",
                "Chronicle could not encode its response",
            )
        })
        .and_then(store_buffer)
}

unsafe fn initialize_output(out: *mut ChronicleBuffer) -> Result<(), FfiError> {
    if out.is_null() {
        return Err(FfiError::new(
            ChronicleStatus::InvalidArgument,
            "null-output",
            "output buffer pointer is required",
        ));
    }
    // SAFETY: caller provides a writable ChronicleBuffer by the C contract.
    unsafe { ptr::write(out, ChronicleBuffer::EMPTY) };
    Ok(())
}

unsafe fn write_output(out: *mut ChronicleBuffer, buffer: ChronicleBuffer) -> Result<(), FfiError> {
    if out.is_null() {
        return Err(FfiError::new(
            ChronicleStatus::InvalidArgument,
            "null-output",
            "output buffer pointer is required",
        ));
    }
    // SAFETY: caller provides a writable ChronicleBuffer by the C contract.
    unsafe { ptr::write(out, buffer) };
    Ok(())
}

unsafe fn copy_input(
    pointer: *const u8,
    length: usize,
    maximum: usize,
    name: &'static str,
) -> Result<Vec<u8>, FfiError> {
    if pointer.is_null() || length == 0 {
        return Err(FfiError::new(
            ChronicleStatus::InvalidArgument,
            "invalid-input-pointer",
            format!("{name} must be non-null and nonempty"),
        ));
    }
    if length > maximum {
        return Err(FfiError::new(
            ChronicleStatus::TooLarge,
            "input-too-large",
            format!("{name} exceeds its byte limit"),
        ));
    }
    // SAFETY: the C contract requires the input allocation to be readable for
    // `length` bytes for this call. We copy immediately and retain no borrow.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec())
}

fn utf8_json(bytes: Vec<u8>) -> Result<String, FfiError> {
    String::from_utf8(bytes).map_err(|_| {
        FfiError::new(
            ChronicleStatus::Contract,
            "invalid-utf8",
            "request must be UTF-8 JSON",
        )
    })
}

fn json_boundary(
    out: *mut ChronicleBuffer,
    operation: impl FnOnce() -> Result<Value, FfiError>,
) -> u32 {
    // Initialize first so a valid caller never observes stale output fields.
    if let Err(error) = unsafe { initialize_output(out) } {
        return error.status as u32;
    }
    let result = catch_unwind(AssertUnwindSafe(operation));
    let (status, value) = match result {
        Ok(Ok(value)) => (ChronicleStatus::Ok, success(value)),
        Ok(Err(error)) => (error.status, error.response()),
        Err(_) => {
            let error = FfiError::new(
                ChronicleStatus::Panic,
                "panic-contained",
                "Chronicle contained an internal panic at the ABI boundary",
            );
            (error.status, error.response())
        }
    };
    match encode_value(&value).and_then(|buffer| {
        // SAFETY: initialize_output already validated the same output pointer.
        unsafe { write_output(out, buffer) }
    }) {
        Ok(()) => status as u32,
        Err(error) => error.status as u32,
    }
}

#[derive(Debug, Deserialize)]
struct OpenRequest {
    schema_version: String,
    application_support_path: String,
    now: DateTime<Utc>,
    aggregator_version: String,
    max_cadence_seconds: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRequest {
    schema_version: String,
    now: DateTime<Utc>,
    #[serde(default)]
    request: Option<SharedServiceRequest>,
    #[serde(default)]
    control: Option<AppControl>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum AppControl {
    RuntimeState,
    StorageHealth,
    FactualReport {
        range: UtcRange,
    },
    TimelinePage {
        stable_cutoff: DateTime<Utc>,
        #[serde(default)]
        snapshot_token: Option<String>,
        filter: TimelineFilter,
        page: PageRequest,
    },
    TimelineSearch {
        stable_cutoff: DateTime<Utc>,
        #[serde(default)]
        snapshot_token: Option<String>,
        filter: TimelineFilter,
        query: String,
        page: PageRequest,
    },
    TimelineChunkDetail {
        snapshot_token: String,
        #[serde(default)]
        revision_id: Option<ChunkRevisionId>,
        #[serde(default)]
        chunk_id: Option<ChunkId>,
    },
    TimelineEventDetail {
        snapshot_token: String,
        event_id: EventId,
    },
    AnalysisPage {
        stable_cutoff: DateTime<Utc>,
        #[serde(default)]
        snapshot_token: Option<String>,
        range: UtcRange,
        page: PageRequest,
    },
    AnalysisDetail {
        snapshot_token: String,
        artifact_id: ArtifactId,
        #[serde(default)]
        revision_id: Option<ArtifactRevisionId>,
    },
    SetRecordingPreference {
        enabled: bool,
    },
    SetCadence {
        cadence: CaptureCadence,
    },
    SetScreenshotRetention {
        retention: ScreenshotRetention,
    },
    ConfigureStudy {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    UsePersonalMode,
    ExtendStudy {
        new_end: DateTime<Utc>,
    },
    CaptureAdmission,
    ReconcilePendingImages,
    ReconcileRuntimeGap {
        reason: RuntimeGapReason,
        device_id: DeviceId,
        display_timezone: String,
    },
    PrepareTermination {
        session_id: String,
    },
    StartupReconcile {
        session_id: String,
        device_id: DeviceId,
        display_timezone: String,
    },
    InstallDisclosureGrant {
        grant: DisclosureGrant,
    },
    RevokeDisclosureGrant {
        grant_id: GrantId,
        client_id: ClientId,
        receipt_id: ReceiptId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimelineFilter {
    range: UtcRange,
    application_bundle_id: Option<String>,
    window_text: Option<String>,
    authorized_domain: Option<String>,
    #[serde(default)]
    coverage_states: Vec<TimelineCoverageState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum TimelineCoverageState {
    Captured,
    Protected,
    Paused,
    Unavailable,
    Error,
    MissingObservation,
}

#[derive(Debug, Serialize)]
struct FactualReportSnapshot {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    stable_cutoff: DateTime<Utc>,
    store_generation: u64,
    range: UtcRange,
    coverage: QueryCoverage,
    factual_totals: Vec<FactualReportTotal>,
    activity_buckets: Vec<FactualReportActivityBucket>,
    transitions: Vec<Transition>,
    domain_context_available: bool,
    provenance: FactualReportProvenance,
}

#[derive(Debug, Serialize)]
struct FactualReportTotal {
    dimension: DimensionKind,
    key: String,
    label: String,
    parent_key: Option<String>,
    estimated_seconds: u32,
    supporting_chunk_ids: Vec<ChunkId>,
    supporting_event_ids: Vec<EventId>,
}

#[derive(Debug, Serialize)]
struct FactualReportActivityBucket {
    chunk_id: ChunkId,
    revision_id: ChunkRevisionId,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    display_timezone: String,
    evidence_seconds: EvidenceSeconds,
    presence_seconds: PresenceSeconds,
    duration_estimates: Vec<DurationEstimate>,
    gaps: Vec<ChunkGap>,
    transitions: Vec<Transition>,
    late_input: bool,
}

#[derive(Debug, Serialize)]
struct FactualReportProvenance {
    query_engine_version: &'static str,
    projection_build_id: &'static str,
    sqlite_version: &'static str,
    sqlite_source_id: &'static str,
    source_event_ids: Vec<EventId>,
    source_chunk_revision_ids: Vec<ChunkRevisionId>,
}

#[derive(Debug, Serialize)]
struct TimelinePageSnapshot {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    stable_cutoff: DateTime<Utc>,
    snapshot_token: String,
    store_generation: u64,
    filter: TimelineFilter,
    coverage: QueryCoverage,
    chunks: Vec<TimelineChunkBand>,
    page: PageInfo,
    domain_context_available: bool,
    provenance: FactualReportProvenance,
}

#[derive(Debug, Serialize)]
struct TimelineChunkBand {
    chunk_id: ChunkId,
    revision_id: ChunkRevisionId,
    prior_revision_id: Option<ChunkRevisionId>,
    supersedes_revision_id: Option<ChunkRevisionId>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    generated_at: DateTime<Utc>,
    display_timezone: String,
    aggregator_version: String,
    input_digest: String,
    store_generation: u64,
    finalization_cadence_seconds: u32,
    evidence_seconds: EvidenceSeconds,
    presence_seconds: PresenceSeconds,
    duration_estimates: Vec<DurationEstimate>,
    transitions: Vec<Transition>,
    extracts: Vec<TimelineExtractMetadata>,
    gaps: Vec<ChunkGap>,
    supporting_event_ids: Vec<EventId>,
    late_input: bool,
}

#[derive(Debug, Serialize)]
struct TimelineExtractMetadata {
    source_event_id: EventId,
    character_count: u32,
    untrusted_evidence: bool,
}

#[derive(Debug, Serialize)]
struct TimelineSearchSnapshot {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    stable_cutoff: DateTime<Utc>,
    snapshot_token: String,
    store_generation: u64,
    filter: TimelineFilter,
    coverage: QueryCoverage,
    query: String,
    hits: Vec<TimelineSearchHit>,
    page: PageInfo,
    provenance: FactualReportProvenance,
}

#[derive(Debug, Serialize)]
struct TimelineSearchHit {
    event_id: EventId,
    observed_at: DateTime<Utc>,
    context: PermittedWindowContext,
    evidence_state: EvidenceState,
    presence_state: PresenceState,
    ocr_state: OcrState,
    snippet: TimelineSearchSnippet,
    untrusted_evidence: bool,
}

#[derive(Debug, Serialize)]
struct TimelineSearchSnippet {
    text: String,
    highlights: Vec<TimelineHighlightRange>,
}

#[derive(Debug, Serialize)]
struct TimelineHighlightRange {
    start: u32,
    length: u32,
}

#[derive(Debug, Serialize)]
struct TimelineChunkDetailSnapshot {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    stable_cutoff: DateTime<Utc>,
    snapshot_token: String,
    store_generation: u64,
    chunk: ChunkRevision,
    provenance: FactualReportProvenance,
}

#[derive(Debug, Serialize)]
struct TimelineEventDetailSnapshot {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    stable_cutoff: DateTime<Utc>,
    snapshot_token: String,
    store_generation: u64,
    event: QueryEvent,
    provenance: FactualReportProvenance,
}

#[derive(Debug, Serialize)]
struct AnalysisPageSnapshot {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    stable_cutoff: DateTime<Utc>,
    snapshot_token: String,
    store_generation: u64,
    range: UtcRange,
    artifacts: Vec<QueryArtifact>,
    page: PageInfo,
    provenance: AnalysisProvenance,
}

#[derive(Debug, Serialize)]
struct AnalysisDetailSnapshot {
    schema_version: &'static str,
    generated_at: DateTime<Utc>,
    stable_cutoff: DateTime<Utc>,
    snapshot_token: String,
    store_generation: u64,
    artifact: QueryArtifact,
    provenance: AnalysisProvenance,
}

#[derive(Debug, Serialize)]
struct AnalysisProvenance {
    query_engine_version: &'static str,
    projection_build_id: &'static str,
    sqlite_version: &'static str,
    sqlite_source_id: &'static str,
    source_event_ids: Vec<EventId>,
    source_chunk_ids: Vec<ChunkId>,
    source_artifact_revision_ids: Vec<ArtifactRevisionId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimelineSnapshotToken {
    version: String,
    store_generation: u64,
    projection_instance_id: String,
    stable_cutoff: DateTime<Utc>,
    event_rowid_high_water: u64,
    chunk_revision_rowid_high_water: u64,
    artifact_revision_rowid_high_water: u64,
    event_anchor_id: Option<EventId>,
    chunk_revision_anchor_id: Option<ChunkRevisionId>,
    artifact_revision_anchor_id: Option<ArtifactRevisionId>,
    scope_digest: String,
}

impl TimelineSnapshotToken {
    const fn high_water(&self) -> ProjectionHighWater {
        ProjectionHighWater {
            event_rowid: self.event_rowid_high_water,
            chunk_revision_rowid: self.chunk_revision_rowid_high_water,
            artifact_revision_rowid: self.artifact_revision_rowid_high_water,
        }
    }
}

#[derive(Serialize)]
struct TimelineSnapshotScope<'a> {
    operation: &'static str,
    stable_cutoff: DateTime<Utc>,
    filter: &'a TimelineFilter,
    query: Option<&'a str>,
}

#[derive(Serialize)]
struct AnalysisSnapshotScope<'a> {
    operation: &'static str,
    stable_cutoff: DateTime<Utc>,
    range: &'a UtcRange,
}

#[derive(Debug, Default)]
struct FactualReportTotalMetadata {
    label: String,
    parent_key: Option<String>,
    estimated_seconds: u32,
    supporting_event_ids: BTreeSet<EventId>,
}

fn validate_factual_report_range(range: &UtcRange, now: DateTime<Utc>) -> Result<(), FfiError> {
    let duration = range.end.signed_duration_since(range.start).num_seconds();
    let utc_aligned = range.start.timestamp().rem_euclid(300) == 0
        && range.end.timestamp().rem_euclid(300) == 0
        && range.start.timestamp_subsec_nanos() == 0
        && range.end.timestamp_subsec_nanos() == 0;
    if range.validate().is_err()
        || !utc_aligned
        || duration > MAX_FACTUAL_REPORT_RANGE_SECONDS
        || range.end > now
    {
        return Err(FfiError::new(
            ChronicleStatus::Contract,
            "invalid-factual-report-range",
            "factual report range must be a past, UTC five-minute-aligned interval of at most 31 days",
        ));
    }
    Ok(())
}

fn build_factual_report_snapshot(
    range: UtcRange,
    now: DateTime<Utc>,
    store_generation: u64,
    report: chronicle_store::StatisticsReport,
) -> Result<FactualReportSnapshot, FfiError> {
    if report.coverage.range != range {
        return Err(factual_report_inconsistent());
    }

    let mut metadata = BTreeMap::<(DimensionKind, String), FactualReportTotalMetadata>::new();
    let mut source_event_ids = BTreeSet::new();
    for chunk in &report.activity_chunks {
        source_event_ids.extend(chunk.supporting_event_ids.iter().cloned());
        for estimate in &chunk.duration_estimates {
            let parent_key = report_parent_key(&chunk.duration_estimates, estimate);
            let entry = metadata
                .entry((estimate.dimension, estimate.key.clone()))
                .or_default();
            if entry.parent_key.is_some() && entry.parent_key != parent_key {
                return Err(factual_report_inconsistent());
            }
            entry.label.clone_from(&estimate.label);
            entry.parent_key = parent_key;
            entry.estimated_seconds = entry
                .estimated_seconds
                .checked_add(estimate.estimated_seconds)
                .ok_or_else(factual_report_inconsistent)?;
            entry
                .supporting_event_ids
                .extend(estimate.supporting_event_ids.iter().cloned());
        }
    }

    let mut factual_totals = Vec::with_capacity(report.factual_totals.len());
    for total in report.factual_totals {
        let total_metadata = metadata
            .remove(&(total.dimension, total.key.clone()))
            .ok_or_else(factual_report_inconsistent)?;
        if total_metadata.label.is_empty()
            || total_metadata.estimated_seconds != total.estimated_seconds
        {
            return Err(factual_report_inconsistent());
        }
        factual_totals.push(FactualReportTotal {
            dimension: total.dimension,
            key: total.key,
            label: total_metadata.label,
            parent_key: total_metadata.parent_key,
            estimated_seconds: total.estimated_seconds,
            supporting_chunk_ids: total.supporting_chunk_ids,
            supporting_event_ids: total_metadata.supporting_event_ids.into_iter().collect(),
        });
    }
    if !metadata.is_empty() {
        return Err(factual_report_inconsistent());
    }
    let domain_context_available = factual_totals
        .iter()
        .any(|total| total.dimension == DimensionKind::AuthorizedDomain);
    let activity_buckets = report
        .activity_chunks
        .into_iter()
        .map(|chunk| FactualReportActivityBucket {
            chunk_id: chunk.chunk_id,
            revision_id: chunk.revision_id,
            start: chunk.window.start,
            end: chunk.window.end,
            display_timezone: chunk.display_timezone,
            evidence_seconds: chunk.evidence_seconds,
            presence_seconds: chunk.presence_seconds,
            duration_estimates: chunk.duration_estimates,
            gaps: chunk.gaps,
            transitions: chunk.transitions,
            late_input: chunk.late_input,
        })
        .collect();
    Ok(FactualReportSnapshot {
        schema_version: ABI_SCHEMA_VERSION,
        generated_at: now,
        stable_cutoff: now,
        store_generation,
        range,
        coverage: report.coverage,
        factual_totals,
        activity_buckets,
        transitions: report.transitions,
        domain_context_available,
        provenance: FactualReportProvenance {
            query_engine_version: env!("CARGO_PKG_VERSION"),
            projection_build_id: chronicle_store::STORE_BUILD_ID,
            sqlite_version: chronicle_store::SQLITE_BUNDLED_VERSION,
            sqlite_source_id: chronicle_store::SQLITE_BUNDLED_SOURCE_ID,
            source_event_ids: source_event_ids.into_iter().collect(),
            source_chunk_revision_ids: report.source_chunk_revision_ids,
        },
    })
}

fn report_parent_key(
    estimates: &[DurationEstimate],
    estimate: &DurationEstimate,
) -> Option<String> {
    if estimate.dimension != DimensionKind::Window {
        return None;
    }
    estimates
        .iter()
        .filter(|candidate| candidate.dimension == DimensionKind::Application)
        .filter(|candidate| {
            estimate
                .key
                .strip_prefix(&candidate.key)
                .is_some_and(|suffix| suffix.starts_with(':'))
        })
        .max_by_key(|candidate| candidate.key.len())
        .map(|candidate| candidate.key.clone())
        .or_else(|| {
            estimate
                .key
                .split_once(':')
                .map(|(parent, _)| parent.to_owned())
        })
}

fn factual_report_inconsistent() -> FfiError {
    FfiError::new(
        ChronicleStatus::Internal,
        "factual-report-inconsistent",
        "Chronicle could not reconcile the factual report snapshot",
    )
}

fn serialize_bounded_factual_report(
    snapshot: FactualReportSnapshot,
    max_bytes: usize,
) -> Result<Value, FfiError> {
    let value = serialize_value(snapshot)?;
    let size = serde_json::to_vec(&value)
        .map_err(|_| factual_report_inconsistent())?
        .len();
    if size > max_bytes {
        return Err(FfiError::new(
            ChronicleStatus::TooLarge,
            "factual-report-too-large",
            "factual report exceeds the app response budget; choose a shorter range",
        ));
    }
    Ok(value)
}

fn validate_timeline_filter(
    filter: &TimelineFilter,
    stable_cutoff: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), FfiError> {
    let duration = filter
        .range
        .end
        .signed_duration_since(filter.range.start)
        .num_seconds();
    let aligned = filter.range.start.timestamp().rem_euclid(300) == 0
        && filter.range.end.timestamp().rem_euclid(300) == 0
        && filter.range.start.timestamp_subsec_nanos() == 0
        && filter.range.end.timestamp_subsec_nanos() == 0;
    if filter.range.validate().is_err()
        || !aligned
        || duration > MAX_TIMELINE_RANGE_SECONDS
        || filter.range.end > stable_cutoff
        || stable_cutoff > now
        || stable_cutoff.timestamp_subsec_nanos() != 0
    {
        return Err(FfiError::new(
            ChronicleStatus::Contract,
            "invalid-timeline-range",
            "timeline range must be a past, UTC five-minute-aligned interval of at most 31 days inside a whole-second stable cutoff",
        ));
    }
    for (value, maximum, label) in [
        (
            filter.application_bundle_id.as_deref(),
            255,
            "application bundle ID",
        ),
        (filter.window_text.as_deref(), 512, "window text"),
        (
            filter.authorized_domain.as_deref(),
            253,
            "authorized domain",
        ),
    ] {
        if value.is_some_and(|value| value.is_empty() || value.chars().count() > maximum) {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "invalid-timeline-filter",
                format!("timeline {label} filter is empty or exceeds its character limit"),
            ));
        }
    }
    if filter
        .coverage_states
        .iter()
        .enumerate()
        .any(|(index, state)| filter.coverage_states[..index].contains(state))
    {
        return Err(FfiError::new(
            ChronicleStatus::Contract,
            "invalid-timeline-filter",
            "timeline coverage-state filters must be unique",
        ));
    }
    Ok(())
}

fn validate_timeline_page(page: &PageRequest, cursor_kind: &str) -> Result<(), FfiError> {
    if page.limit == 0 || page.limit > MAX_TIMELINE_PAGE_ITEMS {
        return Err(FfiError::new(
            ChronicleStatus::Contract,
            "invalid-timeline-page",
            format!("timeline page limit must be 1..={MAX_TIMELINE_PAGE_ITEMS}"),
        ));
    }
    if let Some(cursor) = page.cursor.as_deref() {
        if cursor.is_empty() || cursor.len() > 255 {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "invalid-pagination-cursor",
                "timeline cursor is empty or exceeds its byte limit",
            ));
        }
        let valid = match cursor_kind {
            "chunk" => ChunkId::new(cursor.to_owned()).is_ok(),
            "event" => EventId::new(cursor.to_owned()).is_ok(),
            "artifact" => ArtifactId::new(cursor.to_owned()).is_ok(),
            _ => false,
        };
        if !valid {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "invalid-pagination-cursor",
                "timeline cursor is not a valid stable ID",
            ));
        }
    }
    Ok(())
}

fn timeline_activity_filter(filter: &TimelineFilter) -> (ActivityFilter, bool) {
    let mut evidence_states = Vec::new();
    let mut include_missing_observation = false;
    for state in &filter.coverage_states {
        match state {
            TimelineCoverageState::Captured => {
                evidence_states.push(EvidenceState::CapturedNew);
                evidence_states.push(EvidenceState::CapturedUnchanged);
            }
            TimelineCoverageState::Protected => evidence_states.push(EvidenceState::Protected),
            TimelineCoverageState::Paused => evidence_states.push(EvidenceState::Paused),
            TimelineCoverageState::Unavailable => {
                evidence_states.push(EvidenceState::Unavailable);
            }
            TimelineCoverageState::Error => evidence_states.push(EvidenceState::CaptureFailed),
            TimelineCoverageState::MissingObservation => include_missing_observation = true,
        }
    }
    (
        ActivityFilter {
            range: filter.range.clone(),
            application_bundle_id: filter.application_bundle_id.clone(),
            window_text: filter.window_text.clone(),
            authorized_domain: filter.authorized_domain.clone(),
            evidence_states,
        },
        include_missing_observation,
    )
}

fn timeline_scope_digest(
    operation: &'static str,
    stable_cutoff: DateTime<Utc>,
    filter: &TimelineFilter,
    query: Option<&str>,
) -> Result<String, FfiError> {
    let bytes = serde_json::to_vec(&TimelineSnapshotScope {
        operation,
        stable_cutoff,
        filter,
        query,
    })
    .map_err(|_| timeline_internal_error())?;
    Ok(hex_encode(&Sha256::digest(bytes)))
}

fn analysis_scope_digest(
    stable_cutoff: DateTime<Utc>,
    range: &UtcRange,
) -> Result<String, FfiError> {
    let bytes = serde_json::to_vec(&AnalysisSnapshotScope {
        operation: "analysis-page",
        stable_cutoff,
        range,
    })
    .map_err(|_| timeline_internal_error())?;
    Ok(hex_encode(&Sha256::digest(bytes)))
}

fn resolve_timeline_snapshot(
    token: Option<&str>,
    stable_cutoff: DateTime<Utc>,
    scope_digest: &str,
    store_generation: u64,
    queries: &StoreQueries,
    page_has_cursor: bool,
) -> Result<(TimelineSnapshotToken, String), FfiError> {
    if token.is_none() && page_has_cursor {
        return Err(FfiError::new(
            ChronicleStatus::Contract,
            "snapshot-token-required",
            "timeline pagination requires the snapshot token returned by the first page",
        ));
    }
    let snapshot = match token {
        Some(token) => {
            let snapshot = decode_snapshot_token(token)?;
            validate_snapshot_token(
                &snapshot,
                stable_cutoff,
                Some(scope_digest),
                store_generation,
            )?;
            ensure_snapshot_projection_identity(queries, &snapshot)?;
            snapshot
        }
        None => {
            let projection_instance_id =
                queries.projection_instance_id().map_err(FfiError::from)?;
            let high_water = queries.projection_high_water().map_err(FfiError::from)?;
            let anchors = queries
                .projection_anchors(high_water)
                .map_err(FfiError::from)?;
            TimelineSnapshotToken {
                version: SNAPSHOT_TOKEN_VERSION.to_owned(),
                store_generation,
                projection_instance_id,
                stable_cutoff,
                event_rowid_high_water: high_water.event_rowid,
                chunk_revision_rowid_high_water: high_water.chunk_revision_rowid,
                artifact_revision_rowid_high_water: high_water.artifact_revision_rowid,
                event_anchor_id: anchors.event_id,
                chunk_revision_anchor_id: anchors.chunk_revision_id,
                artifact_revision_anchor_id: anchors.artifact_revision_id,
                scope_digest: scope_digest.to_owned(),
            }
        }
    };
    let encoded = encode_snapshot_token(&snapshot)?;
    Ok((snapshot, encoded))
}

fn validate_snapshot_token(
    snapshot: &TimelineSnapshotToken,
    stable_cutoff: DateTime<Utc>,
    expected_scope_digest: Option<&str>,
    store_generation: u64,
) -> Result<(), FfiError> {
    if snapshot.version != SNAPSHOT_TOKEN_VERSION
        || snapshot.store_generation != store_generation
        || snapshot.projection_instance_id.len() != 36
        || snapshot.stable_cutoff != stable_cutoff
        || snapshot.stable_cutoff.timestamp_subsec_nanos() != 0
        || snapshot.scope_digest.len() != 64
        || snapshot.event_anchor_id.is_some() != (snapshot.event_rowid_high_water > 0)
        || snapshot.chunk_revision_anchor_id.is_some()
            != (snapshot.chunk_revision_rowid_high_water > 0)
        || snapshot.artifact_revision_anchor_id.is_some()
            != (snapshot.artifact_revision_rowid_high_water > 0)
        || expected_scope_digest.is_some_and(|expected| snapshot.scope_digest != expected)
    {
        return Err(FfiError::new(
            ChronicleStatus::Contract,
            "snapshot-scope-mismatch",
            "timeline snapshot token does not match this store, cutoff, or query scope",
        ));
    }
    Ok(())
}

fn ensure_snapshot_projection_identity(
    queries: &StoreQueries,
    snapshot: &TimelineSnapshotToken,
) -> Result<(), FfiError> {
    if queries.projection_instance_id().map_err(FfiError::from)? != snapshot.projection_instance_id
    {
        return Err(snapshot_no_longer_available());
    }
    let current = queries.projection_high_water().map_err(FfiError::from)?;
    if snapshot.event_rowid_high_water > current.event_rowid
        || snapshot.chunk_revision_rowid_high_water > current.chunk_revision_rowid
        || snapshot.artifact_revision_rowid_high_water > current.artifact_revision_rowid
    {
        return Err(snapshot_no_longer_available());
    }
    let current_anchors = queries
        .projection_anchors(snapshot.high_water())
        .map_err(FfiError::from)?;
    let token_anchors = ProjectionAnchors {
        event_id: snapshot.event_anchor_id.clone(),
        chunk_revision_id: snapshot.chunk_revision_anchor_id.clone(),
        artifact_revision_id: snapshot.artifact_revision_anchor_id.clone(),
    };
    if current_anchors != token_anchors {
        return Err(snapshot_no_longer_available());
    }
    Ok(())
}

fn snapshot_no_longer_available() -> FfiError {
    FfiError::new(
        ChronicleStatus::StaleGeneration,
        "snapshot-no-longer-available",
        "the timeline projection changed incompatibly; refresh from the first page",
    )
}

fn encode_snapshot_token(snapshot: &TimelineSnapshotToken) -> Result<String, FfiError> {
    let body = serde_json::to_vec(snapshot).map_err(|_| timeline_internal_error())?;
    let checksum = Sha256::digest(&body);
    let token = format!("ocs1.{}.{}", hex_encode(&body), hex_encode(&checksum));
    if token.len() > MAX_SNAPSHOT_TOKEN_BYTES {
        return Err(timeline_internal_error());
    }
    Ok(token)
}

fn decode_snapshot_token(token: &str) -> Result<TimelineSnapshotToken, FfiError> {
    if token.is_empty() || token.len() > MAX_SNAPSHOT_TOKEN_BYTES {
        return Err(invalid_snapshot_token());
    }
    let mut parts = token.split('.');
    let (Some(prefix), Some(body), Some(checksum), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid_snapshot_token());
    };
    if prefix != "ocs1" || checksum.len() != 64 {
        return Err(invalid_snapshot_token());
    }
    let body = hex_decode(body).ok_or_else(invalid_snapshot_token)?;
    let supplied_checksum = hex_decode(checksum).ok_or_else(invalid_snapshot_token)?;
    if supplied_checksum.as_slice() != Sha256::digest(&body).as_slice() {
        return Err(invalid_snapshot_token());
    }
    serde_json::from_slice(&body).map_err(|_| invalid_snapshot_token())
}

fn invalid_snapshot_token() -> FfiError {
    FfiError::new(
        ChronicleStatus::Contract,
        "invalid-snapshot-token",
        "timeline snapshot token is malformed or failed its integrity check",
    )
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Some((high << 4) | low)
        })
        .collect()
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn timeline_band(chunk: ChunkRevision) -> Result<TimelineChunkBand, FfiError> {
    let extracts = chunk
        .ocr_extracts
        .into_iter()
        .map(|extract| {
            Ok(TimelineExtractMetadata {
                source_event_id: extract.source_event_id,
                character_count: u32::try_from(extract.text.chars().count())
                    .map_err(|_| timeline_internal_error())?,
                untrusted_evidence: true,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    Ok(TimelineChunkBand {
        chunk_id: chunk.chunk_id,
        revision_id: chunk.revision_id,
        prior_revision_id: chunk.prior_revision_id,
        supersedes_revision_id: chunk.supersedes_revision_id,
        start: chunk.window.start,
        end: chunk.window.end,
        generated_at: chunk.generated_at,
        display_timezone: chunk.display_timezone,
        aggregator_version: chunk.aggregator_version,
        input_digest: chunk.input_digest,
        store_generation: chunk.store_generation,
        finalization_cadence_seconds: chunk.finalization_cadence_seconds,
        evidence_seconds: chunk.evidence_seconds,
        presence_seconds: chunk.presence_seconds,
        duration_estimates: chunk.duration_estimates,
        transitions: chunk.transitions,
        extracts,
        gaps: chunk.gaps,
        supporting_event_ids: chunk.supporting_event_ids,
        late_input: chunk.late_input,
    })
}

fn timeline_search_hit(hit: SearchHit) -> Result<TimelineSearchHit, FfiError> {
    let event_id = hit.event.event_id;
    let observed_at = hit.event.observed_at;
    let QueryEventPayload::ObservationAttempt(attempt) = hit.event.payload else {
        return Err(timeline_internal_error());
    };
    let QueryObservationContent::Captured { context, .. } = attempt.content else {
        return Err(timeline_internal_error());
    };
    let snippet = hit.snippet.ok_or_else(timeline_internal_error)?;
    Ok(TimelineSearchHit {
        event_id,
        observed_at,
        context,
        evidence_state: attempt.evidence_state,
        presence_state: attempt.presence_state,
        ocr_state: attempt.ocr_state,
        snippet: TimelineSearchSnippet {
            text: snippet.text,
            highlights: snippet
                .highlights
                .into_iter()
                .map(|range| TimelineHighlightRange {
                    start: range.start,
                    length: range.length,
                })
                .collect(),
        },
        untrusted_evidence: true,
    })
}

fn app_query_provenance(
    source_event_ids: impl IntoIterator<Item = EventId>,
    source_chunk_revision_ids: impl IntoIterator<Item = ChunkRevisionId>,
) -> FactualReportProvenance {
    let source_event_ids = source_event_ids.into_iter().collect::<BTreeSet<_>>();
    let source_chunk_revision_ids = source_chunk_revision_ids
        .into_iter()
        .collect::<BTreeSet<_>>();
    FactualReportProvenance {
        query_engine_version: env!("CARGO_PKG_VERSION"),
        projection_build_id: chronicle_store::STORE_BUILD_ID,
        sqlite_version: chronicle_store::SQLITE_BUNDLED_VERSION,
        sqlite_source_id: chronicle_store::SQLITE_BUNDLED_SOURCE_ID,
        source_event_ids: source_event_ids.into_iter().collect(),
        source_chunk_revision_ids: source_chunk_revision_ids.into_iter().collect(),
    }
}

fn analysis_provenance(artifacts: &[QueryArtifact]) -> AnalysisProvenance {
    let source_event_ids = artifacts
        .iter()
        .flat_map(|artifact| artifact.evidence.event_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let source_chunk_ids = artifacts
        .iter()
        .flat_map(|artifact| artifact.evidence.chunk_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let source_artifact_revision_ids = artifacts
        .iter()
        .map(|artifact| artifact.revision_id.clone())
        .collect::<BTreeSet<_>>();
    AnalysisProvenance {
        query_engine_version: env!("CARGO_PKG_VERSION"),
        projection_build_id: chronicle_store::STORE_BUILD_ID,
        sqlite_version: chronicle_store::SQLITE_BUNDLED_VERSION,
        sqlite_source_id: chronicle_store::SQLITE_BUNDLED_SOURCE_ID,
        source_event_ids: source_event_ids.into_iter().collect(),
        source_chunk_ids: source_chunk_ids.into_iter().collect(),
        source_artifact_revision_ids: source_artifact_revision_ids.into_iter().collect(),
    }
}

fn serialize_bounded_timeline<T: Serialize>(
    snapshot: T,
    max_bytes: usize,
    code: &'static str,
) -> Result<Value, FfiError> {
    let value = serialize_value(snapshot)?;
    let size = serde_json::to_vec(&value)
        .map_err(|_| timeline_internal_error())?
        .len();
    if size > max_bytes {
        return Err(FfiError::new(
            ChronicleStatus::TooLarge,
            code,
            "timeline response exceeds its app byte budget; narrow the range or page",
        ));
    }
    Ok(value)
}

fn timeline_internal_error() -> FfiError {
    FfiError::new(
        ChronicleStatus::Internal,
        "timeline-inconsistent",
        "Chronicle could not reconcile the timeline snapshot",
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DisclosureGrantMutation {
    Installed,
    AlreadyInstalled,
    Revoked,
    AlreadyRevoked,
}

#[derive(Debug, Serialize)]
struct DisclosureGrantMutationResponse {
    mutation: DisclosureGrantMutation,
    grant: DisclosureGrant,
}

#[derive(Debug, Deserialize)]
struct IngestEnvelope {
    schema_version: String,
    now: DateTime<Utc>,
    cadence: Option<FfiCadenceStamp>,
    event: Value,
    completion: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct FfiCadenceStamp {
    boot_sequence: String,
    monotonic_tick: u64,
    #[serde(default)]
    execution_generation: Option<u64>,
}

impl From<FfiCadenceStamp> for CadenceStamp {
    fn from(value: FfiCadenceStamp) -> Self {
        // Recognize the app-private linearization generation at the ABI boundary.
        // Cadence replay remains keyed by boot sequence and monotonic tick, while
        // older non-app fixtures remain forward-compatible without this field.
        let _execution_generation = value.execution_generation;
        Self {
            boot_sequence: value.boot_sequence,
            monotonic_tick: value.monotonic_tick,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ImageReadRequest {
    schema_version: String,
    artifact_id: ImageArtifactId,
    store_generation: u64,
    max_bytes: u64,
}

fn parse_request<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, FfiError> {
    parse_versioned(json).map_err(|error| {
        FfiError::new(
            ChronicleStatus::Contract,
            "contract-error",
            format!("invalid versioned request: {error}"),
        )
    })
}

fn parse_event(value: &Value) -> Result<EventEnvelope, FfiError> {
    let encoded = serde_json::to_string(value).map_err(|_| {
        FfiError::new(
            ChronicleStatus::Contract,
            "event-contract-error",
            "event could not be encoded for validation",
        )
    })?;
    EventEnvelope::parse(&encoded).map_err(|error| {
        FfiError::new(
            ChronicleStatus::Contract,
            "event-contract-error",
            format!("invalid event: {error}"),
        )
    })
}

impl CoreHandle {
    fn open(request: OpenRequest) -> Result<Self, FfiError> {
        validate_schema_version(&request.schema_version).map_err(|message| {
            FfiError::new(ChronicleStatus::Contract, "schema-mismatch", message)
        })?;
        let path = PathBuf::from(&request.application_support_path);
        if !path.is_absolute() || request.application_support_path.is_empty() {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "invalid-managed-root",
                "application support path must be absolute",
            ));
        }
        let chunker = ChunkerConfig {
            aggregator_version: request.aggregator_version,
            max_cadence_seconds: request.max_cadence_seconds,
        };
        chunker.validate().map_err(FfiError::from)?;
        let root = ManagedRoot::initialize(path).map_err(FfiError::from)?;
        let capture_owner = LockManager::new(root.clone(), std::time::Duration::from_secs(2))
            .try_capture_owner()
            .map_err(FfiError::from)?;
        let sqlite = SqliteStore::open(root.clone()).map_err(FfiError::from)?;
        let opened_generation = StoreGeneration::load(&root)
            .map_err(FfiError::from)?
            .generation;
        let service = SharedService::open(root.clone(), sqlite.clone()).map_err(FfiError::from)?;
        let coordinator = RecordingCoordinator::open_at(root.clone(), chunker, request.now)
            .map_err(FfiError::from)?;
        Ok(Self {
            _capture_owner: capture_owner,
            root,
            sqlite,
            service,
            coordinator,
            opened_generation,
        })
    }

    fn ensure_generation(&self, expected: u64) -> Result<(), FfiError> {
        let actual = StoreGeneration::load(&self.root)
            .map_err(FfiError::from)?
            .generation;
        if expected != self.opened_generation || expected != actual {
            return Err(FfiError::stale(expected, actual));
        }
        Ok(())
    }

    fn call(&mut self, request: CallRequest) -> Result<Value, FfiError> {
        validate_schema_version(&request.schema_version).map_err(|message| {
            FfiError::new(ChronicleStatus::Contract, "schema-mismatch", message)
        })?;
        match (request.request, request.control) {
            (Some(shared), None) => serialize_value(
                self.service
                    .execute(shared, request.now)
                    .map_err(FfiError::from)?,
            ),
            (None, Some(control)) => self.execute_control(control, request.now),
            (None, None) | (Some(_), Some(_)) => Err(FfiError::new(
                ChronicleStatus::Contract,
                "invalid-call-envelope",
                "call request must contain exactly one shared request or app control",
            )),
        }
    }

    fn execute_control(
        &mut self,
        control: AppControl,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        match control {
            AppControl::RuntimeState => serialize_value(
                self.coordinator
                    .runtime_state(now)
                    .map_err(FfiError::from)?,
            ),
            AppControl::StorageHealth => serialize_value(
                self.coordinator
                    .screenshot_storage_health()
                    .map_err(FfiError::from)?,
            ),
            AppControl::FactualReport { range } => self.factual_report(range, now),
            AppControl::TimelinePage {
                stable_cutoff,
                snapshot_token,
                filter,
                page,
            } => self.timeline_page(stable_cutoff, snapshot_token, filter, page, now),
            AppControl::TimelineSearch {
                stable_cutoff,
                snapshot_token,
                filter,
                query,
                page,
            } => self.timeline_search(stable_cutoff, snapshot_token, filter, query, page, now),
            AppControl::TimelineChunkDetail {
                snapshot_token,
                revision_id,
                chunk_id,
            } => self.timeline_chunk_detail(snapshot_token, revision_id, chunk_id, now),
            AppControl::TimelineEventDetail {
                snapshot_token,
                event_id,
            } => self.timeline_event_detail(snapshot_token, event_id, now),
            AppControl::AnalysisPage {
                stable_cutoff,
                snapshot_token,
                range,
                page,
            } => self.analysis_page(stable_cutoff, snapshot_token, range, page, now),
            AppControl::AnalysisDetail {
                snapshot_token,
                artifact_id,
                revision_id,
            } => self.analysis_detail(snapshot_token, artifact_id, revision_id, now),
            AppControl::SetRecordingPreference { enabled } => self
                .coordinator
                .set_recording_preference(enabled)
                .map(|()| json!({ "recording_preference": enabled }))
                .map_err(FfiError::from),
            AppControl::SetCadence { cadence } => self
                .coordinator
                .set_cadence(cadence)
                .map(|()| json!({ "cadence": cadence }))
                .map_err(FfiError::from),
            AppControl::SetScreenshotRetention { retention } => self
                .coordinator
                .set_screenshot_retention(retention)
                .map(|()| json!({ "screenshot_retention": retention }))
                .map_err(FfiError::from),
            AppControl::ConfigureStudy { start, end } => self
                .coordinator
                .configure_study(StudyBoundary { start, end })
                .map_err(FfiError::from)
                .and_then(serialize_value),
            AppControl::UsePersonalMode => self
                .coordinator
                .use_personal_mode()
                .map(|()| json!({ "mode": "personal" }))
                .map_err(FfiError::from),
            AppControl::ExtendStudy { new_end } => self
                .coordinator
                .extend_study(new_end, now)
                .map_err(FfiError::from)
                .and_then(serialize_value),
            AppControl::CaptureAdmission => self
                .coordinator
                .capture_admission(now)
                .map_err(FfiError::from)
                .and_then(serialize_value),
            AppControl::ReconcilePendingImages => self
                .coordinator
                .reconcile_pending_images(now)
                .map_err(FfiError::from)
                .and_then(serialize_value),
            AppControl::ReconcileRuntimeGap {
                reason,
                device_id,
                display_timezone,
            } => self
                .coordinator
                .reconcile_runtime_gap(RuntimeGapReconcileRequest {
                    reason,
                    device_id,
                    display_timezone,
                    now,
                })
                .map_err(FfiError::from)
                .and_then(serialize_value),
            AppControl::PrepareTermination { session_id } => self
                .coordinator
                .prepare_termination(&session_id, now)
                .map(|()| json!({ "prepared": true }))
                .map_err(FfiError::from),
            AppControl::StartupReconcile {
                session_id,
                device_id,
                display_timezone,
            } => self
                .coordinator
                .startup_reconcile(StartupReconcileRequest {
                    session_id,
                    device_id,
                    display_timezone,
                    now,
                })
                .map_err(FfiError::from)
                .and_then(serialize_value),
            AppControl::InstallDisclosureGrant { grant } => {
                self.install_disclosure_grant(grant, now)
            }
            AppControl::RevokeDisclosureGrant {
                grant_id,
                client_id,
                receipt_id,
            } => self.revoke_disclosure_grant(grant_id, client_id, receipt_id, now),
        }
    }

    fn factual_report(&self, range: UtcRange, now: DateTime<Utc>) -> Result<Value, FfiError> {
        validate_factual_report_range(&range, now)?;
        self.ensure_generation(self.opened_generation)?;
        let queries = StoreQueries::new(self.sqlite.clone())
            .snapshot()
            .map_err(FfiError::from)?;
        let report = FactualStatistics::new(queries)
            .range(&range)
            .map_err(FfiError::from)?;
        let snapshot = build_factual_report_snapshot(range, now, self.opened_generation, report)?;
        serialize_bounded_factual_report(snapshot, MAX_FACTUAL_REPORT_RESPONSE_BYTES)
    }

    fn timeline_page(
        &self,
        stable_cutoff: DateTime<Utc>,
        snapshot_token: Option<String>,
        filter: TimelineFilter,
        page: PageRequest,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        validate_timeline_filter(&filter, stable_cutoff, now)?;
        validate_timeline_page(&page, "chunk")?;
        self.ensure_generation(self.opened_generation)?;
        let queries = StoreQueries::new(self.sqlite.clone())
            .snapshot()
            .map_err(FfiError::from)?;
        let scope_digest = timeline_scope_digest("timeline-page", stable_cutoff, &filter, None)?;
        let (snapshot, encoded_token) = resolve_timeline_snapshot(
            snapshot_token.as_deref(),
            stable_cutoff,
            &scope_digest,
            self.opened_generation,
            &queries,
            page.cursor.is_some(),
        )?;
        let (activity_filter, include_missing_observation) = timeline_activity_filter(&filter);
        let report = FactualStatistics::new(queries.clone())
            .range_at_cutoff(&filter.range, stable_cutoff, snapshot.high_water())
            .map_err(FfiError::from)?;
        let (chunks, truncated) = queries
            .chunk_page_at_cutoff(
                &activity_filter,
                stable_cutoff,
                snapshot.high_water(),
                include_missing_observation,
                page.cursor.as_deref(),
                page.limit,
            )
            .map_err(FfiError::from)?;
        let next_cursor = truncated
            .then(|| chunks.last().map(|chunk| chunk.chunk_id.to_string()))
            .flatten();
        let source_event_ids = report
            .activity_chunks
            .iter()
            .flat_map(|chunk| chunk.supporting_event_ids.iter().cloned())
            .collect::<Vec<_>>();
        let domain_context_available = report.activity_chunks.iter().any(|chunk| {
            chunk
                .duration_estimates
                .iter()
                .any(|estimate| estimate.dimension == DimensionKind::AuthorizedDomain)
        });
        let provenance = app_query_provenance(
            source_event_ids,
            report.source_chunk_revision_ids.iter().cloned(),
        );
        let chunks = chunks
            .into_iter()
            .map(timeline_band)
            .collect::<Result<Vec<_>, _>>()?;
        let returned_items = u32::try_from(chunks.len()).map_err(|_| timeline_internal_error())?;
        serialize_bounded_timeline(
            TimelinePageSnapshot {
                schema_version: ABI_SCHEMA_VERSION,
                generated_at: now,
                stable_cutoff,
                snapshot_token: encoded_token,
                store_generation: self.opened_generation,
                filter,
                coverage: report.coverage,
                chunks,
                page: PageInfo {
                    next_cursor,
                    returned_items,
                    truncated,
                },
                domain_context_available,
                provenance,
            },
            MAX_TIMELINE_PAGE_RESPONSE_BYTES,
            "timeline-page-too-large",
        )
    }

    fn timeline_search(
        &self,
        stable_cutoff: DateTime<Utc>,
        snapshot_token: Option<String>,
        filter: TimelineFilter,
        query: String,
        page: PageRequest,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        validate_timeline_filter(&filter, stable_cutoff, now)?;
        validate_timeline_page(&page, "event")?;
        self.ensure_generation(self.opened_generation)?;
        let queries = StoreQueries::new(self.sqlite.clone())
            .snapshot()
            .map_err(FfiError::from)?;
        let scope_digest =
            timeline_scope_digest("timeline-search", stable_cutoff, &filter, Some(&query))?;
        let (snapshot, encoded_token) = resolve_timeline_snapshot(
            snapshot_token.as_deref(),
            stable_cutoff,
            &scope_digest,
            self.opened_generation,
            &queries,
            page.cursor.is_some(),
        )?;
        let (activity_filter, include_missing_observation) = timeline_activity_filter(&filter);
        let report = FactualStatistics::new(queries.clone())
            .range_at_cutoff(&filter.range, stable_cutoff, snapshot.high_water())
            .map_err(FfiError::from)?;
        let search_page =
            if include_missing_observation && activity_filter.evidence_states.is_empty() {
                chronicle_store::SearchPage {
                    hits: Vec::new(),
                    page: PageInfo {
                        next_cursor: None,
                        returned_items: 0,
                        truncated: false,
                    },
                }
            } else {
                ActivitySearch::from_queries(queries)
                    .search_at_cutoff(
                        &activity_filter,
                        &query,
                        true,
                        &page,
                        stable_cutoff,
                        snapshot.event_rowid_high_water,
                    )
                    .map_err(FfiError::from)?
            };
        let source_event_ids = search_page
            .hits
            .iter()
            .map(|hit| hit.event.event_id.clone())
            .collect::<Vec<_>>();
        let provenance = app_query_provenance(
            source_event_ids,
            report.source_chunk_revision_ids.iter().cloned(),
        );
        let hits = search_page
            .hits
            .into_iter()
            .map(timeline_search_hit)
            .collect::<Result<Vec<_>, _>>()?;
        serialize_bounded_timeline(
            TimelineSearchSnapshot {
                schema_version: ABI_SCHEMA_VERSION,
                generated_at: now,
                stable_cutoff,
                snapshot_token: encoded_token,
                store_generation: self.opened_generation,
                filter,
                coverage: report.coverage,
                query,
                hits,
                page: search_page.page,
                provenance,
            },
            MAX_TIMELINE_SEARCH_RESPONSE_BYTES,
            "timeline-search-too-large",
        )
    }

    fn timeline_chunk_detail(
        &self,
        snapshot_token: String,
        revision_id: Option<ChunkRevisionId>,
        chunk_id: Option<ChunkId>,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        if revision_id.is_some() == chunk_id.is_some() {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "invalid-chunk-detail-selector",
                "timeline chunk detail requires exactly one revision_id or chunk_id",
            ));
        }
        self.ensure_generation(self.opened_generation)?;
        let snapshot = decode_snapshot_token(&snapshot_token)?;
        validate_snapshot_token(
            &snapshot,
            snapshot.stable_cutoff,
            None,
            self.opened_generation,
        )?;
        if snapshot.stable_cutoff > now {
            return Err(invalid_snapshot_token());
        }
        let queries = StoreQueries::new(self.sqlite.clone())
            .snapshot()
            .map_err(FfiError::from)?;
        // Detail navigation is intentionally app-private and exact-ID based, so
        // it does not reapply the originating list/search scope digest. The
        // immutable projection boundary still applies, and no shared/MCP
        // operation maps to these app controls.
        ensure_snapshot_projection_identity(&queries, &snapshot)?;
        let chunk = match (revision_id, chunk_id) {
            (Some(revision_id), None) => queries.chunk_revision_at_snapshot(
                &revision_id,
                snapshot.stable_cutoff,
                snapshot.high_water(),
            ),
            (None, Some(chunk_id)) => {
                queries.chunk_at_cutoff(&chunk_id, snapshot.stable_cutoff, snapshot.high_water())
            }
            (None, None) | (Some(_), Some(_)) => {
                return Err(FfiError::new(
                    ChronicleStatus::Contract,
                    "invalid-chunk-detail-selector",
                    "timeline chunk detail requires exactly one revision_id or chunk_id",
                ));
            }
        }
        .map_err(FfiError::from)?
        .ok_or_else(|| {
            FfiError::new(
                ChronicleStatus::NotFound,
                "not-found",
                "requested chunk revision is not part of this timeline snapshot",
            )
        })?;
        let provenance = app_query_provenance(
            chunk.supporting_event_ids.iter().cloned(),
            [chunk.revision_id.clone()],
        );
        serialize_bounded_timeline(
            TimelineChunkDetailSnapshot {
                schema_version: ABI_SCHEMA_VERSION,
                generated_at: now,
                stable_cutoff: snapshot.stable_cutoff,
                snapshot_token,
                store_generation: self.opened_generation,
                chunk,
                provenance,
            },
            MAX_TIMELINE_DETAIL_RESPONSE_BYTES,
            "timeline-chunk-detail-too-large",
        )
    }

    fn timeline_event_detail(
        &self,
        snapshot_token: String,
        event_id: EventId,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        self.ensure_generation(self.opened_generation)?;
        let snapshot = decode_snapshot_token(&snapshot_token)?;
        validate_snapshot_token(
            &snapshot,
            snapshot.stable_cutoff,
            None,
            self.opened_generation,
        )?;
        if snapshot.stable_cutoff > now {
            return Err(invalid_snapshot_token());
        }
        let queries = StoreQueries::new(self.sqlite.clone())
            .snapshot()
            .map_err(FfiError::from)?;
        // See chunk detail above: app navigation may follow a supporting event
        // outside the visible filter, but never beyond this token's boundary.
        ensure_snapshot_projection_identity(&queries, &snapshot)?;
        let event = queries
            .event_at_snapshot(
                &event_id,
                true,
                snapshot.stable_cutoff,
                snapshot.high_water(),
            )
            .map_err(FfiError::from)?
            .ok_or_else(|| {
                FfiError::new(
                    ChronicleStatus::NotFound,
                    "not-found",
                    "requested event is not part of this timeline snapshot",
                )
            })?;
        let provenance = app_query_provenance([event.event_id.clone()], Vec::new());
        serialize_bounded_timeline(
            TimelineEventDetailSnapshot {
                schema_version: ABI_SCHEMA_VERSION,
                generated_at: now,
                stable_cutoff: snapshot.stable_cutoff,
                snapshot_token,
                store_generation: self.opened_generation,
                event,
                provenance,
            },
            MAX_TIMELINE_DETAIL_RESPONSE_BYTES,
            "timeline-event-detail-too-large",
        )
    }

    fn analysis_page(
        &self,
        stable_cutoff: DateTime<Utc>,
        snapshot_token: Option<String>,
        range: UtcRange,
        page: PageRequest,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        validate_timeline_filter(
            &TimelineFilter {
                range: range.clone(),
                application_bundle_id: None,
                window_text: None,
                authorized_domain: None,
                coverage_states: Vec::new(),
            },
            stable_cutoff,
            now,
        )?;
        validate_timeline_page(&page, "artifact")?;
        self.ensure_generation(self.opened_generation)?;
        let queries = StoreQueries::new(self.sqlite.clone())
            .snapshot()
            .map_err(FfiError::from)?;
        let scope_digest = analysis_scope_digest(stable_cutoff, &range)?;
        let (snapshot, encoded_token) = resolve_timeline_snapshot(
            snapshot_token.as_deref(),
            stable_cutoff,
            &scope_digest,
            self.opened_generation,
            &queries,
            page.cursor.is_some(),
        )?;
        let (artifacts, truncated) = queries
            .artifact_page_at_cutoff(
                &range,
                stable_cutoff,
                snapshot.high_water(),
                page.cursor.as_deref(),
                page.limit,
            )
            .map_err(FfiError::from)?;
        for artifact in &artifacts {
            artifact
                .validate_public()
                .map_err(|_| timeline_internal_error())?;
        }
        let next_cursor = truncated
            .then(|| {
                artifacts
                    .last()
                    .map(|artifact| artifact.artifact_id.to_string())
            })
            .flatten();
        let returned_items =
            u32::try_from(artifacts.len()).map_err(|_| timeline_internal_error())?;
        let provenance = analysis_provenance(&artifacts);
        serialize_bounded_timeline(
            AnalysisPageSnapshot {
                schema_version: ABI_SCHEMA_VERSION,
                generated_at: now,
                stable_cutoff,
                snapshot_token: encoded_token,
                store_generation: self.opened_generation,
                range,
                artifacts,
                page: PageInfo {
                    next_cursor,
                    returned_items,
                    truncated,
                },
                provenance,
            },
            MAX_ANALYSIS_RESPONSE_BYTES,
            "analysis-page-too-large",
        )
    }

    fn analysis_detail(
        &self,
        snapshot_token: String,
        artifact_id: ArtifactId,
        revision_id: Option<ArtifactRevisionId>,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        self.ensure_generation(self.opened_generation)?;
        let snapshot = decode_snapshot_token(&snapshot_token)?;
        validate_snapshot_token(
            &snapshot,
            snapshot.stable_cutoff,
            None,
            self.opened_generation,
        )?;
        if snapshot.stable_cutoff > now {
            return Err(invalid_snapshot_token());
        }
        let queries = StoreQueries::new(self.sqlite.clone())
            .snapshot()
            .map_err(FfiError::from)?;
        ensure_snapshot_projection_identity(&queries, &snapshot)?;
        let artifact = queries
            .artifact_at_snapshot(
                &artifact_id,
                revision_id.as_ref(),
                snapshot.stable_cutoff,
                snapshot.high_water(),
            )
            .map_err(FfiError::from)?
            .ok_or_else(|| {
                FfiError::new(
                    ChronicleStatus::NotFound,
                    "not-found",
                    "requested analysis artifact is not part of this snapshot",
                )
            })?;
        artifact
            .validate_public()
            .map_err(|_| timeline_internal_error())?;
        let provenance = analysis_provenance(std::slice::from_ref(&artifact));
        serialize_bounded_timeline(
            AnalysisDetailSnapshot {
                schema_version: ABI_SCHEMA_VERSION,
                generated_at: now,
                stable_cutoff: snapshot.stable_cutoff,
                snapshot_token,
                store_generation: self.opened_generation,
                artifact,
                provenance,
            },
            MAX_ANALYSIS_RESPONSE_BYTES,
            "analysis-detail-too-large",
        )
    }

    fn install_disclosure_grant(
        &self,
        grant: DisclosureGrant,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        grant.validate().map_err(|_| {
            FfiError::new(
                ChronicleStatus::Contract,
                "invalid-disclosure-grant",
                "disclosure grant violates the Chronicle contract",
            )
        })?;
        if !grant.is_active_at(now) || grant.disclosed_bytes != 0 {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "inactive-disclosure-grant",
                "a new disclosure grant must be active with no prior disclosure",
            ));
        }

        match self.service.grant(&grant.grant_id) {
            Ok(existing) if existing == grant => serialize_value(DisclosureGrantMutationResponse {
                mutation: DisclosureGrantMutation::AlreadyInstalled,
                grant: existing,
            }),
            Ok(_) => Err(FfiError::new(
                ChronicleStatus::Contract,
                "disclosure-grant-conflict",
                "an existing disclosure grant has a different contract",
            )),
            Err(SharedServiceError::GrantNotFound) => {
                self.service
                    .install_grant(grant.clone())
                    .map_err(FfiError::from)?;
                serialize_value(DisclosureGrantMutationResponse {
                    mutation: DisclosureGrantMutation::Installed,
                    grant,
                })
            }
            Err(error) => Err(FfiError::from(error)),
        }
    }

    fn revoke_disclosure_grant(
        &self,
        grant_id: GrantId,
        client_id: ClientId,
        receipt_id: ReceiptId,
        now: DateTime<Utc>,
    ) -> Result<Value, FfiError> {
        let existing = self.service.grant(&grant_id).map_err(FfiError::from)?;
        if existing.client_id != client_id || existing.receipt_id != receipt_id {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "disclosure-grant-identity-mismatch",
                "disclosure grant identity does not match the installed receipt",
            ));
        }
        if existing.state == GrantState::Revoked {
            return serialize_value(DisclosureGrantMutationResponse {
                mutation: DisclosureGrantMutation::AlreadyRevoked,
                grant: existing,
            });
        }

        self.service
            .revoke_grant(&grant_id, now)
            .map_err(FfiError::from)?;
        let revoked = self.service.grant(&grant_id).map_err(FfiError::from)?;
        serialize_value(DisclosureGrantMutationResponse {
            mutation: DisclosureGrantMutation::Revoked,
            grant: revoked,
        })
    }

    fn ingest(
        &mut self,
        request: IngestEnvelope,
        image: Option<Vec<u8>>,
    ) -> Result<Value, FfiError> {
        validate_schema_version(&request.schema_version).map_err(|message| {
            FfiError::new(ChronicleStatus::Contract, "schema-mismatch", message)
        })?;
        let event = parse_event(&request.event)?;
        let cadence = request.cadence.map(CadenceStamp::from);
        let outcome = match image {
            None => {
                if request.completion.is_some() {
                    return Err(FfiError::new(
                        ChronicleStatus::Contract,
                        "unexpected-completion",
                        "non-image ingest cannot include an image lifecycle completion",
                    ));
                }
                self.coordinator
                    .ingest(IngestRequest { event, cadence }, request.now)
                    .map_err(FfiError::from)?
            }
            Some(bytes) => {
                let completion = request.completion.as_ref().ok_or_else(|| {
                    FfiError::new(
                        ChronicleStatus::Contract,
                        "missing-completion",
                        "image ingest requires a lifecycle completion event",
                    )
                })?;
                let completion = parse_event(completion)?;
                let cadence = cadence.ok_or_else(|| {
                    FfiError::new(
                        ChronicleStatus::Contract,
                        "missing-cadence",
                        "image ingest requires an explicit cadence stamp",
                    )
                })?;
                let retained = self
                    .coordinator
                    .retain_screenshot(
                        &event,
                        &bytes,
                        &completion,
                        cadence,
                        request.now,
                        FaultInjector::none(),
                    )
                    .map_err(FfiError::from)?;
                retained.ingest
            }
        };
        Ok(json!({
            "store_generation": self.opened_generation,
            "acknowledgement": outcome.acknowledgement,
            "projection": outcome.projection,
            "health": outcome.health,
            "aggregation_ran": outcome.aggregation.is_some(),
        }))
    }

    fn image_read(&self, request: ImageReadRequest) -> Result<Vec<u8>, FfiError> {
        validate_schema_version(&request.schema_version).map_err(|message| {
            FfiError::new(ChronicleStatus::Contract, "schema-mismatch", message)
        })?;
        if request.max_bytes == 0 || request.max_bytes > MAX_ENCODED_IMAGE_BYTES as u64 {
            return Err(FfiError::new(
                ChronicleStatus::TooLarge,
                "image-size-bound",
                "image read max_bytes must be between 1 byte and 4 MiB",
            ));
        }
        self.ensure_generation(request.store_generation)?;
        let locks = LockManager::new(self.root.clone(), std::time::Duration::from_secs(2));
        let shared = locks.shared_request().map_err(FfiError::from)?;
        let _screenshots = shared.screenshots().map_err(FfiError::from)?;
        let _snapshot = locks.query_snapshot().map_err(FfiError::from)?;
        self.ensure_generation(request.store_generation)?;

        let connection = self.sqlite.connection().map_err(FfiError::from)?;
        let projected: Option<(String, String)> = connection
            .query_row(
                "SELECT lifecycle.state, events.body_json
                 FROM screenshot_lifecycle lifecycle
                 JOIN events ON events.event_id=lifecycle.source_event_id
                 WHERE lifecycle.artifact_id=?1",
                [request.artifact_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::from)
            .map_err(FfiError::from)?;
        let (state, source_json) = projected.ok_or_else(|| {
            FfiError::new(
                ChronicleStatus::NotFound,
                "image-not-found",
                "screenshot artifact is not present in the projected store",
            )
        })?;
        let state = serde_json::from_value::<ScreenshotProjectedState>(Value::String(state))
            .map_err(|_| {
                FfiError::new(
                    ChronicleStatus::Internal,
                    "invalid-image-state",
                    "projected screenshot state is invalid",
                )
            })?;
        if state != ScreenshotProjectedState::Retained {
            return Err(FfiError::new(
                ChronicleStatus::NotRetained,
                "image-not-retained",
                "screenshot artifact is not currently retained",
            ));
        }
        let source = EventEnvelope::parse(&source_json).map_err(|_| {
            FfiError::new(
                ChronicleStatus::Internal,
                "invalid-image-source",
                "projected screenshot source is invalid",
            )
        })?;
        let image = match &source.payload {
            EventPayload::ObservationAttempt(attempt) => match &attempt.content {
                ObservationContent::Captured(content) => content.image.as_ref(),
                ObservationContent::Unchanged(_)
                | ObservationContent::Protected(_)
                | ObservationContent::NoEvidence(_) => None,
            },
            EventPayload::RecordingGap(_) | EventPayload::ScreenshotLifecycle(_) => None,
        }
        .filter(|image| image.artifact_id == request.artifact_id)
        .ok_or_else(|| {
            FfiError::new(
                ChronicleStatus::Internal,
                "invalid-image-source",
                "projected screenshot source has no matching image intent",
            )
        })?;
        let derived = format!(
            "screenshots/{}/{}.heic",
            source.recorded_at.format("%Y-%m-%d"),
            request.artifact_id
        );
        if image.managed_relative_path.as_str() != derived {
            return Err(FfiError::new(
                ChronicleStatus::Contract,
                "invalid-image-reference",
                "canonical screenshot reference violates managed derivation",
            ));
        }
        let mut file = self
            .root
            .open_file(&derived, false, false, false)
            .map_err(|error| match error {
                StoreError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => FfiError::new(
                    ChronicleStatus::NotFound,
                    "image-file-missing",
                    "retained screenshot file is missing",
                ),
                other => FfiError::from(other),
            })?;
        let length = file
            .metadata()
            .map_err(StoreError::from)
            .map_err(FfiError::from)?
            .len();
        if length == 0
            || length > request.max_bytes
            || length > u64::try_from(MAX_ENCODED_IMAGE_BYTES).unwrap_or(u64::MAX)
        {
            return Err(FfiError::new(
                ChronicleStatus::TooLarge,
                "image-too-large",
                "retained screenshot exceeds the requested byte bound",
            ));
        }
        let capacity = usize::try_from(length).map_err(|_| {
            FfiError::new(
                ChronicleStatus::TooLarge,
                "image-too-large",
                "retained screenshot size is unsupported",
            )
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        file.read_to_end(&mut bytes)
            .map_err(StoreError::from)
            .map_err(FfiError::from)?;
        if bytes.len() != capacity {
            return Err(FfiError::new(
                ChronicleStatus::Io,
                "image-size-changed",
                "retained screenshot changed while it was being read",
            ));
        }
        Ok(bytes)
    }
}

/// Opens one serialized application core handle.
///
/// # Safety
///
/// Input must be readable for `request_len`; outputs must point to writable C
/// ABI values for the duration of this synchronous call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chronicle_open(
    request_ptr: *const u8,
    request_len: usize,
    out_handle: *mut u64,
    out_response: *mut ChronicleBuffer,
) -> u32 {
    if out_handle.is_null() {
        // SAFETY: json_boundary validates and initializes out_response.
        return json_boundary(out_response, || {
            Err(FfiError::new(
                ChronicleStatus::InvalidArgument,
                "null-handle-output",
                "output handle pointer is required",
            ))
        });
    }
    // SAFETY: caller provides a writable handle pointer by the C contract.
    unsafe { ptr::write(out_handle, 0) };
    // SAFETY: json_boundary validates and initializes out_response.
    json_boundary(out_response, || {
        // SAFETY: this ABI entry owns validation and immediate copying.
        let bytes = unsafe {
            copy_input(
                request_ptr,
                request_len,
                MAX_OPEN_REQUEST_BYTES,
                "open request",
            )
        }?;
        let request = parse_request::<OpenRequest>(&utf8_json(bytes)?)?;
        let core = CoreHandle::open(request)?;
        let generation = core.opened_generation;
        let handle = register_handle(core)?;
        // SAFETY: out_handle was validated above and remains caller-owned.
        unsafe { ptr::write(out_handle, handle) };
        Ok(json!({ "store_generation": generation }))
    })
}

/// Executes exactly one bounded SharedService request or app-only control at
/// the explicit request time.
///
/// # Safety
///
/// Input must be readable for `request_len` and `out_response` must be writable
/// for the duration of this synchronous call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chronicle_call(
    handle: u64,
    request_ptr: *const u8,
    request_len: usize,
    out_response: *mut ChronicleBuffer,
) -> u32 {
    // SAFETY: json_boundary validates and initializes out_response.
    json_boundary(out_response, || {
        // SAFETY: this ABI entry owns validation and immediate copying.
        let bytes = unsafe {
            copy_input(
                request_ptr,
                request_len,
                MAX_CALL_REQUEST_BYTES,
                "call request",
            )
        }?;
        let request = parse_request::<CallRequest>(&utf8_json(bytes)?)?;
        with_handle(handle, |core| core.call(request))
    })
}

/// Ingests one factual event, optionally with one bounded encoded image copy.
///
/// # Safety
///
/// Request/image inputs must be readable for their supplied lengths and
/// `out_response` must be writable for this synchronous call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chronicle_ingest(
    handle: u64,
    request_ptr: *const u8,
    request_len: usize,
    encoded_image_ptr: *const u8,
    encoded_image_len: usize,
    out_response: *mut ChronicleBuffer,
) -> u32 {
    // SAFETY: json_boundary validates and initializes out_response.
    json_boundary(out_response, || {
        // SAFETY: this ABI entry owns validation and immediate copying.
        let request_bytes = unsafe {
            copy_input(
                request_ptr,
                request_len,
                MAX_INGEST_REQUEST_BYTES,
                "ingest request",
            )
        }?;
        let image = match (encoded_image_ptr.is_null(), encoded_image_len) {
            (true, 0) => None,
            (false, 1..=MAX_ENCODED_IMAGE_BYTES) => {
                // SAFETY: the validated non-null caller allocation remains
                // readable for encoded_image_len during this call.
                Some(
                    unsafe { std::slice::from_raw_parts(encoded_image_ptr, encoded_image_len) }
                        .to_vec(),
                )
            }
            (false, 0) => {
                return Err(FfiError::new(
                    ChronicleStatus::InvalidArgument,
                    "empty-image-pointer",
                    "zero-length image input must use a null pointer",
                ));
            }
            (true, _) => {
                return Err(FfiError::new(
                    ChronicleStatus::InvalidArgument,
                    "null-image-pointer",
                    "nonempty image input requires a valid pointer",
                ));
            }
            (false, _) => {
                return Err(FfiError::new(
                    ChronicleStatus::TooLarge,
                    "image-too-large",
                    "encoded image exceeds 4 MiB",
                ));
            }
        };
        let request = parse_request::<IngestEnvelope>(&utf8_json(request_bytes)?)?;
        with_handle(handle, |core| core.ingest(request, image))
    })
}

/// Reads one projected-retained managed image by opaque ID and generation.
///
/// # Safety
///
/// Input must be readable for `request_len` and `out_response` must be writable
/// for the duration of this synchronous call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chronicle_image_read(
    handle: u64,
    request_ptr: *const u8,
    request_len: usize,
    out_response: *mut ChronicleBuffer,
) -> u32 {
    if let Err(error) = unsafe { initialize_output(out_response) } {
        return error.status as u32;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: this ABI entry owns validation and immediate copying.
        let bytes = unsafe {
            copy_input(
                request_ptr,
                request_len,
                MAX_IMAGE_REQUEST_BYTES,
                "image read request",
            )
        }?;
        let request = parse_request::<ImageReadRequest>(&utf8_json(bytes)?)?;
        with_handle(handle, |core| core.image_read(request))
    }));
    let (status, buffer) = match result {
        Ok(Ok(bytes)) => (ChronicleStatus::Ok, store_buffer(bytes)),
        Ok(Err(error)) => (error.status, encode_value(&error.response())),
        Err(_) => {
            let error = FfiError::new(
                ChronicleStatus::Panic,
                "panic-contained",
                "Chronicle contained an internal panic at the ABI boundary",
            );
            (error.status, encode_value(&error.response()))
        }
    };
    match buffer.and_then(|buffer| {
        // SAFETY: initialize_output already validated the same pointer.
        unsafe { write_output(out_response, buffer) }
    }) {
        Ok(()) => status as u32,
        Err(error) => error.status as u32,
    }
}

/// Closes a handle token. Repeated close is a typed invalid-handle failure.
///
/// # Safety
///
/// `out_response` must point to a writable C ABI buffer value for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chronicle_close(handle: u64, out_response: *mut ChronicleBuffer) -> u32 {
    // SAFETY: json_boundary validates and initializes out_response.
    json_boundary(out_response, || {
        close_handle(handle)?;
        Ok(json!({ "closed": true }))
    })
}

/// Returns the versioned ABI/contract identity as an owned JSON buffer.
///
/// # Safety
///
/// `out_response` must point to a writable C ABI buffer value for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chronicle_schema_version(out_response: *mut ChronicleBuffer) -> u32 {
    // SAFETY: json_boundary validates and initializes out_response.
    json_boundary(out_response, || {
        Ok(json!({
            "abi_schema_version": ABI_SCHEMA_VERSION,
            "contract_schema_version": chronicle_domain::CONTRACT_VERSION,
        }))
    })
}

/// Frees one exact registry-owned output. Never reconstructs a caller pointer.
///
/// # Safety
///
/// `buffer` must point to a readable/writable C ABI buffer value. Its fields
/// may be stale or invalid; the registry validates them before freeing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chronicle_buffer_free(buffer: *mut ChronicleBuffer) -> u32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if buffer.is_null() {
            return ChronicleStatus::InvalidArgument;
        }
        // SAFETY: caller provides a readable/writable ChronicleBuffer by the C
        // contract. We inspect values only; allocation ownership stays registry-based.
        let supplied = unsafe { ptr::read(buffer) };
        if supplied.token == 0 || supplied.ptr.is_null() || supplied.len == 0 {
            return ChronicleStatus::InvalidBuffer;
        }
        let mut registry = lock_recover(buffers());
        let valid = registry
            .get(&supplied.token)
            .is_some_and(|owned| owned.as_ptr() == supplied.ptr && owned.len() == supplied.len);
        if !valid {
            return ChronicleStatus::InvalidBuffer;
        }
        let _owned = registry.remove(&supplied.token);
        drop(registry);
        // SAFETY: same caller-provided output pointer validated above.
        unsafe { ptr::write(buffer, ChronicleBuffer::EMPTY) };
        ChronicleStatus::Ok
    }));
    match result {
        Ok(status) => status as u32,
        Err(_) => ChronicleStatus::Panic as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronicle_domain::{
        ArtifactStatus, ArtifactType, AuthorIdentity, AuthorKind, ChunkRevision,
        DerivedArtifactRevision, DurationEstimate, EvidenceReferences, QueryResponse,
    };
    use chronicle_store::{ArtifactStore, CanonicalJournal, Projector, RecoveryManager};
    use std::sync::{Arc, Barrier};

    fn response_bytes(buffer: ChronicleBuffer) -> Vec<u8> {
        assert_ne!(buffer.token, 0);
        assert!(!buffer.ptr.is_null());
        // SAFETY: tests copy a live registry-owned buffer before freeing it.
        unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) }.to_vec()
    }

    fn free(buffer: &mut ChronicleBuffer) {
        // SAFETY: tests pass the exact buffer returned by this ABI.
        assert_eq!(
            unsafe { chronicle_buffer_free(buffer) },
            ChronicleStatus::Ok as u32
        );
    }

    fn open(temporary: &tempfile::TempDir) -> (u64, Value) {
        let (status, handle, value) = open_raw(temporary);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{value}");
        assert_ne!(handle, 0);
        (handle, value)
    }

    fn open_raw(temporary: &tempfile::TempDir) -> (u32, u64, Value) {
        let request = json!({
            "schema_version": "1.0",
            "application_support_path": temporary.path().join("store"),
            "now": "2026-07-13T09:00:00Z",
            "aggregator_version": "ffi-test-1",
            "max_cadence_seconds": 60,
        });
        let encoded = serde_json::to_vec(&request).expect("encode open request");
        let mut handle = 0;
        let mut response = ChronicleBuffer::EMPTY;
        // SAFETY: test pointers are valid for each synchronous call.
        let status =
            unsafe { chronicle_open(encoded.as_ptr(), encoded.len(), &mut handle, &mut response) };
        let value: Value =
            serde_json::from_slice(&response_bytes(response)).expect("decode open response");
        free(&mut response);
        (status, handle, value)
    }

    fn close(handle: u64) -> (u32, Value) {
        let mut response = ChronicleBuffer::EMPTY;
        // SAFETY: test output is valid.
        let status = unsafe { chronicle_close(handle, &mut response) };
        let value: Value =
            serde_json::from_slice(&response_bytes(response)).expect("decode close response");
        free(&mut response);
        (status, value)
    }

    fn call_raw(handle: u64, bytes: &[u8]) -> (u32, Value) {
        let mut response = ChronicleBuffer::EMPTY;
        // SAFETY: test pointers are valid for each synchronous call.
        let status = unsafe { chronicle_call(handle, bytes.as_ptr(), bytes.len(), &mut response) };
        let value: Value =
            serde_json::from_slice(&response_bytes(response)).expect("decode call response");
        free(&mut response);
        (status, value)
    }

    fn control(handle: u64, now: &str, control: Value) -> (u32, Value) {
        call_raw(
            handle,
            &serde_json::to_vec(&json!({
                "schema_version": "1.0",
                "now": now,
                "control": control,
            }))
            .expect("encode app control"),
        )
    }

    fn disclosure_grant(
        generation: u64,
        grant_id: &str,
        client_id: &str,
        receipt_id: &str,
    ) -> Value {
        json!({
            "schema_version": "1.0",
            "grant_id": grant_id,
            "client_id": client_id,
            "receipt_id": receipt_id,
            "time_scope": {
                "type": "rolling-horizon",
                "seconds": 86_400
            },
            "content_classes": ["metadata", "derived"],
            "created_at": "2026-07-13T09:00:00Z",
            "expires_at": "2026-07-20T09:00:00Z",
            "state": "active",
            "limits": {
                "max_page_items": 100,
                "max_response_bytes": 262_144,
                "max_cumulative_bytes": 1_048_576
            },
            "disclosed_bytes": 0,
            "store_generation": generation
        })
    }

    fn start_recording(handle: u64, session_id: &str) {
        for request in [
            json!({
                "type": "startup-reconcile",
                "session_id": session_id,
                "device_id": "dev-ffi-ingest",
                "display_timezone": "Europe/Zurich"
            }),
            json!({ "type": "set-recording-preference", "enabled": true }),
        ] {
            let (status, response) = control(handle, "2026-07-13T09:00:00Z", request);
            assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        }
    }

    fn ingest_raw(handle: u64, request: &Value, image: Option<&[u8]>) -> (u32, Value) {
        let bytes = serde_json::to_vec(request).expect("encode ingest");
        let (pointer, length) =
            image.map_or((ptr::null(), 0), |value| (value.as_ptr(), value.len()));
        let mut response = ChronicleBuffer::EMPTY;
        // SAFETY: test pointers are valid for each synchronous call.
        let status = unsafe {
            chronicle_ingest(
                handle,
                bytes.as_ptr(),
                bytes.len(),
                pointer,
                length,
                &mut response,
            )
        };
        let value: Value =
            serde_json::from_slice(&response_bytes(response)).expect("decode ingest response");
        free(&mut response);
        (status, value)
    }

    fn fixture_events() -> Vec<Value> {
        include_str!("../../../fixtures/synthetic/session-v1/events.jsonl")
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).expect("valid fixture event"))
            .collect()
    }

    fn seed_report_chunk(handle: u64, include_domain: bool) {
        let mut chunk = include_str!("../../../fixtures/synthetic/session-v1/chunks.jsonl")
            .lines()
            .last()
            .map(ChunkRevision::parse)
            .expect("fixture chunk line")
            .expect("valid fixture chunk");
        chunk.prior_revision_id = None;
        chunk.supersedes_revision_id = None;
        chunk.duration_estimates.push(DurationEstimate {
            dimension: DimensionKind::Window,
            key: "com.example.writer:Quarterly notes".to_owned(),
            label: "Quarterly notes".to_owned(),
            estimated_seconds: 60,
            supporting_event_ids: vec![
                EventId::new("evt-090015").expect("event ID"),
                EventId::new("evt-090045").expect("event ID"),
            ],
        });
        if include_domain {
            chunk.duration_estimates.push(DurationEstimate {
                dimension: DimensionKind::AuthorizedDomain,
                key: "example.test".to_owned(),
                label: "example.test".to_owned(),
                estimated_seconds: 30,
                supporting_event_ids: vec![EventId::new("evt-090015").expect("event ID")],
            });
        }
        chunk.validate().expect("augmented report chunk is valid");
        with_handle(handle, |core| {
            let record = CanonicalJournal::new(core.root.clone())
                .append_chunk(&chunk, FaultInjector::none())
                .map_err(FfiError::from)?;
            Projector::new(core.sqlite.clone())
                .project_record(&record, FaultInjector::none())
                .map_err(FfiError::from)
        })
        .expect("seed report chunk");
    }

    fn seed_timeline_fixture(handle: u64) {
        with_handle(handle, |core| {
            let journal = CanonicalJournal::new(core.root.clone());
            let projector = Projector::new(core.sqlite.clone());
            for value in fixture_events().into_iter().take(9) {
                let event = parse_event(&value)?;
                let record = journal
                    .append_event(&event, FaultInjector::none())
                    .map_err(FfiError::from)?;
                projector
                    .project_record(&record, FaultInjector::none())
                    .map_err(FfiError::from)?;
            }
            for line in include_str!("../../../fixtures/synthetic/session-v1/chunks.jsonl")
                .lines()
                .filter(|line| !line.is_empty())
            {
                let chunk = ChunkRevision::parse(line).map_err(|error| {
                    FfiError::new(
                        ChronicleStatus::Contract,
                        "fixture-error",
                        error.to_string(),
                    )
                })?;
                let record = journal
                    .append_chunk(&chunk, FaultInjector::none())
                    .map_err(FfiError::from)?;
                projector
                    .project_record(&record, FaultInjector::none())
                    .map_err(FfiError::from)?;
            }
            Ok(())
        })
        .expect("seed timeline fixture");
    }

    fn analysis_revision(
        revision_id: &str,
        prior_revision_id: Option<&str>,
        statement: &str,
    ) -> DerivedArtifactRevision {
        let prior_revision_id = prior_revision_id
            .map(|value| ArtifactRevisionId::new(value).expect("prior revision ID"));
        DerivedArtifactRevision {
            schema_version: "1.0".to_owned(),
            artifact_id: ArtifactId::new("analysis-work-pattern").expect("artifact ID"),
            revision_id: ArtifactRevisionId::new(revision_id).expect("revision ID"),
            prior_revision_id: prior_revision_id.clone(),
            expected_prior_revision_id: prior_revision_id,
            artifact_type: ArtifactType::Hypothesis,
            author: AuthorIdentity {
                kind: AuthorKind::Model,
                display_name: Some("Local analysis".to_owned()),
                client_id: None,
                model: Some("fixture-model".to_owned()),
            },
            created_at: "2026-07-13T09:04:00Z".parse().expect("created at"),
            status: ArtifactStatus::Draft,
            payload: json!({"statement": statement}),
            evidence: EvidenceReferences {
                event_ids: vec![EventId::new("evt-090015").expect("event ID")],
                chunk_ids: vec![ChunkId::new("chunk-20260713T0900Z").expect("chunk ID")],
            },
            confidence: Some(0.75),
            store_generation: 1,
        }
    }

    fn write_analysis_revision(handle: u64, revision: &DerivedArtifactRevision) {
        with_handle(handle, |core| {
            ArtifactStore::new(core.root.clone(), Projector::new(core.sqlite.clone()))
                .write_revision(revision, FaultInjector::none())
                .map_err(FfiError::from)
        })
        .expect("write analysis revision");
    }

    fn timeline_filter(coverage_states: Value) -> Value {
        json!({
            "range": {
                "start": "2026-07-13T09:00:00Z",
                "end": "2026-07-13T09:05:00Z"
            },
            "application_bundle_id": null,
            "window_text": null,
            "authorized_domain": null,
            "coverage_states": coverage_states
        })
    }

    fn timeline_page_control(
        handle: u64,
        snapshot_token: Option<&str>,
        cursor: Option<&str>,
        coverage_states: Value,
    ) -> (u32, Value) {
        control(
            handle,
            "2026-07-13T09:07:00Z",
            json!({
                "type": "timeline-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": snapshot_token,
                "filter": timeline_filter(coverage_states),
                "page": { "cursor": cursor, "limit": 10 }
            }),
        )
    }

    fn factual_report_control(handle: u64) -> (u32, Value) {
        control(
            handle,
            "2026-07-13T09:07:00Z",
            json!({
                "type": "factual-report",
                "range": {
                    "start": "2026-07-13T09:00:00Z",
                    "end": "2026-07-13T09:05:00Z"
                }
            }),
        )
    }

    fn image_request(artifact_id: &str, generation: u64, max_bytes: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": "1.0",
            "artifact_id": artifact_id,
            "store_generation": generation,
            "max_bytes": max_bytes,
        }))
        .expect("encode image request")
    }

    fn read_image(handle: u64, request: &[u8]) -> (u32, Vec<u8>) {
        let mut response = ChronicleBuffer::EMPTY;
        // SAFETY: test pointers are valid for each synchronous call.
        let status =
            unsafe { chronicle_image_read(handle, request.as_ptr(), request.len(), &mut response) };
        let bytes = response_bytes(response);
        free(&mut response);
        (status, bytes)
    }

    #[test]
    fn schema_buffer_is_owned_and_double_free_is_safe() {
        let mut buffer = ChronicleBuffer::EMPTY;
        // SAFETY: output pointer is valid.
        assert_eq!(
            unsafe { chronicle_schema_version(&mut buffer) },
            ChronicleStatus::Ok as u32
        );
        let mut stale_copy = buffer;
        let value: Value =
            serde_json::from_slice(&response_bytes(buffer)).expect("decode schema response");
        assert_eq!(value["result"]["abi_schema_version"], "1.0");
        free(&mut buffer);
        // SAFETY: stale token is deliberately tested; registry owns memory.
        assert_eq!(
            unsafe { chronicle_buffer_free(&mut stale_copy) },
            ChronicleStatus::InvalidBuffer as u32
        );
    }

    #[test]
    fn open_health_call_close_and_double_close_are_typed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        let request = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:01Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-health-ffi",
                "store_generation": generation,
                "operation": { "type": "health" }
            }
        });
        let (status, response) = call_raw(
            handle,
            &serde_json::to_vec(&request).expect("encode call request"),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        assert_eq!(response["result"]["request_id"], "req-health-ffi");

        let (status, response) = close(handle);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        let (status, response) = close(handle);
        assert_eq!(status, ChronicleStatus::InvalidHandle as u32);
        assert_eq!(response["error"]["code"], "invalid-handle");
    }

    #[test]
    fn app_open_owns_capture_until_close_and_reports_a_typed_conflict() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (first, _) = open(&temporary);

        let (status, second, response) = open_raw(&temporary);
        assert_eq!(status, ChronicleStatus::CaptureOwnerActive as u32);
        assert_eq!(second, 0);
        assert_eq!(response["error"]["code"], "capture-owner-active");

        assert_eq!(close(first).0, ChronicleStatus::Ok as u32);
        let (reopened, _) = open(&temporary);
        assert_eq!(close(reopened).0, ChronicleStatus::Ok as u32);
    }

    #[test]
    fn call_envelope_requires_exactly_one_shared_request_or_app_control() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        let shared_request = json!({
            "schema_version": "1.0",
            "request_id": "req-exclusive-envelope",
            "store_generation": generation,
            "operation": { "type": "health" }
        });

        for invalid in [
            json!({
                "schema_version": "1.0",
                "now": "2026-07-13T09:00:01Z"
            }),
            json!({
                "schema_version": "1.0",
                "now": "2026-07-13T09:00:01Z",
                "request": shared_request,
                "control": { "type": "runtime-state" }
            }),
        ] {
            let (status, response) = call_raw(
                handle,
                &serde_json::to_vec(&invalid).expect("encode invalid envelope"),
            );
            assert_eq!(status, ChronicleStatus::Contract as u32, "{response}");
            assert_eq!(response["error"]["code"], "invalid-call-envelope");
        }

        let runtime_state = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:01Z",
            "control": { "type": "runtime-state" }
        });
        let (status, response) = call_raw(
            handle,
            &serde_json::to_vec(&runtime_state).expect("encode runtime state"),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        assert_eq!(response["result"]["recording_preference"], false);
        assert_eq!(response["result"]["cadence"], "sixty-seconds");
        assert_eq!(
            response["result"]["screenshot_retention"],
            "twenty-four-hours"
        );

        let private_operation_as_shared = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:01Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-private-storage-control",
                "store_generation": generation,
                "operation": { "type": "storage-health" }
            }
        });
        let (status, response) = call_raw(
            handle,
            &serde_json::to_vec(&private_operation_as_shared).expect("encode private operation"),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{response}");
        assert!(response.get("result").is_none());
        let _ = close(handle);
    }

    #[test]
    fn disclosure_grant_controls_are_app_only_idempotent_and_exactly_revocable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (mut handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        let grant = disclosure_grant(
            generation,
            "grant-ffi-codex",
            "client-ffi-codex",
            "receipt-ffi-codex",
        );

        let install = json!({
            "type": "install-disclosure-grant",
            "grant": grant
        });
        let (status, installed) = control(handle, "2026-07-13T09:00:01Z", install.clone());
        assert_eq!(status, ChronicleStatus::Ok as u32, "{installed}");
        assert_eq!(installed["result"]["mutation"], "installed");
        assert_eq!(installed["result"]["grant"]["state"], "active");
        assert_eq!(
            installed["result"]["grant"]["content_classes"],
            json!(["metadata", "derived"])
        );

        let (status, replayed) = control(handle, "2026-07-13T09:00:02Z", install);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{replayed}");
        assert_eq!(replayed["result"]["mutation"], "already-installed");

        assert_eq!(close(handle).0, ChronicleStatus::Ok as u32);
        let (reopened, reopened_response) = open(&temporary);
        assert_eq!(reopened_response["result"]["store_generation"], generation);
        handle = reopened;

        let wrong_identity = json!({
            "type": "revoke-disclosure-grant",
            "grant_id": "grant-ffi-codex",
            "client_id": "SECRET_CLIENT_MUST_NOT_BE_ECHOED",
            "receipt_id": "receipt-ffi-codex"
        });
        let (status, mismatch) = control(handle, "2026-07-13T09:00:03Z", wrong_identity);
        assert_eq!(status, ChronicleStatus::Contract as u32, "{mismatch}");
        assert_eq!(
            mismatch["error"]["code"],
            "disclosure-grant-identity-mismatch"
        );
        assert!(
            !mismatch
                .to_string()
                .contains("SECRET_CLIENT_MUST_NOT_BE_ECHOED")
        );

        let revoke = json!({
            "type": "revoke-disclosure-grant",
            "grant_id": "grant-ffi-codex",
            "client_id": "client-ffi-codex",
            "receipt_id": "receipt-ffi-codex"
        });
        let (status, revoked) = control(handle, "2026-07-13T09:00:04Z", revoke.clone());
        assert_eq!(status, ChronicleStatus::Ok as u32, "{revoked}");
        assert_eq!(revoked["result"]["mutation"], "revoked");
        assert_eq!(revoked["result"]["grant"]["state"], "revoked");

        let (status, replayed) = control(handle, "2026-07-13T09:00:05Z", revoke);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{replayed}");
        assert_eq!(replayed["result"]["mutation"], "already-revoked");

        let shared_install = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:06Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-shared-grant-install",
                "store_generation": generation,
                "operation": {
                    "type": "install-disclosure-grant",
                    "data": { "grant": disclosure_grant(
                        generation,
                        "grant-agent-forbidden",
                        "client-agent-forbidden",
                        "receipt-agent-forbidden"
                    ) }
                }
            }
        });
        let (status, forbidden) = call_raw(
            handle,
            &serde_json::to_vec(&shared_install).expect("encode forbidden shared operation"),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{forbidden}");
        assert!(forbidden.get("result").is_none());
        let _ = close(handle);
    }

    #[test]
    fn disclosure_grant_install_fails_closed_without_persisting_invalid_or_conflicting_input() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        let mut inactive = disclosure_grant(
            generation,
            "grant-ffi-invalid",
            "SECRET_INVALID_CLIENT_MUST_NOT_BE_ECHOED",
            "receipt-ffi-invalid",
        );
        inactive["expires_at"] = json!("2026-07-13T09:00:01Z");
        let (status, rejected) = control(
            handle,
            "2026-07-13T09:00:01Z",
            json!({
                "type": "install-disclosure-grant",
                "grant": inactive
            }),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");
        assert_eq!(rejected["error"]["code"], "inactive-disclosure-grant");
        assert!(
            !rejected
                .to_string()
                .contains("SECRET_INVALID_CLIENT_MUST_NOT_BE_ECHOED")
        );

        let valid = disclosure_grant(
            generation,
            "grant-ffi-invalid",
            "client-ffi-valid",
            "receipt-ffi-invalid",
        );
        let (status, installed) = control(
            handle,
            "2026-07-13T09:00:02Z",
            json!({
                "type": "install-disclosure-grant",
                "grant": valid
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{installed}");
        assert_eq!(installed["result"]["mutation"], "installed");

        let collision = disclosure_grant(
            generation,
            "grant-ffi-collision",
            "SECRET_COLLISION_CLIENT_MUST_NOT_BE_ECHOED",
            "receipt-ffi-invalid",
        );
        let (status, rejected) = control(
            handle,
            "2026-07-13T09:00:03Z",
            json!({
                "type": "install-disclosure-grant",
                "grant": collision
            }),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");
        assert_eq!(rejected["error"]["code"], "disclosure-grant-conflict");
        assert!(
            !rejected
                .to_string()
                .contains("SECRET_COLLISION_CLIENT_MUST_NOT_BE_ECHOED")
        );
        let _ = close(handle);
    }

    #[test]
    fn factual_report_is_one_factual_snapshot_with_hierarchy_gaps_and_no_private_payloads() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        seed_report_chunk(handle, true);

        let (status, response) = factual_report_control(handle);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        let report = &response["result"];
        assert_eq!(report["schema_version"], "1.0");
        assert_eq!(report["generated_at"], "2026-07-13T09:07:00Z");
        assert_eq!(report["stable_cutoff"], report["generated_at"]);
        assert_eq!(report["store_generation"], 1);
        assert_eq!(report["coverage"]["evidence_seconds"]["captured"], 150);
        assert_eq!(report["coverage"]["evidence_seconds"]["protected"], 30);
        assert_eq!(report["coverage"]["evidence_seconds"]["unavailable"], 30);
        assert_eq!(report["coverage"]["evidence_seconds"]["gap"], 90);
        let evidence = &report["coverage"]["evidence_seconds"];
        let evidence_total: u64 = [
            "captured",
            "protected",
            "paused",
            "unavailable",
            "error",
            "gap",
        ]
        .into_iter()
        .map(|key| evidence[key].as_u64().expect("evidence seconds"))
        .sum();
        assert_eq!(evidence_total, 300);
        assert_eq!(report["coverage"]["gaps"].as_array().map(Vec::len), Some(4));

        let totals = report["factual_totals"].as_array().expect("factual totals");
        let writer = totals
            .iter()
            .find(|total| {
                total["dimension"] == "application" && total["key"] == "com.example.writer"
            })
            .expect("writer total");
        assert_eq!(writer["label"], "Synthetic Writer");
        assert_eq!(writer["estimated_seconds"], 60);
        assert_eq!(
            writer["supporting_chunk_ids"],
            json!(["chunk-20260713T0900Z"])
        );
        assert_eq!(
            writer["supporting_event_ids"],
            json!(["evt-090015", "evt-090045"])
        );
        let window = totals
            .iter()
            .find(|total| total["dimension"] == "window")
            .expect("window total");
        assert_eq!(window["label"], "Quarterly notes");
        assert_eq!(window["parent_key"], "com.example.writer");
        assert_eq!(report["domain_context_available"], true);
        assert!(
            totals
                .iter()
                .any(|total| total["dimension"] == "authorized-domain")
        );

        let buckets = report["activity_buckets"]
            .as_array()
            .expect("activity buckets");
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0]["revision_id"], "chunk-rev-002");
        assert_eq!(buckets[0]["late_input"], true);
        assert_eq!(buckets[0]["transitions"].as_array().map(Vec::len), Some(2));
        assert_eq!(report["transitions"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            report["provenance"]["source_chunk_revision_ids"],
            json!(["chunk-rev-002"])
        );

        let bucket_application_seconds: u64 = buckets[0]["duration_estimates"]
            .as_array()
            .expect("bucket estimates")
            .iter()
            .filter(|estimate| estimate["dimension"] == "application")
            .map(|estimate| {
                estimate["estimated_seconds"]
                    .as_u64()
                    .expect("bucket seconds")
            })
            .sum();
        let total_application_seconds: u64 = totals
            .iter()
            .filter(|total| total["dimension"] == "application")
            .map(|total| total["estimated_seconds"].as_u64().expect("total seconds"))
            .sum();
        assert_eq!(bucket_application_seconds, total_application_seconds);

        let query_golden: Value = serde_json::from_str(include_str!(
            "../../../fixtures/synthetic/session-v1/query-results-v1.json"
        ))
        .expect("language-neutral query golden");
        let mut expected_applications = query_golden["statistics"]["data"]["factual_totals"]
            .as_array()
            .expect("golden factual totals")
            .clone();
        let mut actual_applications: Vec<Value> = totals
            .iter()
            .filter(|total| total["dimension"] == "application")
            .map(|total| {
                json!({
                    "dimension": total["dimension"],
                    "key": total["key"],
                    "estimated_seconds": total["estimated_seconds"],
                    "supporting_chunk_ids": total["supporting_chunk_ids"],
                })
            })
            .collect();
        expected_applications
            .sort_by_key(|total| total["key"].as_str().unwrap_or_default().to_owned());
        actual_applications
            .sort_by_key(|total| total["key"].as_str().unwrap_or_default().to_owned());
        assert_eq!(actual_applications, expected_applications);

        let encoded = response.to_string();
        for forbidden in [
            "ignore previous instructions",
            "ocr_extracts",
            "managed_relative_path",
            "image_bytes",
            "screenshot_bytes",
            "grant_id",
            "receipt_id",
            "disclosed_bytes",
        ] {
            assert!(!encoded.contains(forbidden), "leaked {forbidden}");
        }
        let _ = close(handle);
    }

    #[test]
    fn factual_report_is_app_only_and_does_not_meter_disclosure_grants() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        seed_report_chunk(handle, false);
        let grant_id = GrantId::new("grant-ffi-report-meter").expect("grant ID");
        let grant = disclosure_grant(
            generation,
            grant_id.as_str(),
            "client-ffi-report-meter",
            "receipt-ffi-report-meter",
        );
        let (status, installed) = control(
            handle,
            "2026-07-13T09:00:01Z",
            json!({ "type": "install-disclosure-grant", "grant": grant }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{installed}");
        let before = with_handle(handle, |core| {
            core.service.grant(&grant_id).map_err(FfiError::from)
        })
        .expect("grant before report");

        let (status, response) = factual_report_control(handle);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        assert_eq!(response["result"]["domain_context_available"], false);
        let after = with_handle(handle, |core| {
            core.service.grant(&grant_id).map_err(FfiError::from)
        })
        .expect("grant after report");
        assert_eq!(before, after);
        assert_eq!(after.disclosed_bytes, 0);

        let shared_private_call = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:07:00Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-shared-private-report",
                "store_generation": generation,
                "operation": {
                    "type": "factual-report",
                    "data": {
                        "range": {
                            "start": "2026-07-13T09:00:00Z",
                            "end": "2026-07-13T09:05:00Z"
                        }
                    }
                }
            }
        });
        let (status, rejected) = call_raw(
            handle,
            &serde_json::to_vec(&shared_private_call).expect("shared private report request"),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");

        let shared_statistics = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:07:00Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-shared-statistics-after-report",
                "store_generation": generation,
                "operation": {
                    "type": "query",
                    "data": {
                        "schema_version": "1.0",
                        "request_id": "req-shared-statistics-after-report",
                        "client_id": "client-ffi-report-meter",
                        "grant_id": "grant-ffi-report-meter",
                        "store_generation": generation,
                        "operation": {
                            "type": "statistics",
                            "data": {
                                "filter": {
                                    "range": {
                                        "start": "2026-07-13T09:00:00Z",
                                        "end": "2026-07-13T09:05:00Z"
                                    },
                                    "application_bundle_id": null,
                                    "window_text": null,
                                    "authorized_domain": null,
                                    "evidence_states": []
                                }
                            }
                        }
                    }
                }
            }
        });
        let (status, shared) = call_raw(
            handle,
            &serde_json::to_vec(&shared_statistics).expect("shared statistics request"),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{shared}");
        let shared_result = &shared["result"]["result"]["data"]["result"];
        assert_eq!(shared_result["type"], "statistics");
        let shared_data = shared_result["data"].as_object().expect("statistics data");
        assert_eq!(shared_data.len(), 1);
        assert!(shared_data.contains_key("factual_totals"));
        let _ = close(handle);
    }

    #[test]
    fn factual_report_rejects_invalid_ranges_and_enforces_its_response_budget() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        seed_report_chunk(handle, false);

        for (now, range) in [
            (
                "2026-07-13T09:07:00Z",
                json!({
                    "start": "2026-07-13T09:00:01Z",
                    "end": "2026-07-13T09:05:00Z"
                }),
            ),
            (
                "2026-07-13T09:07:00Z",
                json!({
                    "start": "2026-07-13T09:00:00Z",
                    "end": "2026-07-13T09:10:00Z"
                }),
            ),
            (
                "2026-09-01T00:00:00Z",
                json!({
                    "start": "2026-07-01T00:00:00Z",
                    "end": "2026-08-01T00:05:00Z"
                }),
            ),
            (
                "2026-07-13T09:07:00Z",
                json!({
                    "start": "2026-07-13T09:05:00Z",
                    "end": "2026-07-13T09:05:00Z"
                }),
            ),
        ] {
            let (status, rejected) = control(
                handle,
                now,
                json!({ "type": "factual-report", "range": range }),
            );
            assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");
            assert_eq!(rejected["error"]["code"], "invalid-factual-report-range");
        }

        let range = UtcRange {
            start: "2026-07-13T09:00:00Z".parse().expect("range start"),
            end: "2026-07-13T09:05:00Z".parse().expect("range end"),
        };
        let now = "2026-07-13T09:07:00Z".parse().expect("report now");
        let error = with_handle(handle, |core| {
            let report = FactualStatistics::new(
                StoreQueries::new(core.sqlite.clone())
                    .snapshot()
                    .map_err(FfiError::from)?,
            )
            .range(&range)
            .map_err(FfiError::from)?;
            let snapshot =
                build_factual_report_snapshot(range.clone(), now, core.opened_generation, report)?;
            serialize_bounded_factual_report(snapshot, 1)
        })
        .expect_err("one-byte report budget must fail");
        assert_eq!(error.status, ChronicleStatus::TooLarge);
        assert_eq!(error.code, "factual-report-too-large");
        let _ = close(handle);
    }

    #[test]
    fn timeline_page_search_and_details_share_factual_ids_without_paths_or_markup() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        seed_timeline_fixture(handle);

        let (status, response) = timeline_page_control(handle, None, None, json!([]));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        let page = &response["result"];
        assert_eq!(page["schema_version"], "1.0");
        assert_eq!(page["stable_cutoff"], "2026-07-13T09:07:00Z");
        assert_eq!(page["chunks"].as_array().map(Vec::len), Some(1));
        assert_eq!(page["chunks"][0]["chunk_id"], "chunk-20260713T0900Z");
        assert_eq!(page["chunks"][0]["revision_id"], "chunk-rev-002");
        assert_eq!(page["chunks"][0]["evidence_seconds"]["gap"], 90);
        assert_eq!(
            page["chunks"][0]["extracts"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(page["chunks"][0]["extracts"][0]["untrusted_evidence"], true);
        assert!(page["chunks"][0]["extracts"][0]["character_count"].is_u64());
        assert_eq!(page["coverage"]["evidence_seconds"]["captured"], 150);
        let token = page["snapshot_token"]
            .as_str()
            .expect("snapshot token")
            .to_owned();
        assert!(token.starts_with("ocs1."));

        let encoded_page = page.to_string();
        for forbidden in [
            "ignore previous instructions",
            "managed_relative_path",
            "screenshots/",
            "image_bytes",
            "screenshot_bytes",
            "<mark>",
        ] {
            assert!(!encoded_page.contains(forbidden), "leaked {forbidden}");
        }

        let (status, detail) = control(
            handle,
            "2026-07-13T09:07:01Z",
            json!({
                "type": "timeline-chunk-detail",
                "snapshot_token": token,
                "revision_id": "chunk-rev-002"
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{detail}");
        assert_eq!(detail["result"]["chunk"]["revision_id"], "chunk-rev-002");
        assert!(
            detail["result"]["chunk"]["ocr_extracts"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("ignore previous instructions"))
        );

        let (status, logical_detail) = control(
            handle,
            "2026-07-13T09:07:01Z",
            json!({
                "type": "timeline-chunk-detail",
                "snapshot_token": detail["result"]["snapshot_token"],
                "chunk_id": "chunk-20260713T0900Z"
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{logical_detail}");
        assert_eq!(
            logical_detail["result"]["chunk"]["revision_id"],
            "chunk-rev-002"
        );

        let detail_token = detail["result"]["snapshot_token"]
            .as_str()
            .expect("detail token");
        let (status, event) = control(
            handle,
            "2026-07-13T09:07:02Z",
            json!({
                "type": "timeline-event-detail",
                "snapshot_token": detail_token,
                "event_id": "evt-090015"
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{event}");
        assert_eq!(event["result"]["event"]["event_id"], "evt-090015");
        assert_eq!(
            event["result"]["event"]["payload"]["data"]["content"]["data"]["image"]["state"],
            "retained"
        );
        let encoded_event = event.to_string();
        assert!(!encoded_event.contains("managed_relative_path"));
        assert!(!encoded_event.contains("screenshots/"));

        let (status, first_search) = control(
            handle,
            "2026-07-13T09:07:03Z",
            json!({
                "type": "timeline-search",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "filter": timeline_filter(json!([])),
                "query": "synthetic",
                "page": { "cursor": null, "limit": 1 }
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{first_search}");
        let search = &first_search["result"];
        assert_eq!(search["hits"].as_array().map(Vec::len), Some(1));
        assert_eq!(search["hits"][0]["untrusted_evidence"], true);
        assert!(search["hits"][0]["snippet"]["text"].is_string());
        assert!(search["hits"][0]["snippet"]["highlights"][0]["start"].is_u64());
        assert!(!search.to_string().contains("<mark>"));
        assert!(search["page"]["truncated"].as_bool().unwrap_or(false));
        let search_token = search["snapshot_token"]
            .as_str()
            .expect("search snapshot token");
        let cursor = search["page"]["next_cursor"]
            .as_str()
            .expect("search cursor");
        let first_id = search["hits"][0]["event_id"].clone();
        let (status, second_search) = control(
            handle,
            "2026-07-13T09:07:04Z",
            json!({
                "type": "timeline-search",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": search_token,
                "filter": timeline_filter(json!([])),
                "query": "synthetic",
                "page": { "cursor": cursor, "limit": 1 }
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{second_search}");
        assert_ne!(second_search["result"]["hits"][0]["event_id"], first_id);

        let (status, forged_search_cursor) = control(
            handle,
            "2026-07-13T09:07:04Z",
            json!({
                "type": "timeline-search",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": search_token,
                "filter": timeline_filter(json!([])),
                "query": "synthetic",
                "page": { "cursor": "evt-090045", "limit": 1 }
            }),
        );
        assert_eq!(
            status,
            ChronicleStatus::Contract as u32,
            "{forged_search_cursor}"
        );
        assert_eq!(
            forged_search_cursor["error"]["code"],
            "invalid-pagination-cursor"
        );

        let (status, missing) =
            timeline_page_control(handle, None, None, json!(["missing-observation"]));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{missing}");
        assert_eq!(
            missing["result"]["chunks"].as_array().map(Vec::len),
            Some(1)
        );
        let (status, missing_search) = control(
            handle,
            "2026-07-13T09:07:05Z",
            json!({
                "type": "timeline-search",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "filter": timeline_filter(json!(["missing-observation"])),
                "query": "synthetic",
                "page": { "cursor": null, "limit": 10 }
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{missing_search}");
        assert!(
            missing_search["result"]["hits"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );

        let shared_private_call = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:07:06Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-shared-private-timeline",
                "store_generation": page["store_generation"],
                "operation": {
                    "type": "timeline-event-detail",
                    "data": {
                        "snapshot_token": detail_token,
                        "event_id": "evt-090015"
                    }
                }
            }
        });
        let (status, rejected) = call_raw(
            handle,
            &serde_json::to_vec(&shared_private_call).expect("shared private timeline request"),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");

        let _ = close(handle);
    }

    #[test]
    fn timeline_snapshot_high_water_excludes_backfilled_revisions_and_rejects_scope_changes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        seed_timeline_fixture(handle);
        let (status, first) = timeline_page_control(handle, None, None, json!([]));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{first}");
        let token = first["result"]["snapshot_token"]
            .as_str()
            .expect("snapshot token")
            .to_owned();
        assert_eq!(first["result"]["chunks"][0]["revision_id"], "chunk-rev-002");

        let mut late = include_str!("../../../fixtures/synthetic/session-v1/chunks.jsonl")
            .lines()
            .last()
            .map(ChunkRevision::parse)
            .expect("fixture chunk")
            .expect("valid fixture chunk");
        late.revision_id = ChunkRevisionId::new("chunk-rev-003").expect("revision ID");
        late.prior_revision_id = Some(ChunkRevisionId::new("chunk-rev-002").expect("revision ID"));
        late.supersedes_revision_id =
            Some(ChunkRevisionId::new("chunk-rev-002").expect("revision ID"));
        late.generated_at = "2026-07-13T09:06:45Z".parse().expect("generated at");
        late.input_digest = "sha256-input-003".to_owned();
        late.validate().expect("late revision valid");
        with_handle(handle, |core| {
            let record = CanonicalJournal::new(core.root.clone())
                .append_chunk(&late, FaultInjector::none())
                .map_err(FfiError::from)?;
            Projector::new(core.sqlite.clone())
                .project_record(&record, FaultInjector::none())
                .map_err(FfiError::from)
        })
        .expect("project late revision");

        let (status, stable) = timeline_page_control(handle, Some(&token), None, json!([]));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{stable}");
        assert_eq!(
            stable["result"]["chunks"][0]["revision_id"],
            "chunk-rev-002"
        );
        let (status, refreshed) = timeline_page_control(handle, None, None, json!([]));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{refreshed}");
        assert_eq!(
            refreshed["result"]["chunks"][0]["revision_id"],
            "chunk-rev-003"
        );

        let (status, hidden_detail) = control(
            handle,
            "2026-07-13T09:07:01Z",
            json!({
                "type": "timeline-chunk-detail",
                "snapshot_token": token,
                "revision_id": "chunk-rev-003"
            }),
        );
        assert_eq!(status, ChronicleStatus::NotFound as u32, "{hidden_detail}");

        let mut tampered = token.clone();
        tampered.push('0');
        let (status, rejected) = timeline_page_control(handle, Some(&tampered), None, json!([]));
        assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");
        assert_eq!(rejected["error"]["code"], "invalid-snapshot-token");

        let mut wrong_anchor = decode_snapshot_token(&token).expect("decode snapshot token");
        wrong_anchor.event_anchor_id =
            Some(EventId::new("evt-not-the-boundary").expect("event ID"));
        let wrong_anchor = encode_snapshot_token(&wrong_anchor).expect("encode wrong anchor");
        let (status, rejected) =
            timeline_page_control(handle, Some(&wrong_anchor), None, json!([]));
        assert_eq!(
            status,
            ChronicleStatus::StaleGeneration as u32,
            "{rejected}"
        );
        assert_eq!(rejected["error"]["code"], "snapshot-no-longer-available");

        let (status, scope_mismatch) =
            timeline_page_control(handle, Some(&token), None, json!(["protected"]));
        assert_eq!(status, ChronicleStatus::Contract as u32, "{scope_mismatch}");
        assert_eq!(scope_mismatch["error"]["code"], "snapshot-scope-mismatch");

        let (status, cursor_without_token) =
            timeline_page_control(handle, None, Some("chunk-20260713T0900Z"), json!([]));
        assert_eq!(
            status,
            ChronicleStatus::Contract as u32,
            "{cursor_without_token}"
        );
        assert_eq!(
            cursor_without_token["error"]["code"],
            "snapshot-token-required"
        );

        let root = with_handle(handle, |core| Ok(core.root.clone())).expect("managed root");
        let _ = close(handle);
        RecoveryManager::new(root)
            .rebuild_index()
            .expect("deterministic projection rebuild");
        let (rebuilt_handle, _) = open(&temporary);
        let (status, after_rebuild) =
            timeline_page_control(rebuilt_handle, Some(&token), None, json!([]));
        assert_eq!(
            status,
            ChronicleStatus::StaleGeneration as u32,
            "{after_rebuild}"
        );
        assert_eq!(
            after_rebuild["error"]["code"],
            "snapshot-no-longer-available"
        );
        let _ = close(rebuilt_handle);
    }

    #[test]
    fn timeline_controls_validate_ranges_filters_cursors_and_response_budgets() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        seed_timeline_fixture(handle);

        for control_value in [
            json!({
                "type": "timeline-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "filter": {
                    "range": {"start":"2026-07-13T09:00:01Z","end":"2026-07-13T09:05:00Z"},
                    "application_bundle_id": null,
                    "window_text": null,
                    "authorized_domain": null,
                    "coverage_states": []
                },
                "page": {"cursor":null,"limit":10}
            }),
            json!({
                "type": "timeline-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "filter": timeline_filter(json!(["captured", "captured"])),
                "page": {"cursor":null,"limit":10}
            }),
            json!({
                "type": "timeline-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "filter": timeline_filter(json!([])),
                "page": {"cursor":null,"limit":101}
            }),
            json!({
                "type": "timeline-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "filter": timeline_filter(json!([])),
                "page": {"cursor":"not a valid ID!","limit":10}
            }),
            json!({
                "type": "timeline-chunk-detail",
                "snapshot_token": "not-used-because-selector-is-invalid"
            }),
            json!({
                "type": "timeline-chunk-detail",
                "snapshot_token": "not-used-because-selector-is-invalid",
                "revision_id": "chunk-rev-002",
                "chunk_id": "chunk-20260713T0900Z"
            }),
        ] {
            let (status, rejected) = control(handle, "2026-07-13T09:07:00Z", control_value);
            assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");
        }

        let (status, response) = timeline_page_control(handle, None, None, json!([]));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        let error =
            serialize_bounded_timeline(response["result"].clone(), 1, "timeline-test-limit")
                .expect_err("one-byte timeline budget must fail");
        assert_eq!(error.status, ChronicleStatus::TooLarge);
        assert_eq!(error.code, "timeline-test-limit");

        let excluded_filter = json!({
            "range": {
                "start": "2026-07-13T09:00:00Z",
                "end": "2026-07-13T09:05:00Z"
            },
            "application_bundle_id": "com.example.does-not-exist",
            "window_text": null,
            "authorized_domain": null,
            "coverage_states": []
        });
        let (status, excluded) = control(
            handle,
            "2026-07-13T09:07:00Z",
            json!({
                "type": "timeline-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "filter": excluded_filter.clone(),
                "page": {"cursor": null, "limit": 10}
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{excluded}");
        assert!(
            excluded["result"]["chunks"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );
        let (status, forged_chunk_cursor) = control(
            handle,
            "2026-07-13T09:07:00Z",
            json!({
                "type": "timeline-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": excluded["result"]["snapshot_token"],
                "filter": excluded_filter,
                "page": {"cursor": "chunk-20260713T0900Z", "limit": 10}
            }),
        );
        assert_eq!(
            status,
            ChronicleStatus::Contract as u32,
            "{forged_chunk_cursor}"
        );
        assert_eq!(
            forged_chunk_cursor["error"]["code"],
            "invalid-pagination-cursor"
        );
        let _ = close(handle);
    }

    #[test]
    fn analysis_controls_page_real_immutable_artifacts_without_shared_grants() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        seed_timeline_fixture(handle);
        write_analysis_revision(
            handle,
            &analysis_revision("analysis-rev-001", None, "Initial derived finding"),
        );
        write_analysis_revision(
            handle,
            &analysis_revision(
                "analysis-rev-002",
                Some("analysis-rev-001"),
                "Reviewed derived finding",
            ),
        );

        let (status, page) = control(
            handle,
            "2026-07-13T09:07:00Z",
            json!({
                "type": "analysis-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": null,
                "range": {
                    "start": "2026-07-13T09:00:00Z",
                    "end": "2026-07-13T09:05:00Z"
                },
                "page": {"cursor": null, "limit": 10}
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{page}");
        assert_eq!(
            page["result"]["artifacts"][0]["revision_id"],
            "analysis-rev-002"
        );
        assert_eq!(
            page["result"]["artifacts"][0]["payload"]["statement"],
            "Reviewed derived finding"
        );
        assert_eq!(
            page["result"]["provenance"]["source_event_ids"][0],
            "evt-090015"
        );
        let token = page["result"]["snapshot_token"]
            .as_str()
            .expect("analysis snapshot token")
            .to_owned();

        let (status, detail) = control(
            handle,
            "2026-07-13T09:07:01Z",
            json!({
                "type": "analysis-detail",
                "snapshot_token": token,
                "artifact_id": "analysis-work-pattern",
                "revision_id": "analysis-rev-001"
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{detail}");
        assert_eq!(
            detail["result"]["artifact"]["revision_id"],
            "analysis-rev-001"
        );

        write_analysis_revision(
            handle,
            &analysis_revision(
                "analysis-rev-003",
                Some("analysis-rev-002"),
                "Late projected derived finding",
            ),
        );
        let (status, stable) = control(
            handle,
            "2026-07-13T09:07:02Z",
            json!({
                "type": "analysis-page",
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "snapshot_token": token,
                "range": {
                    "start": "2026-07-13T09:00:00Z",
                    "end": "2026-07-13T09:05:00Z"
                },
                "page": {"cursor": null, "limit": 10}
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{stable}");
        assert_eq!(
            stable["result"]["artifacts"][0]["revision_id"],
            "analysis-rev-002"
        );

        let shared_private_call = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:07:03Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-shared-private-analysis",
                "store_generation": page["result"]["store_generation"],
                "operation": {
                    "type": "analysis-page",
                    "data": {}
                }
            }
        });
        let (status, rejected) = call_raw(
            handle,
            &serde_json::to_vec(&shared_private_call).expect("shared private analysis request"),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{rejected}");
        let _ = close(handle);
    }

    #[test]
    fn screenshot_storage_errors_are_typed_retryable_and_never_acknowledged() {
        for (error, code) in [
            (
                StoreError::ScreenshotFreeSpace {
                    available_bytes: 1,
                    required_bytes: 2,
                },
                "screenshot-free-space",
            ),
            (
                StoreError::ScreenshotImageQuota {
                    managed_image_bytes: 20,
                    candidate_bytes: 1,
                    quota_bytes: 20,
                },
                "screenshot-image-quota",
            ),
        ] {
            let error = FfiError::from(error);
            assert_eq!(error.status, ChronicleStatus::Io);
            let response = error.response();
            assert_eq!(response["ok"], false);
            assert_eq!(response["error"]["code"], code);
            assert_eq!(response["error"]["retryable"], true);
            assert!(response.get("result").is_none());
            assert!(response.get("acknowledgement").is_none());
        }
    }

    #[test]
    fn app_controls_delegate_to_the_recording_coordinator() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);

        for (now, request) in [
            (
                "2026-07-13T09:00:00Z",
                json!({
                    "type": "startup-reconcile",
                    "session_id": "ffi-session-one",
                    "device_id": "dev-ffi-runtime",
                    "display_timezone": "Europe/Zurich"
                }),
            ),
            (
                "2026-07-13T09:00:01Z",
                json!({ "type": "set-recording-preference", "enabled": true }),
            ),
            (
                "2026-07-13T09:00:02Z",
                json!({ "type": "set-cadence", "cadence": "thirty-seconds" }),
            ),
            (
                "2026-07-13T09:00:02.500Z",
                json!({
                    "type": "set-screenshot-retention",
                    "retention": "seven-days"
                }),
            ),
            (
                "2026-07-13T09:00:03Z",
                json!({
                    "type": "configure-study",
                    "start": "2026-07-13T09:00:00Z",
                    "end": "2026-07-13T10:00:00Z"
                }),
            ),
            (
                "2026-07-13T09:30:00Z",
                json!({ "type": "capture-admission" }),
            ),
            (
                "2026-07-13T09:30:01Z",
                json!({
                    "type": "extend-study",
                    "new_end": "2026-07-13T11:00:00Z"
                }),
            ),
            (
                "2026-07-13T09:30:02Z",
                json!({ "type": "use-personal-mode" }),
            ),
            (
                "2026-07-13T09:30:03Z",
                json!({ "type": "reconcile-pending-images" }),
            ),
        ] {
            let (status, response) = control(handle, now, request);
            assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        }

        let (status, state) = control(
            handle,
            "2026-07-13T09:30:04Z",
            json!({ "type": "runtime-state" }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{state}");
        assert_eq!(state["result"]["recording_preference"], true);
        assert_eq!(state["result"]["cadence"], "thirty-seconds");
        assert_eq!(state["result"]["screenshot_retention"], "seven-days");

        let (status, storage) = control(
            handle,
            "2026-07-13T09:30:04Z",
            json!({ "type": "storage-health" }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{storage}");
        assert_eq!(storage["result"]["managed_image_bytes"], 0);
        assert_eq!(
            storage["result"]["warning_free_bytes"],
            4 * 1024 * 1024 * 1024_u64
        );
        assert_eq!(
            storage["result"]["minimum_free_bytes"],
            2 * 1024 * 1024 * 1024_u64
        );
        assert_eq!(
            storage["result"]["managed_image_quota_bytes"],
            20 * 1024 * 1024 * 1024_u64
        );

        let (status, response) = control(
            handle,
            "2026-07-13T09:31:00Z",
            json!({
                "type": "prepare-termination",
                "session_id": "ffi-session-one"
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        assert_eq!(response["result"]["prepared"], true);
        let _ = close(handle);
    }

    #[test]
    fn runtime_gap_control_is_app_private_typed_and_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        start_recording(handle, "ffi-runtime-gap");

        let request = json!({
            "type": "reconcile-runtime-gap",
            "reason": "sleep",
            "device_id": "dev-ffi-ingest",
            "display_timezone": "Europe/Zurich"
        });
        let (status, first) = control(handle, "2026-07-13T09:05:00Z", request.clone());
        assert_eq!(status, ChronicleStatus::Ok as u32, "{first}");
        let (status, repeated) = control(handle, "2026-07-13T09:05:00Z", request);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{repeated}");
        assert_eq!(first["result"], repeated["result"]);
        assert_eq!(
            first["result"]["gap_event_ids"]
                .as_array()
                .expect("gap event IDs")
                .len(),
            1
        );

        let invalid = json!({
            "type": "reconcile-runtime-gap",
            "reason": "quit",
            "device_id": "dev-ffi-ingest",
            "display_timezone": "Europe/Zurich"
        });
        let (status, response) = control(handle, "2026-07-13T09:06:00Z", invalid);
        assert_eq!(status, ChronicleStatus::Contract as u32, "{response}");
        assert_eq!(response["error"]["code"], "contract-error");

        let private_operation_as_shared = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:06:00Z",
            "request": {
                "schema_version": "1.0",
                "request_id": "req-private-runtime-gap",
                "store_generation": generation,
                "operation": {
                    "type": "reconcile-runtime-gap",
                    "reason": "sleep"
                }
            }
        });
        let (status, response) = call_raw(
            handle,
            &serde_json::to_vec(&private_operation_as_shared)
                .expect("encode private shared operation"),
        );
        assert_eq!(status, ChronicleStatus::Contract as u32, "{response}");
        assert!(response.get("result").is_none());
        let _ = close(handle);
    }

    #[test]
    fn capture_admission_is_runtime_inactive_before_startup_and_after_termination() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        let (status, response) = control(
            handle,
            "2026-07-13T09:00:00Z",
            json!({ "type": "set-recording-preference", "enabled": true }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");

        let assert_inactive = |now: &str| {
            let (status, response) = control(handle, now, json!({ "type": "capture-admission" }));
            assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
            assert_eq!(response["result"]["allowed"], false);
            assert_eq!(response["result"]["reason"], "runtime-inactive");
        };
        assert_inactive("2026-07-13T09:00:01Z");

        let (status, response) = control(
            handle,
            "2026-07-13T09:00:02Z",
            json!({
                "type": "startup-reconcile",
                "session_id": "ffi-inactive-session",
                "device_id": "dev-ffi-runtime",
                "display_timezone": "Europe/Zurich"
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        let (status, response) = control(
            handle,
            "2026-07-13T09:00:03Z",
            json!({
                "type": "prepare-termination",
                "session_id": "ffi-inactive-session"
            }),
        );
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        assert_inactive("2026-07-13T09:00:04Z");
        let _ = close(handle);
    }

    #[test]
    fn null_malformed_invalid_utf8_and_oversized_inputs_are_typed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        let mut response = ChronicleBuffer::EMPTY;
        // SAFETY: null input is deliberately tested with a valid output.
        let status = unsafe { chronicle_call(handle, ptr::null(), 0, &mut response) };
        assert_eq!(status, ChronicleStatus::InvalidArgument as u32);
        free(&mut response);

        let (status, value) = call_raw(handle, b"{");
        assert_eq!(status, ChronicleStatus::Contract as u32, "{value}");
        let (status, value) = call_raw(handle, &[0xff, 0xfe]);
        assert_eq!(status, ChronicleStatus::Contract as u32, "{value}");

        let oversized = vec![b' '; MAX_CALL_REQUEST_BYTES + 1];
        let (status, value) = call_raw(handle, &oversized);
        assert_eq!(status, ChronicleStatus::TooLarge as u32, "{value}");
        let _ = close(handle);
    }

    #[test]
    fn panic_is_contained_as_versioned_error() {
        let mut response = ChronicleBuffer::EMPTY;
        let status = json_boundary(&mut response, || -> Result<Value, FfiError> {
            panic!("synthetic ABI panic")
        });
        assert_eq!(status, ChronicleStatus::Panic as u32);
        let value: Value =
            serde_json::from_slice(&response_bytes(response)).expect("decode panic response");
        assert_eq!(value["schema_version"], "1.0");
        assert_eq!(value["error"]["code"], "panic-contained");
        free(&mut response);
    }

    #[test]
    fn swift_query_fixtures_are_valid_v1_service_responses() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../macos/OpenChronicleTests/Fixtures/shared-response-v1.json"
        ))
        .expect("decode Swift fixture set");
        for key in ["statistics_response", "chunk_response", "search_response"] {
            let encoded = serde_json::to_string(&fixture[key]).expect("encode fixture response");
            QueryResponse::parse(&encoded)
                .unwrap_or_else(|error| panic!("{key} is not a valid QueryResponse: {error}"));
        }
    }

    #[test]
    fn non_image_and_transactional_image_ingest_use_the_recording_coordinator() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, _) = open(&temporary);
        start_recording(handle, "ffi-ingest-session");
        let events = fixture_events();
        let image_ingest = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:18Z",
            "cadence": { "boot_sequence": "ffi-boot", "monotonic_tick": 1 },
            "event": events[0],
            "completion": events[1],
        });
        let image = b"synthetic-heic-bytes";
        let (status, response) = ingest_raw(handle, &image_ingest, Some(image));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        assert_eq!(response["result"]["acknowledgement"], "durable");

        let non_image = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:47Z",
            "cadence": { "boot_sequence": "ffi-boot", "monotonic_tick": 2 },
            "event": events[2],
            "completion": null,
        });
        let (status, response) = ingest_raw(handle, &non_image, None);
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");
        let _ = close(handle);
    }

    #[test]
    fn image_read_enforces_id_generation_state_path_file_and_size_boundaries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        start_recording(handle, "ffi-image-session");
        let events = fixture_events();
        let image_ingest = json!({
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:18Z",
            "cadence": { "boot_sequence": "ffi-image-boot", "monotonic_tick": 1 },
            "event": events[0],
            "completion": events[1],
        });
        let image = b"synthetic-image";
        let (status, response) = ingest_raw(handle, &image_ingest, Some(image));
        assert_eq!(status, ChronicleStatus::Ok as u32, "{response}");

        let (status, bytes) = read_image(handle, &image_request("img-001", generation, 1024));
        assert_eq!(status, ChronicleStatus::Ok as u32);
        assert_eq!(bytes, image);

        let (status, error) = read_image(handle, &image_request("img-001", generation + 1, 1024));
        assert_eq!(status, ChronicleStatus::StaleGeneration as u32);
        let value: Value = serde_json::from_slice(&error).expect("stale JSON");
        assert_eq!(value["error"]["code"], "stale-generation");

        let (status, _) = read_image(handle, &image_request("img-001", generation, 1));
        assert_eq!(status, ChronicleStatus::TooLarge as u32);
        let (status, _) = read_image(handle, &image_request("missing", generation, 1024));
        assert_eq!(status, ChronicleStatus::NotFound as u32);
        let (status, invalid) = read_image(handle, &image_request("../escape", generation, 1024));
        assert_eq!(status, ChronicleStatus::Contract as u32);
        let value: Value = serde_json::from_slice(&invalid).expect("invalid ID JSON");
        assert!(!value.to_string().contains("screenshots/"));

        for projected_state in [
            "expired",
            "user-deleted",
            "missing",
            "delete-pending",
            "write-failed",
        ] {
            with_handle(handle, |core| {
                core.sqlite
                    .connection()
                    .map_err(FfiError::from)?
                    .execute(
                        "UPDATE screenshot_lifecycle SET state=?1 WHERE artifact_id='img-001'",
                        [projected_state],
                    )
                    .map_err(StoreError::from)
                    .map_err(FfiError::from)?;
                Ok(())
            })
            .expect("update projected state");
            let (status, _) = read_image(handle, &image_request("img-001", generation, 1024));
            assert_eq!(
                status,
                ChronicleStatus::NotRetained as u32,
                "projected state {projected_state} returned image bytes"
            );
        }

        with_handle(handle, |core| {
            core.sqlite
                .connection()
                .map_err(FfiError::from)?
                .execute(
                    "UPDATE screenshot_lifecycle SET state='retained' WHERE artifact_id='img-001'",
                    [],
                )
                .map_err(StoreError::from)
                .map_err(FfiError::from)?;
            core.root
                .unlink("screenshots/2026-07-13/img-001.heic")
                .map_err(FfiError::from)
        })
        .expect("remove retained file");
        let (status, _) = read_image(handle, &image_request("img-001", generation, 1024));
        assert_eq!(status, ChronicleStatus::NotFound as u32);
        let _ = close(handle);
    }

    #[test]
    fn repeated_lifecycle_does_not_reuse_tokens() {
        let mut prior = 0;
        for _ in 0..16 {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let (handle, _) = open(&temporary);
            assert!(handle > prior);
            prior = handle;
            assert_eq!(close(handle).0, ChronicleStatus::Ok as u32);
        }
    }

    #[test]
    fn concurrent_calls_serialize_and_close_race_stays_typed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (handle, opened) = open(&temporary);
        let generation = opened["result"]["store_generation"]
            .as_u64()
            .expect("generation");
        let request = Arc::new(
            serde_json::to_vec(&json!({
                "schema_version": "1.0",
                "now": "2026-07-13T09:00:01Z",
                "request": {
                    "schema_version": "1.0",
                    "request_id": "req-concurrent-health",
                    "store_generation": generation,
                    "operation": { "type": "health" }
                }
            }))
            .expect("encode concurrent request"),
        );
        let barrier = Arc::new(Barrier::new(9));
        let callers = (0..8)
            .map(|_| {
                let request = Arc::clone(&request);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    call_raw(handle, &request)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let (close_status, close_response) = close(handle);
        assert_eq!(close_status, ChronicleStatus::Ok as u32, "{close_response}");
        for caller in callers {
            let (status, response) = caller.join().expect("caller did not panic");
            assert!(
                status == ChronicleStatus::Ok as u32
                    || status == ChronicleStatus::InvalidHandle as u32,
                "unexpected status {status}: {response}"
            );
        }
        let (status, response) = call_raw(handle, &request);
        assert_eq!(status, ChronicleStatus::InvalidHandle as u32);
        assert_eq!(response["error"]["code"], "invalid-handle");
    }
}
