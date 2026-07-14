use std::time::Duration as StdDuration;

use chronicle_domain::{
    ChunkRevision, ChunkSummary, ContentClass, DiagnosticHealthSnapshot, DisclosureGrant, GrantId,
    GrantSummary, HealthCode, HealthIssue, HealthSeverity, ImageMetadata, McpHealthSummary,
    PageInfo, PageRequest, ProjectionHealth, QueryCapability, QueryCoverage, QueryEvent,
    QueryEventPayload, QueryObservationContent, QueryOperation, QueryProvenance, QueryRequest,
    QueryResponse, QueryResult, QueryScope, QueryStatus, SchemaDescriptor, SharedServiceOperation,
    SharedServiceRequest, SharedServiceResponse, SharedServiceResult, StorageHealthSummary,
    UtcRange,
};
use chronicle_store::{
    ActivitySearch, FactualStatistics, GrantQuerySession, GrantReceiptStore, LockManager,
    ManagedRoot, SqliteStore, StoreError, StoreGeneration, StoreQueries, storage_available_bytes,
    storage_health_summary, store_health_metrics,
};
use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::{
    PolicyDecision, PolicyError, authorize_query, authorize_query_content,
    operation_requires_full_range,
};

const LOCK_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SERVICE_SCHEMA_BUILD_ID: &str = env!("CARGO_PKG_VERSION");
const MAX_MOMENT_EVENTS: u32 = 1_000;

#[derive(Debug, Error)]
pub enum SharedServiceError {
    #[error("shared request contract failed: {0}")]
    Contract(String),
    #[error("disclosure grant was not found")]
    GrantNotFound,
    #[error("disclosure grant belongs to another client")]
    GrantClientMismatch,
    #[error("disclosure grant is inactive")]
    GrantInactive,
    #[error("query content class is not granted: {0:?}")]
    ContentDenied(ContentClass),
    #[error("query range is outside the disclosure policy")]
    RangeDenied,
    #[error("query range exceeds the service bound")]
    RangeLimit,
    #[error("pagination cursor does not match this query scope")]
    CursorScopeMismatch,
    #[error("pagination cursor was not found or expired")]
    CursorNotFound,
    #[error("response exceeds the grant byte budget")]
    ResponseByteLimit,
    #[error("requested evidence was not found")]
    NotFound,
    #[error("operation belongs to a later service slice")]
    UnsupportedOperation,
    #[error("store generation is stale; expected {expected}, found {actual}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error(transparent)]
    Store(StoreError),
}

#[derive(Clone, Debug)]
pub struct SharedService {
    root: ManagedRoot,
    sqlite: SqliteStore,
    queries: StoreQueries,
    grants: GrantReceiptStore,
    locks: LockManager,
    opened_generation: u64,
}

impl SharedService {
    pub fn open(root: ManagedRoot, sqlite: SqliteStore) -> Result<Self, SharedServiceError> {
        let generation = StoreGeneration::load(&root).map_err(map_store)?;
        Ok(Self {
            queries: StoreQueries::new(sqlite.clone()),
            grants: GrantReceiptStore::new(root.clone(), LOCK_TIMEOUT),
            locks: LockManager::new(root.clone(), LOCK_TIMEOUT),
            root,
            sqlite,
            opened_generation: generation.generation,
        })
    }

    pub fn install_grant(&self, grant: DisclosureGrant) -> Result<(), SharedServiceError> {
        let created_at = grant.created_at;
        self.grants.install(grant, created_at).map_err(map_store)
    }

    pub fn revoke_grant(
        &self,
        grant_id: &GrantId,
        now: DateTime<Utc>,
    ) -> Result<(), SharedServiceError> {
        self.grants
            .revoke(grant_id, self.opened_generation, now)
            .map_err(map_store)
    }

    pub fn grant(&self, grant_id: &GrantId) -> Result<DisclosureGrant, SharedServiceError> {
        self.grants.grant(grant_id).map_err(map_store)
    }

    pub fn grant_receipt_bytes(&self) -> Result<Vec<u8>, SharedServiceError> {
        self.grants.receipt_bytes().map_err(map_store)
    }

    pub fn execute(
        &self,
        request: SharedServiceRequest,
        now: DateTime<Utc>,
    ) -> Result<SharedServiceResponse, SharedServiceError> {
        let encoded = serde_json::to_string(&request)
            .map_err(|error| SharedServiceError::Contract(error.to_string()))?;
        let request = SharedServiceRequest::parse(&encoded)
            .map_err(|error| SharedServiceError::Contract(error.to_string()))?;
        match request.operation {
            SharedServiceOperation::Health => {
                let _store_guard = self.locks.shared_request().map_err(map_store)?;
                let current = StoreGeneration::load(&self.root).map_err(map_store)?;
                if request.store_generation != current.generation {
                    return Err(SharedServiceError::StaleGeneration {
                        expected: request.store_generation,
                        actual: current.generation,
                    });
                }
                let mcp = self
                    .grants
                    .health_summary(now, current.generation)
                    .map_err(map_store)?;
                // A cold managed-size measurement may walk evidence files. It
                // runs before the short health consistency lock so capture is
                // never delayed by storage accounting.
                let storage = storage_health_summary(&self.root).map_err(map_store)?;
                let _snapshot_guard = self.locks.query_snapshot().map_err(map_store)?;
                let health = self.health(now, current.generation, mcp, storage)?;
                let response = SharedServiceResponse {
                    schema_version: "1.0".to_owned(),
                    request_id: request.request_id,
                    generated_at: now,
                    store_generation: current.generation,
                    result: SharedServiceResult::Health(Box::new(health)),
                };
                response.validate().map_err(SharedServiceError::Contract)?;
                Ok(response)
            }
            SharedServiceOperation::Query(query) => self.execute_query(*query, now),
        }
    }

    fn execute_query(
        &self,
        query: QueryRequest,
        now: DateTime<Utc>,
    ) -> Result<SharedServiceResponse, SharedServiceError> {
        let mut grant_session = self
            .grants
            .begin_query(
                &query.grant_id,
                &query.client_id,
                query.store_generation,
                now,
            )
            .map_err(map_store)?;
        authorize_query_content(grant_session.grant(), &query.operation).map_err(map_policy)?;
        let queries = self.queries.snapshot().map_err(map_store)?;
        let search = ActivitySearch::from_queries(queries.clone());
        let requested_ranges = self.requested_ranges(&queries, &query.operation)?;
        let decision = authorize_query(
            grant_session.grant(),
            &query.operation,
            requested_ranges,
            now,
        )
        .map_err(|error| {
            if operation_requires_full_range(query.operation.kind())
                && matches!(
                    error,
                    PolicyError::RangeDenied | PolicyError::RangeLimit | PolicyError::InvalidRange
                )
            {
                SharedServiceError::NotFound
            } else {
                map_policy(error)
            }
        })?;
        if operation_requires_full_range(query.operation.kind())
            && decision.requested_ranges != decision.effective_ranges
        {
            return Err(SharedServiceError::NotFound);
        }
        let executed = self.run_query(
            &queries,
            &search,
            &query.operation,
            &decision,
            &grant_session,
            now,
        )?;
        let mut response = QueryResponse {
            schema_version: "1.0".to_owned(),
            request_id: query.request_id.clone(),
            operation: query.operation.kind(),
            generated_at: now,
            store_generation: query.store_generation,
            grant: grant_summary(grant_session.grant()),
            scope: QueryScope {
                requested_ranges: decision.requested_ranges.clone(),
                effective_ranges: decision.effective_ranges.clone(),
                content_classes: decision.content_classes.clone(),
                ocr_included: decision.include_ocr,
            },
            page: executed.page,
            stable_cutoff: now,
            coverage: executed.coverage,
            provenance: QueryProvenance {
                query_engine_version: env!("CARGO_PKG_VERSION").to_owned(),
                schema_build_id: SERVICE_SCHEMA_BUILD_ID.to_owned(),
                projection_build_id: chronicle_store::STORE_BUILD_ID.to_owned(),
                sqlite_version: chronicle_store::SQLITE_BUNDLED_VERSION.to_owned(),
                sqlite_source_id: chronicle_store::SQLITE_BUNDLED_SOURCE_ID.to_owned(),
                source_event_ids: executed.source_event_ids,
                source_chunk_revision_ids: executed.source_chunk_revision_ids,
            },
            result: executed.result,
        };
        if let (Some(raw), Some(page)) = (executed.raw_next_cursor, response.page.as_mut()) {
            let token = grant_session
                .stage_cursor(query.operation.kind(), decision.cursor_scope_digest, raw)
                .map_err(map_store)?;
            page.next_cursor = Some(token);
        }
        drop(search);
        drop(queries);
        let mut shared = SharedServiceResponse {
            schema_version: "1.0".to_owned(),
            request_id: query.request_id,
            generated_at: now,
            store_generation: query.store_generation,
            result: SharedServiceResult::Query(Box::new(response)),
        };
        charge_response(&mut shared, &mut grant_session)?;
        shared.validate().map_err(SharedServiceError::Contract)?;
        let committed = grant_session.commit().map_err(map_store)?;
        if let SharedServiceResult::Query(response) = &mut shared.result {
            response.grant = grant_summary(&committed);
        }
        shared.validate().map_err(SharedServiceError::Contract)?;
        Ok(shared)
    }

    fn requested_ranges(
        &self,
        queries: &StoreQueries,
        operation: &QueryOperation,
    ) -> Result<Vec<UtcRange>, SharedServiceError> {
        match operation {
            QueryOperation::ListChunks { filter, .. }
            | QueryOperation::SearchActivity { filter, .. }
            | QueryOperation::Statistics { filter }
            | QueryOperation::BuildContextPacket { filter, .. } => Ok(vec![filter.range.clone()]),
            QueryOperation::ComparePeriods { first, second } => {
                Ok(vec![first.clone(), second.clone()])
            }
            QueryOperation::ListDerived { range, .. } => Ok(vec![range.clone()]),
            QueryOperation::ReadChunk { chunk_id }
            | QueryOperation::SupportingEvidence { chunk_id, .. } => {
                let chunk = queries
                    .current_chunk(chunk_id)
                    .map_err(map_store)?
                    .ok_or(SharedServiceError::NotFound)?;
                Ok(vec![chunk_window_range(&chunk)])
            }
            QueryOperation::GetEvent { event_id } => {
                let event = queries
                    .event(event_id, false)
                    .map_err(map_store)?
                    .ok_or(SharedServiceError::NotFound)?;
                Ok(vec![event_range(&event)?])
            }
            QueryOperation::InspectMoment { at } => Ok(vec![bucket_range(*at)?]),
            QueryOperation::Status | QueryOperation::Schemas => Ok(Vec::new()),
            QueryOperation::GetArtifact { .. } => Err(SharedServiceError::UnsupportedOperation),
        }
    }

    fn run_query(
        &self,
        queries: &StoreQueries,
        search: &ActivitySearch,
        operation: &QueryOperation,
        decision: &PolicyDecision,
        grants: &GrantQuerySession,
        now: DateTime<Utc>,
    ) -> Result<ExecutedQuery, SharedServiceError> {
        match operation {
            QueryOperation::Status => {
                let metrics =
                    store_health_metrics(&self.root, &self.sqlite, now).map_err(map_store)?;
                Ok(ExecutedQuery::unpaged(QueryResult::Status(QueryStatus {
                    recording_available: storage_available_bytes(&self.root).map_err(map_store)?
                        > 0
                        && metrics.latest.last_scheduled_attempt_at.is_some(),
                    projection_current: metrics.projection_pending_records == 0,
                    latest_recorded_at: metrics.latest.last_projection_at,
                })))
            }
            QueryOperation::Schemas => Ok(ExecutedQuery::unpaged(QueryResult::Schemas {
                schemas: vec![
                    SchemaDescriptor {
                        name: "event".to_owned(),
                        major_version: 1,
                        schema_id: "open-chronicle/event/v1".to_owned(),
                    },
                    SchemaDescriptor {
                        name: "chunk".to_owned(),
                        major_version: 1,
                        schema_id: "open-chronicle/chunk/v1".to_owned(),
                    },
                    SchemaDescriptor {
                        name: "query".to_owned(),
                        major_version: 1,
                        schema_id: "open-chronicle/query/v1".to_owned(),
                    },
                ],
            })),
            QueryOperation::ListChunks { filter, page } => {
                let range = only_range(decision)?;
                let raw_cursor = resolve_cursor(grants, operation, decision, page)?;
                let page_limit = decision
                    .page_limit
                    .ok_or_else(|| SharedServiceError::Contract("missing page limit".to_owned()))?;
                let mut bounded_filter = filter.clone();
                bounded_filter.range = range.clone();
                let (chunks, truncated) = queries
                    .current_chunk_page(&bounded_filter, raw_cursor.as_deref(), page_limit)
                    .map_err(map_store)?;
                let raw_next = truncated
                    .then(|| chunks.last().map(|chunk| chunk.chunk_id.to_string()))
                    .flatten();
                let source_chunk_revision_ids = chunks
                    .iter()
                    .map(|chunk| chunk.revision_id.clone())
                    .collect();
                let summaries = chunks.iter().map(chunk_summary).collect::<Vec<_>>();
                let coverage = self.coverage(queries, range)?;
                Ok(ExecutedQuery {
                    result: QueryResult::ChunkList { chunks: summaries },
                    page: Some(PageInfo {
                        next_cursor: None,
                        returned_items: u32::try_from(chunks.len()).unwrap_or(u32::MAX),
                        truncated,
                    }),
                    coverage: Some(coverage),
                    source_event_ids: Vec::new(),
                    source_chunk_revision_ids,
                    raw_next_cursor: raw_next,
                })
            }
            QueryOperation::ReadChunk { chunk_id } => {
                let mut chunk = queries
                    .current_chunk(chunk_id)
                    .map_err(map_store)?
                    .ok_or(SharedServiceError::NotFound)?;
                if !decision.include_ocr {
                    chunk.ocr_extracts.clear();
                }
                let source_event_ids = chunk.supporting_event_ids.clone();
                let source_chunk_revision_ids = vec![chunk.revision_id.clone()];
                let images = self.chunk_images(queries, &chunk)?;
                let coverage = self.coverage(queries, &chunk_window_range(&chunk))?;
                Ok(ExecutedQuery {
                    result: QueryResult::Chunk {
                        chunk: Box::new(chunk),
                        images,
                    },
                    page: None,
                    coverage: Some(coverage),
                    source_event_ids,
                    source_chunk_revision_ids,
                    raw_next_cursor: None,
                })
            }
            QueryOperation::GetEvent { event_id } => {
                let event = queries
                    .event(event_id, decision.include_ocr)
                    .map_err(map_store)?
                    .ok_or(SharedServiceError::NotFound)?;
                let coverage = self.coverage(queries, only_range(decision)?)?;
                Ok(ExecutedQuery {
                    result: QueryResult::Event {
                        event: Box::new(event.clone()),
                    },
                    page: None,
                    coverage: Some(coverage),
                    source_event_ids: vec![event.event_id],
                    source_chunk_revision_ids: Vec::new(),
                    raw_next_cursor: None,
                })
            }
            QueryOperation::SearchActivity {
                filter,
                query,
                page,
                ..
            } => {
                let mut filter = filter.clone();
                filter.range = only_range(decision)?.clone();
                let raw_cursor = resolve_cursor(grants, operation, decision, page)?;
                let bounded_page = PageRequest {
                    cursor: raw_cursor,
                    limit: decision.page_limit.ok_or_else(|| {
                        SharedServiceError::Contract("missing page limit".to_owned())
                    })?,
                };
                let search = search
                    .search(&filter, query, decision.include_ocr, &bounded_page)
                    .map_err(map_store)?;
                let events = search
                    .hits
                    .into_iter()
                    .map(|hit| hit.event)
                    .collect::<Vec<_>>();
                let source_event_ids: Vec<_> =
                    events.iter().map(|event| event.event_id.clone()).collect();
                let raw_next_cursor = search.page.next_cursor;
                let coverage = self.coverage(queries, &filter.range)?;
                Ok(ExecutedQuery {
                    result: QueryResult::Search { events },
                    page: Some(PageInfo {
                        next_cursor: None,
                        returned_items: search.page.returned_items,
                        truncated: search.page.truncated,
                    }),
                    coverage: Some(coverage),
                    source_event_ids,
                    source_chunk_revision_ids: Vec::new(),
                    raw_next_cursor,
                })
            }
            QueryOperation::InspectMoment { .. } => {
                let range = only_range(decision)?.clone();
                let events = queries
                    .bounded_query_events_in_range(&range, decision.include_ocr, MAX_MOMENT_EVENTS)
                    .map_err(map_store)?;
                let source_event_ids = events.iter().map(|event| event.event_id.clone()).collect();
                Ok(ExecutedQuery {
                    result: QueryResult::Moment { events },
                    page: None,
                    coverage: Some(self.coverage(queries, &range)?),
                    source_event_ids,
                    source_chunk_revision_ids: Vec::new(),
                    raw_next_cursor: None,
                })
            }
            QueryOperation::Statistics { .. } => {
                let range = only_range(decision)?;
                let report = FactualStatistics::new(queries.clone())
                    .range(range)
                    .map_err(map_store)?;
                Ok(ExecutedQuery {
                    result: QueryResult::Statistics {
                        factual_totals: report.factual_totals,
                    },
                    page: None,
                    coverage: Some(report.coverage),
                    source_event_ids: Vec::new(),
                    source_chunk_revision_ids: report.source_chunk_revision_ids,
                    raw_next_cursor: None,
                })
            }
            QueryOperation::ComparePeriods { .. } => {
                let [first_range, second_range] = decision.effective_ranges.as_slice() else {
                    return Err(SharedServiceError::Contract(
                        "comparison requires two effective ranges".to_owned(),
                    ));
                };
                let stats = FactualStatistics::new(queries.clone());
                let first = stats.range(first_range).map_err(map_store)?;
                let second = stats.range(second_range).map_err(map_store)?;
                let mut source_chunk_revision_ids = first.source_chunk_revision_ids;
                source_chunk_revision_ids.extend(second.source_chunk_revision_ids);
                source_chunk_revision_ids.sort();
                source_chunk_revision_ids.dedup();
                Ok(ExecutedQuery {
                    result: QueryResult::Comparison {
                        first: first.coverage,
                        second: second.coverage,
                    },
                    page: None,
                    coverage: None,
                    source_event_ids: Vec::new(),
                    source_chunk_revision_ids,
                    raw_next_cursor: None,
                })
            }
            QueryOperation::SupportingEvidence { chunk_id, page } => {
                let events = queries
                    .supporting_events(chunk_id, decision.include_ocr)
                    .map_err(map_store)?;
                let raw_cursor = resolve_cursor(grants, operation, decision, page)?;
                let (events, raw_next, truncated) = paginate_events(
                    events,
                    raw_cursor.as_deref(),
                    decision.page_limit.ok_or_else(|| {
                        SharedServiceError::Contract("missing page limit".to_owned())
                    })?,
                )?;
                let source_event_ids: Vec<_> =
                    events.iter().map(|event| event.event_id.clone()).collect();
                let chunk = queries
                    .current_chunk(chunk_id)
                    .map_err(map_store)?
                    .ok_or(SharedServiceError::NotFound)?;
                Ok(ExecutedQuery {
                    result: QueryResult::SupportingEvidence { events },
                    page: Some(PageInfo {
                        next_cursor: None,
                        returned_items: u32::try_from(source_event_ids.len()).unwrap_or(u32::MAX),
                        truncated,
                    }),
                    coverage: Some(self.coverage(queries, &chunk_window_range(&chunk))?),
                    source_event_ids,
                    source_chunk_revision_ids: vec![chunk.revision_id],
                    raw_next_cursor: raw_next,
                })
            }
            QueryOperation::GetArtifact { .. }
            | QueryOperation::BuildContextPacket { .. }
            | QueryOperation::ListDerived { .. } => Err(SharedServiceError::UnsupportedOperation),
        }
    }

    fn coverage(
        &self,
        queries: &StoreQueries,
        range: &UtcRange,
    ) -> Result<QueryCoverage, SharedServiceError> {
        Ok(FactualStatistics::new(queries.clone())
            .range(range)
            .map_err(map_store)?
            .coverage)
    }

    fn chunk_images(
        &self,
        queries: &StoreQueries,
        chunk: &ChunkRevision,
    ) -> Result<Vec<ImageMetadata>, SharedServiceError> {
        let events = queries
            .supporting_events(&chunk.chunk_id, false)
            .map_err(map_store)?;
        let mut images = Vec::new();
        for event in events {
            let QueryEventPayload::ObservationAttempt(attempt) = event.payload else {
                continue;
            };
            let image = match attempt.content {
                QueryObservationContent::Captured { image, .. }
                | QueryObservationContent::Unchanged { image, .. } => image,
                QueryObservationContent::Protected(_) | QueryObservationContent::NoEvidence(_) => {
                    None
                }
            };
            if let Some(image) = image
                && !images
                    .iter()
                    .any(|existing: &ImageMetadata| existing.artifact_id == image.artifact_id)
            {
                images.push(image);
            }
        }
        Ok(images)
    }

    fn health(
        &self,
        now: DateTime<Utc>,
        store_generation: u64,
        mcp: McpHealthSummary,
        storage: StorageHealthSummary,
    ) -> Result<DiagnosticHealthSnapshot, SharedServiceError> {
        let metrics = store_health_metrics(&self.root, &self.sqlite, now).map_err(map_store)?;
        let mut issues = Vec::new();
        if storage.available_bytes == 0 {
            issues.push(HealthIssue {
                severity: HealthSeverity::Critical,
                code: HealthCode::StorageUnavailable,
            });
        }
        if metrics.projection_pending_records > 0 {
            issues.push(HealthIssue {
                severity: HealthSeverity::Warning,
                code: HealthCode::ProjectionLag,
            });
        }
        let health = DiagnosticHealthSnapshot {
            schema_version: "1.0".to_owned(),
            observed_at: now,
            store_generation,
            projection: if metrics.projection_pending_records == 0 {
                ProjectionHealth::Current
            } else {
                ProjectionHealth::Lagging
            },
            acknowledgement: if metrics.projection_pending_records == 0 {
                chronicle_domain::DurableAcknowledgement::Durable
            } else {
                chronicle_domain::DurableAcknowledgement::JournalDurableProjectionPending
            },
            latest: metrics.latest,
            aggregation_watermark: metrics.aggregation_watermark,
            aggregation_pending_buckets: metrics.aggregation_pending_buckets,
            projection_lag_seconds: metrics.projection_lag_seconds,
            projection_pending_records: metrics.projection_pending_records,
            storage,
            mcp,
            issues,
        };
        health.validate().map_err(SharedServiceError::Contract)?;
        Ok(health)
    }
}

#[derive(Debug)]
struct ExecutedQuery {
    result: QueryResult,
    page: Option<PageInfo>,
    coverage: Option<QueryCoverage>,
    source_event_ids: Vec<chronicle_domain::EventId>,
    source_chunk_revision_ids: Vec<chronicle_domain::ChunkRevisionId>,
    raw_next_cursor: Option<String>,
}

impl ExecutedQuery {
    fn unpaged(result: QueryResult) -> Self {
        Self {
            result,
            page: None,
            coverage: None,
            source_event_ids: Vec::new(),
            source_chunk_revision_ids: Vec::new(),
            raw_next_cursor: None,
        }
    }
}

fn charge_response(
    response: &mut SharedServiceResponse,
    grant_session: &mut GrantQuerySession,
) -> Result<(), SharedServiceError> {
    let mut prior_size = u64::MAX;
    for _ in 0..8 {
        let bytes = u64::try_from(
            serde_json::to_vec(response)
                .map_err(|error| SharedServiceError::Contract(error.to_string()))?
                .len(),
        )
        .map_err(|_| SharedServiceError::ResponseByteLimit)?;
        grant_session
            .stage_disclosed_bytes(bytes)
            .map_err(map_store)?;
        if let SharedServiceResult::Query(query) = &mut response.result {
            query.grant = grant_summary(grant_session.grant());
        }
        if bytes == prior_size {
            return Ok(());
        }
        prior_size = bytes;
    }
    Err(SharedServiceError::ResponseByteLimit)
}

fn grant_summary(grant: &DisclosureGrant) -> GrantSummary {
    let mut capabilities = Vec::new();
    if grant.content_classes.contains(&ContentClass::Metadata) {
        capabilities.push(QueryCapability::Metadata);
    }
    if grant.content_classes.contains(&ContentClass::Ocr) {
        capabilities.push(QueryCapability::Ocr);
    }
    if grant.content_classes.contains(&ContentClass::Derived) {
        capabilities.push(QueryCapability::DerivedRead);
    }
    GrantSummary {
        grant_id: grant.grant_id.clone(),
        client_id: grant.client_id.clone(),
        receipt_id: grant.receipt_id.clone(),
        state: grant.state,
        time_scope: grant.time_scope.clone(),
        created_at: grant.created_at,
        expires_at: grant.expires_at,
        content_classes: grant.content_classes.clone(),
        capabilities,
        limits: grant.limits.clone(),
        remaining_cumulative_bytes: grant
            .limits
            .max_cumulative_bytes
            .saturating_sub(grant.disclosed_bytes),
        disclosed_bytes: grant.disclosed_bytes,
        store_generation: grant.store_generation,
    }
}

fn resolve_cursor(
    grants: &GrantQuerySession,
    operation: &QueryOperation,
    decision: &PolicyDecision,
    page: &PageRequest,
) -> Result<Option<String>, SharedServiceError> {
    page.cursor
        .as_deref()
        .map(|cursor| {
            grants
                .resolve_cursor(cursor, operation.kind(), &decision.cursor_scope_digest)
                .map_err(map_store)
        })
        .transpose()
}

fn only_range(decision: &PolicyDecision) -> Result<&UtcRange, SharedServiceError> {
    let [range] = decision.effective_ranges.as_slice() else {
        return Err(SharedServiceError::Contract(
            "operation requires exactly one effective range".to_owned(),
        ));
    };
    Ok(range)
}

fn bucket_range(at: DateTime<Utc>) -> Result<UtcRange, SharedServiceError> {
    let epoch = at.timestamp() - at.timestamp().rem_euclid(300);
    let start = DateTime::from_timestamp(epoch, 0).ok_or(SharedServiceError::RangeDenied)?;
    let end = start
        .checked_add_signed(Duration::seconds(300))
        .ok_or(SharedServiceError::RangeDenied)?;
    Ok(UtcRange { start, end })
}

fn event_range(event: &QueryEvent) -> Result<UtcRange, SharedServiceError> {
    let mut earliest = event.observed_at;
    if let Some(scheduled_at) = event.scheduled_at {
        earliest = earliest.min(scheduled_at);
    }
    let mut interval_end = None;
    if let QueryEventPayload::RecordingGap(gap) = &event.payload {
        earliest = earliest.min(gap.start);
        interval_end = Some(gap.end);
    }
    let start_epoch = earliest.timestamp() - earliest.timestamp().rem_euclid(300);
    // Observed/scheduled timestamps are instants in a half-open response range,
    // so their enclosing bucket must always end strictly after the instant.
    let latest_instant = event
        .scheduled_at
        .unwrap_or(event.observed_at)
        .max(event.observed_at);
    let instant_end_epoch = latest_instant
        .timestamp()
        .checked_sub(latest_instant.timestamp().rem_euclid(300))
        .and_then(|value| value.checked_add(300))
        .ok_or(SharedServiceError::RangeDenied)?;
    let interval_end_epoch = interval_end.map_or(i64::MIN, |end| {
        let remainder = end.timestamp().rem_euclid(300);
        if remainder == 0 {
            end.timestamp()
        } else {
            end.timestamp().saturating_add(300 - remainder)
        }
    });
    let end_epoch = instant_end_epoch.max(interval_end_epoch);
    let start = DateTime::from_timestamp(start_epoch, 0).ok_or(SharedServiceError::RangeDenied)?;
    let end = DateTime::from_timestamp(end_epoch, 0).ok_or(SharedServiceError::RangeDenied)?;
    Ok(UtcRange { start, end })
}

fn chunk_summary(chunk: &ChunkRevision) -> ChunkSummary {
    ChunkSummary {
        chunk_id: chunk.chunk_id.clone(),
        revision_id: chunk.revision_id.clone(),
        start: chunk.window.start,
        end: chunk.window.end,
        evidence_seconds: chunk.evidence_seconds.clone(),
        presence_seconds: chunk.presence_seconds.clone(),
        late_input: chunk.late_input,
    }
}

fn chunk_window_range(chunk: &ChunkRevision) -> UtcRange {
    UtcRange {
        start: chunk.window.start,
        end: chunk.window.end,
    }
}

fn paginate_events(
    events: Vec<QueryEvent>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<(Vec<QueryEvent>, Option<String>, bool), SharedServiceError> {
    let start = if let Some(cursor) = cursor {
        events
            .iter()
            .position(|event| event.event_id.as_str() == cursor)
            .map(|position| position + 1)
            .ok_or(SharedServiceError::CursorScopeMismatch)?
    } else {
        0
    };
    let limit = usize::try_from(limit).map_err(|_| SharedServiceError::RangeLimit)?;
    let end = start.saturating_add(limit).min(events.len());
    let truncated = end < events.len();
    let page = events[start..end].to_vec();
    let next = truncated
        .then(|| page.last().map(|event| event.event_id.to_string()))
        .flatten();
    Ok((page, next, truncated))
}

fn map_policy(error: PolicyError) -> SharedServiceError {
    match error {
        PolicyError::ContentDenied(class) => SharedServiceError::ContentDenied(class),
        PolicyError::RangeDenied | PolicyError::InvalidRange => SharedServiceError::RangeDenied,
        PolicyError::RangeLimit => SharedServiceError::RangeLimit,
    }
}

fn map_store(error: StoreError) -> SharedServiceError {
    match error {
        StoreError::GrantNotFound => SharedServiceError::GrantNotFound,
        StoreError::GrantClientMismatch => SharedServiceError::GrantClientMismatch,
        StoreError::GrantInactive => SharedServiceError::GrantInactive,
        StoreError::CursorNotFound => SharedServiceError::CursorNotFound,
        StoreError::CursorScopeMismatch => SharedServiceError::CursorScopeMismatch,
        StoreError::DisclosureByteLimit => SharedServiceError::ResponseByteLimit,
        StoreError::StaleGeneration { expected, actual } => {
            SharedServiceError::StaleGeneration { expected, actual }
        }
        other => SharedServiceError::Store(other),
    }
}
