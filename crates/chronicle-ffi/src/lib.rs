//! Narrow, versioned C ABI for the signed macOS application.
//!
//! Handles and output buffers are process-local registry tokens rather than
//! caller-visible Rust pointers. This makes stale/double close and free calls
//! ordinary typed failures instead of use-after-free undefined behavior.

use std::collections::BTreeMap;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use chronicle_domain::{
    CaptureCadence, DeviceId, EventEnvelope, EventPayload, ImageArtifactId, ObservationContent,
    ScreenshotProjectedState, SharedServiceRequest, parse_versioned, validate_schema_version,
};
use chronicle_engine::{
    CadenceStamp, ChunkerConfig, EngineError, IngestRequest, RecordingCoordinator, SharedService,
    SharedServiceError, StartupReconcileRequest, StudyBoundary,
};
use chronicle_store::{
    CaptureOwnerGuard, FaultInjector, LockManager, ManagedRoot, SqliteStore, StoreError,
    StoreGeneration,
};
use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const ABI_SCHEMA_VERSION: &str = chronicle_domain::CONTRACT_VERSION;
const MAX_OPEN_REQUEST_BYTES: usize = 16 * 1024;
const MAX_CALL_REQUEST_BYTES: usize = chronicle_domain::MAX_SHARED_REQUEST_BYTES + 16 * 1024;
const MAX_INGEST_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_REQUEST_BYTES: usize = 8 * 1024;
const MAX_ENCODED_IMAGE_BYTES: usize = 4 * 1024 * 1024;

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
    SetRecordingPreference {
        enabled: bool,
    },
    SetCadence {
        cadence: CaptureCadence,
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
    PrepareTermination {
        session_id: String,
    },
    StartupReconcile {
        session_id: String,
        device_id: DeviceId,
        display_timezone: String,
    },
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
}

impl From<FfiCadenceStamp> for CadenceStamp {
    fn from(value: FfiCadenceStamp) -> Self {
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
        }
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
    use chronicle_domain::QueryResponse;
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
