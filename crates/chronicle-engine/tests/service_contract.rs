mod common;

use std::error::Error;

use chronicle_domain::{
    ActivityFilter, ArtifactId, ClientId, ContentClass, DisclosureGrant, DisclosureLimits,
    EvidenceState, GrantId, GrantState, GrantTimeScope, PageRequest, ProjectionHealth,
    QueryOperation, QueryRequest, ReceiptId, RequestId, SharedServiceOperation,
    SharedServiceRequest, SharedServiceResult, UtcRange,
};
use chronicle_engine::{
    CadenceStamp, ChunkerConfig, IngestEngine, IngestRequest, SharedService, SharedServiceError,
};
use chronicle_store::{CanonicalJournal, FaultInjector, LockManager, StoreGeneration};
use chrono::{DateTime, Duration, SecondsFormat, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid UTC fixture timestamp")
}

fn grant(id: &str, client: &str, classes: Vec<ContentClass>, range: UtcRange) -> DisclosureGrant {
    DisclosureGrant {
        schema_version: "1.0".to_owned(),
        grant_id: GrantId::new(id).expect("grant ID"),
        client_id: ClientId::new(client).expect("client ID"),
        receipt_id: ReceiptId::new(format!("receipt-{id}")).expect("receipt ID"),
        time_scope: GrantTimeScope::Absolute { range },
        content_classes: classes,
        created_at: at("2026-07-13T08:00:00Z"),
        expires_at: at("2026-07-14T08:00:00Z"),
        state: GrantState::Active,
        limits: DisclosureLimits {
            max_page_items: 1,
            max_response_bytes: 64 * 1024,
            max_cumulative_bytes: 512 * 1024,
        },
        disclosed_bytes: 0,
        store_generation: 1,
    }
}

fn search_request(
    request: &str,
    client: &str,
    grant: &str,
    range: UtcRange,
    include_ocr: bool,
    cursor: Option<String>,
) -> SharedServiceRequest {
    let request_id = RequestId::new(request).expect("request ID");
    SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: request_id.clone(),
        store_generation: 1,
        operation: SharedServiceOperation::Query(Box::new(QueryRequest {
            schema_version: "1.0".to_owned(),
            request_id,
            client_id: ClientId::new(client).expect("client ID"),
            grant_id: GrantId::new(grant).expect("grant ID"),
            store_generation: 1,
            operation: QueryOperation::SearchActivity {
                filter: ActivityFilter {
                    range,
                    application_bundle_id: None,
                    window_text: None,
                    authorized_domain: None,
                    evidence_states: vec![EvidenceState::CapturedNew],
                },
                query: "synthetic".to_owned(),
                include_ocr,
                page: PageRequest { cursor, limit: 50 },
            },
        })),
    }
}

fn event_request(request: &str, client: &str, grant: &str) -> SharedServiceRequest {
    event_request_id(request, client, grant, "evt-090015")
}

fn event_request_id(
    request: &str,
    client: &str,
    grant: &str,
    event_id: &str,
) -> SharedServiceRequest {
    let request_id = RequestId::new(request).expect("request ID");
    SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: request_id.clone(),
        store_generation: 1,
        operation: SharedServiceOperation::Query(Box::new(QueryRequest {
            schema_version: "1.0".to_owned(),
            request_id,
            client_id: ClientId::new(client).expect("client ID"),
            grant_id: GrantId::new(grant).expect("grant ID"),
            store_generation: 1,
            operation: QueryOperation::GetEvent {
                event_id: chronicle_domain::EventId::new(event_id).expect("event ID"),
            },
        })),
    }
}

#[test]
fn schema_discovery_lists_every_published_u5_contract() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let service = SharedService::open(root, sqlite)?;
    service.install_grant(grant(
        "schema-discovery",
        "codex",
        vec![ContentClass::Metadata, ContentClass::Derived],
        UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T09:05:00Z"),
        },
    ))?;
    let request_id = RequestId::new("schema-discovery-request")?;
    let response = service.execute(
        SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: request_id.clone(),
            store_generation: 1,
            operation: SharedServiceOperation::Query(Box::new(QueryRequest {
                schema_version: "1.0".to_owned(),
                request_id,
                client_id: ClientId::new("codex")?,
                grant_id: GrantId::new("schema-discovery")?,
                store_generation: 1,
                operation: QueryOperation::Schemas,
            })),
        },
        at("2026-07-13T09:04:00Z"),
    )?;
    let SharedServiceResult::Query(query) = response.result else {
        return Err("expected query response".into());
    };
    let chronicle_domain::QueryResult::Schemas { schemas } = query.result else {
        return Err("expected schema response".into());
    };
    assert_eq!(
        schemas
            .iter()
            .map(|schema| (schema.name.as_str(), schema.schema_id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("event", "open-chronicle/event/v1"),
            ("chunk", "open-chronicle/chunk/v1"),
            ("derived-artifact", "open-chronicle/derived-artifact/v1"),
            ("query", "open-chronicle/query/v1"),
        ]
    );
    Ok(())
}

#[test]
fn no_grant_expiry_revoke_and_store_generation_fail_closed() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    let range = UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    };
    let request = search_request(
        "query-no-grant",
        "claude",
        "missing-grant",
        range.clone(),
        false,
        None,
    );
    assert!(matches!(
        service.execute(request, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::GrantNotFound)
    ));

    service.install_grant(grant(
        "grant-a",
        "claude",
        vec![ContentClass::Metadata, ContentClass::Ocr],
        range.clone(),
    ))?;
    let active = search_request(
        "query-active",
        "claude",
        "grant-a",
        range.clone(),
        false,
        None,
    );
    service.execute(active, at("2026-07-13T09:04:00Z"))?;
    service.revoke_grant(&GrantId::new("grant-a")?, at("2026-07-13T09:04:01Z"))?;
    let revoked = search_request(
        "query-revoked",
        "claude",
        "grant-a",
        range.clone(),
        false,
        None,
    );
    assert!(matches!(
        service.execute(revoked, at("2026-07-13T09:04:02Z")),
        Err(SharedServiceError::GrantInactive)
    ));

    let expired = grant(
        "grant-expired",
        "codex",
        vec![ContentClass::Metadata, ContentClass::Ocr],
        range.clone(),
    );
    service.install_grant(expired)?;
    let expired_request = search_request(
        "query-expired",
        "codex",
        "grant-expired",
        range,
        false,
        None,
    );
    assert!(matches!(
        service.execute(expired_request, at("2026-07-15T09:04:00Z")),
        Err(SharedServiceError::GrantInactive)
    ));

    let stale = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: RequestId::new("stale")?,
        store_generation: 2,
        operation: SharedServiceOperation::Health,
    };
    assert!(matches!(
        service.execute(stale, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::StaleGeneration { .. })
    ));
    Ok(())
}

#[test]
fn typed_entrypoint_enforces_versions_request_bytes_and_grant_schema() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let service = SharedService::open(root, sqlite)?;
    let mut wrong_version = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: RequestId::new("wrong-version")?,
        store_generation: 1,
        operation: SharedServiceOperation::Health,
    };
    wrong_version.schema_version = "2.0".to_owned();
    assert!(matches!(
        service.execute(wrong_version, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::Contract(_))
    ));

    let mut oversized = search_request(
        "oversized-typed-request",
        "claude",
        "not-consulted",
        UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T09:05:00Z"),
        },
        false,
        None,
    );
    let SharedServiceOperation::Query(oversized_query) = &mut oversized.operation else {
        return Err("expected query request".into());
    };
    let QueryOperation::SearchActivity { query, .. } = &mut oversized_query.operation else {
        return Err("expected search request".into());
    };
    *query = "x".repeat(70 * 1024);
    assert!(matches!(
        service.execute(oversized, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::Contract(_))
    ));

    let mut invalid_grant = grant(
        "wrong-grant-version",
        "claude",
        vec![ContentClass::Metadata],
        UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T09:05:00Z"),
        },
    );
    invalid_grant.schema_version = "2.0".to_owned();
    assert!(service.install_grant(invalid_grant).is_err());
    Ok(())
}

#[test]
fn range_ocr_page_response_cumulative_and_cursor_caps_are_enforced() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let service = SharedService::open(root, sqlite)?;
    let allowed = UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    };
    service.install_grant(grant(
        "grant-b",
        "claude",
        vec![ContentClass::Metadata, ContentClass::Ocr],
        allowed.clone(),
    ))?;

    let clipped = search_request(
        "query-clipped",
        "claude",
        "grant-b",
        UtcRange {
            start: allowed.start - Duration::hours(1),
            end: allowed.end + Duration::hours(1),
        },
        false,
        None,
    );
    let response = service.execute(clipped, at("2026-07-13T09:04:00Z"))?;
    let SharedServiceResult::Query(response) = response.result else {
        return Err("expected query response".into());
    };
    assert_eq!(response.scope.effective_ranges, vec![allowed.clone()]);
    assert!(
        response
            .page
            .as_ref()
            .is_some_and(|page| page.returned_items <= 2)
    );

    service.install_grant(grant(
        "grant-meta",
        "codex",
        vec![ContentClass::Metadata],
        allowed.clone(),
    ))?;
    let ocr = search_request(
        "query-ocr-denied",
        "codex",
        "grant-meta",
        allowed.clone(),
        false,
        None,
    );
    assert!(matches!(
        service.execute(ocr, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::ContentDenied(ContentClass::Ocr))
    ));
    assert_eq!(
        service.grant(&GrantId::new("grant-meta")?)?.disclosed_bytes,
        0,
        "denied OCR search disclosed or charged a result"
    );

    let first = search_request(
        "query-page-1",
        "claude",
        "grant-b",
        allowed.clone(),
        false,
        None,
    );
    let first = service.execute(first, at("2026-07-13T09:04:00Z"))?;
    let SharedServiceResult::Query(first) = first.result else {
        return Err("expected query response".into());
    };
    let cursor = first
        .page
        .and_then(|page| page.next_cursor)
        .ok_or("fixture search should paginate")?;
    assert!(
        !cursor.starts_with("evt-"),
        "cursor exposed its event position"
    );

    let escaped = search_request(
        "query-page-escape",
        "claude",
        "grant-b",
        UtcRange {
            start: allowed.start - Duration::minutes(5),
            end: allowed.end,
        },
        false,
        Some(cursor.clone()),
    );
    assert!(matches!(
        service.execute(escaped, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::CursorScopeMismatch)
    ));

    let second = search_request(
        "query-page-2",
        "claude",
        "grant-b",
        allowed.clone(),
        false,
        Some(cursor),
    );
    service.execute(second, at("2026-07-13T09:04:00Z"))?;

    let mut tiny = grant(
        "grant-tiny",
        "codex",
        vec![ContentClass::Metadata],
        allowed.clone(),
    );
    tiny.limits.max_response_bytes = 256;
    tiny.limits.max_cumulative_bytes = 256;
    tiny.content_classes.push(ContentClass::Ocr);
    service.install_grant(tiny)?;
    let too_large = search_request(
        "query-too-large",
        "codex",
        "grant-tiny",
        allowed,
        false,
        None,
    );
    assert!(matches!(
        service.execute(too_large, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::ResponseByteLimit)
    ));
    assert_eq!(
        service.grant(&GrantId::new("grant-tiny")?)?.disclosed_bytes,
        0
    );
    Ok(())
}

#[test]
fn evidence_detail_uses_ocr_only_when_the_grant_contains_ocr() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let service = SharedService::open(root, sqlite)?;
    let range = UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    };
    service.install_grant(grant(
        "detail-meta",
        "claude",
        vec![ContentClass::Metadata],
        range.clone(),
    ))?;
    service.install_grant(grant(
        "detail-ocr",
        "claude",
        vec![ContentClass::Metadata, ContentClass::Ocr],
        range,
    ))?;

    let metadata = service.execute(
        event_request("detail-meta-request", "claude", "detail-meta"),
        at("2026-07-13T09:04:00Z"),
    )?;
    let ocr = service.execute(
        event_request("detail-ocr-request", "claude", "detail-ocr"),
        at("2026-07-13T09:04:00Z"),
    )?;
    let has_ocr = |response: chronicle_domain::SharedServiceResponse| {
        let SharedServiceResult::Query(response) = response.result else {
            return false;
        };
        let chronicle_domain::QueryResult::Event { event } = response.result else {
            return false;
        };
        matches!(
            event.payload,
            chronicle_domain::QueryEventPayload::ObservationAttempt(attempt)
                if matches!(
                    attempt.content,
                    chronicle_domain::QueryObservationContent::Captured { ocr: Some(_), .. }
                )
        )
    };
    assert!(!has_ocr(metadata));
    assert!(has_ocr(ocr));
    Ok(())
}

#[test]
fn health_is_content_free_even_when_evidence_contains_titles_and_ocr() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let service = SharedService::open(root, sqlite)?;
    let request = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: RequestId::new("health")?,
        store_generation: 1,
        operation: SharedServiceOperation::Health,
    };
    let response = service.execute(request, at("2026-07-13T09:04:00Z"))?;
    let SharedServiceResult::Health(health) = response.result else {
        return Err("expected health response".into());
    };
    health.validate()?;
    let json = serde_json::to_string(&health)?;
    for evidence in [
        "Synthetic note",
        "com.example.editor",
        "Late synthetic observation",
    ] {
        assert!(!json.contains(evidence), "health disclosed {evidence}");
    }
    Ok(())
}

#[test]
fn health_survives_wall_clock_rollback_with_future_pending_journal_records()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let event = common::fixture_events("events.jsonl")?
        .into_iter()
        .next()
        .ok_or("missing event fixture")?;
    CanonicalJournal::new(root.clone()).append_event(&event, FaultInjector::none())?;
    let service = SharedService::open(root, sqlite)?;
    let observed_at = event.recorded_at - Duration::hours(1);
    let response = service.execute(
        SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: RequestId::new("health-after-clock-rollback")?,
            store_generation: 1,
            operation: SharedServiceOperation::Health,
        },
        observed_at,
    )?;
    let SharedServiceResult::Health(health) = response.result else {
        return Err("expected health response".into());
    };
    assert_eq!(health.projection, ProjectionHealth::Lagging);
    assert_eq!(health.projection_pending_records, 1);
    assert!(
        health
            .latest
            .last_journal_at
            .is_none_or(|timestamp| timestamp <= observed_at)
    );
    health.validate()?;
    Ok(())
}

#[test]
fn health_orders_and_filters_same_second_facts_at_full_precision() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let early = at("2026-07-13T09:00:00.100000000Z");
    let late = at("2026-07-13T09:00:00.900000000Z");
    let connection = sqlite.connection()?;
    for (stable_id, occurred_at) in [("same-second-early", early), ("same-second-late", late)] {
        connection.execute(
            "INSERT INTO health_operation_facts(
               fact_type, stable_id, occurred_at, occurred_epoch, occurred_subsec_nanos)
             VALUES('scheduled-attempt', ?1, ?2, ?3, ?4)",
            (
                stable_id,
                occurred_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
                occurred_at.timestamp(),
                occurred_at.timestamp_subsec_nanos(),
            ),
        )?;
    }
    drop(connection);
    let service = SharedService::open(root, sqlite)?;
    let health_at = |request_id: &str, observed_at: DateTime<Utc>| {
        let response = service.execute(
            SharedServiceRequest {
                schema_version: "1.0".to_owned(),
                request_id: RequestId::new(request_id).expect("request ID"),
                store_generation: 1,
                operation: SharedServiceOperation::Health,
            },
            observed_at,
        )?;
        let SharedServiceResult::Health(health) = response.result else {
            return Err(SharedServiceError::Contract(
                "expected health response".to_owned(),
            ));
        };
        Ok::<_, SharedServiceError>(health)
    };
    let before_late = health_at(
        "same-second-before-late",
        at("2026-07-13T09:00:00.500000000Z"),
    )?;
    assert_eq!(before_late.latest.last_scheduled_attempt_at, Some(early));
    let after_late = health_at(
        "same-second-after-late",
        at("2026-07-13T09:00:00.950000000Z"),
    )?;
    assert_eq!(after_late.latest.last_scheduled_attempt_at, Some(late));
    Ok(())
}

#[test]
fn health_rechecks_generation_after_maintenance_and_classifies_old_grants_as_stale()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let service = SharedService::open(root.clone(), sqlite)?;
    service.install_grant(grant(
        "old-generation",
        "claude",
        vec![ContentClass::Metadata],
        UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T09:05:00Z"),
        },
    ))?;
    let locks = LockManager::new(root.clone(), std::time::Duration::from_secs(2));
    let maintenance = locks.exclusive_maintenance()?;
    let racing_service = service.clone();
    let racing_health = std::thread::spawn(move || {
        racing_service.execute(
            SharedServiceRequest {
                schema_version: "1.0".to_owned(),
                request_id: RequestId::new("racing-health").expect("request ID"),
                store_generation: 1,
                operation: SharedServiceOperation::Health,
            },
            at("2026-07-13T09:04:00Z"),
        )
    });
    assert_eq!(
        StoreGeneration::load(&root)?.increment(&root)?.generation,
        2
    );
    drop(maintenance);
    assert!(matches!(
        racing_health.join().map_err(|_| "health thread panicked")?,
        Err(SharedServiceError::StaleGeneration {
            expected: 1,
            actual: 2
        })
    ));

    let current = SharedService::open(root.clone(), chronicle_store::SqliteStore::open(root)?)?;
    let response = current.execute(
        SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: RequestId::new("current-health")?,
            store_generation: 2,
            operation: SharedServiceOperation::Health,
        },
        at("2026-07-13T09:04:00Z"),
    )?;
    let SharedServiceResult::Health(health) = response.result else {
        return Err("expected health response".into());
    };
    assert_eq!(health.mcp.active_grants, 0);
    assert_eq!(health.mcp.stale_generation_grants, 1);
    Ok(())
}

#[test]
fn large_history_health_is_indexed_and_does_not_interrupt_capture() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let mut engine = IngestEngine::open(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "large-health-capture-1".to_owned(),
            max_cadence_seconds: 30,
        },
    )?;
    let service = SharedService::open(root, sqlite.clone())?;
    let connection = sqlite.connection()?;
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TEMP TABLE bulk_health_sequence(value INTEGER PRIMARY KEY);
         WITH digits(d) AS (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9))
         INSERT INTO bulk_health_sequence(value)
         SELECT a.d + 10*b.d + 100*c.d + 1000*d.d + 10000*e.d
         FROM digits a, digits b, digits c, digits d, digits e
         WHERE a.d + 10*b.d + 100*c.d + 1000*d.d + 10000*e.d < 50000;

         INSERT INTO events(event_id, checksum, kind, recorded_at, body_json)
         SELECT printf('bulk-health-%05d', value), printf('checksum-%05d', value),
                'observation-attempt',
                strftime('%Y-%m-%dT%H:%M:%S+00:00', 1700000000 + value*30, 'unixepoch'),
                json_object(
                  'scheduled_at', strftime('%Y-%m-%dT%H:%M:%S+00:00',
                                           1700000000 + value*30, 'unixepoch'),
                  'observed_at', strftime('%Y-%m-%dT%H:%M:%S+00:00',
                                          1700000000 + value*30, 'unixepoch'))
         FROM bulk_health_sequence;

         INSERT INTO observations(
           event_id, attempt_status, evidence_state, presence_state, ocr_state,
           application_bundle_id, process_name, window_title, authorized_domain,
           content_hash, ocr_text)
         SELECT printf('bulk-health-%05d', value), 'completed', 'captured-new',
                'active', 'complete', 'com.example.bulk', 'Bulk', NULL, NULL,
                printf('hash-%05d', value), NULL
         FROM bulk_health_sequence;

         WITH fact_types(fact_type) AS (
           VALUES('scheduled-attempt'), ('successful-capture'),
                 ('successful-ocr'), ('event-projected'))
         INSERT INTO health_operation_facts(
           fact_type, stable_id, occurred_at, occurred_epoch, occurred_subsec_nanos)
         SELECT fact_type, printf('bulk-health-%05d', value),
                strftime('%Y-%m-%dT%H:%M:%S+00:00', 1700000000 + value*30, 'unixepoch'),
                1700000000 + value*30, 0
         FROM bulk_health_sequence CROSS JOIN fact_types;
         DROP TABLE bulk_health_sequence;
         COMMIT;",
    )?;
    drop(connection);

    let event = common::fixture_events("ae4-ten-scheduled-events.jsonl")?
        .into_iter()
        .next()
        .ok_or("fixture empty")?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let health_barrier = barrier.clone();
    let health = std::thread::spawn(move || {
        health_barrier.wait();
        let started = std::time::Instant::now();
        let result = service.execute(
            SharedServiceRequest {
                schema_version: "1.0".to_owned(),
                request_id: RequestId::new("large-history-health").expect("request ID"),
                store_generation: 1,
                operation: SharedServiceOperation::Health,
            },
            at("2026-07-13T09:04:00Z"),
        );
        (started.elapsed(), result)
    });
    let ingest_barrier = barrier.clone();
    let ingest = std::thread::spawn(move || {
        ingest_barrier.wait();
        engine.ingest(
            IngestRequest {
                event: event.clone(),
                cadence: Some(CadenceStamp {
                    boot_sequence: "large-history-health".to_owned(),
                    monotonic_tick: 1,
                }),
            },
            event.recorded_at,
        )
    });
    barrier.wait();
    let (health_elapsed, health_result) = health.join().map_err(|_| "health thread panicked")?;
    assert!(matches!(
        health_result?.result,
        SharedServiceResult::Health(_)
    ));
    assert!(
        health_elapsed < std::time::Duration::from_secs(2),
        "indexed health exceeded the capture lock budget: {health_elapsed:?}"
    );
    assert_eq!(
        ingest
            .join()
            .map_err(|_| "ingest thread panicked")??
            .acknowledgement,
        chronicle_domain::DurableAcknowledgement::Durable
    );
    Ok(())
}

#[test]
fn derived_reads_without_derived_grant_do_not_charge_disclosure_bytes() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let service = SharedService::open(root, sqlite)?;
    let range = UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    };
    service.install_grant(grant(
        "future-slice",
        "claude",
        vec![ContentClass::Metadata],
        range,
    ))?;
    let request_id = RequestId::new("unsupported-artifact")?;
    let request = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: request_id.clone(),
        store_generation: 1,
        operation: SharedServiceOperation::Query(Box::new(QueryRequest {
            schema_version: "1.0".to_owned(),
            request_id,
            client_id: ClientId::new("claude")?,
            grant_id: GrantId::new("future-slice")?,
            store_generation: 1,
            operation: QueryOperation::GetArtifact {
                artifact_id: ArtifactId::new("future-artifact")?,
                revision_id: None,
            },
        })),
    };
    assert!(matches!(
        service.execute(request, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::ContentDenied(ContentClass::Derived))
    ));
    assert_eq!(
        service
            .grant(&GrantId::new("future-slice")?)?
            .disclosed_bytes,
        0
    );
    Ok(())
}

#[test]
fn cumulative_budget_is_durable_and_never_overruns() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let service = SharedService::open(root.clone(), sqlite.clone())?;
    let range = UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    };
    let mut bounded = grant(
        "grant-c",
        "codex",
        vec![ContentClass::Metadata, ContentClass::Ocr],
        range.clone(),
    );
    bounded.limits.max_response_bytes = 64 * 1024;
    bounded.limits.max_cumulative_bytes = 64 * 1024;
    service.install_grant(bounded)?;

    let mut blocked = false;
    for index in 0..100 {
        let request = search_request(
            &format!("cumulative-{index}"),
            "codex",
            "grant-c",
            range.clone(),
            false,
            None,
        );
        match service.execute(request, at("2026-07-13T09:04:00Z")) {
            Ok(_) => {}
            Err(SharedServiceError::ResponseByteLimit) => {
                blocked = true;
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    assert!(
        blocked,
        "cumulative grant never exhausted its bounded budget"
    );
    let before_reopen = service.grant(&GrantId::new("grant-c")?)?;
    assert!(before_reopen.disclosed_bytes > 0);
    assert!(before_reopen.disclosed_bytes <= before_reopen.limits.max_cumulative_bytes);
    drop(service);

    let reopened = SharedService::open(root, sqlite)?;
    let after_reopen = reopened.grant(&GrantId::new("grant-c")?)?;
    assert_eq!(after_reopen.disclosed_bytes, before_reopen.disclosed_bytes);
    Ok(())
}

#[test]
fn effective_ranges_over_thirty_one_days_are_rejected_before_querying() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let service = SharedService::open(root, sqlite)?;
    let start = at("2026-01-01T00:00:00Z");
    let end = start + Duration::days(32);
    let mut wide = grant(
        "grant-wide",
        "claude",
        vec![ContentClass::Metadata, ContentClass::Ocr],
        UtcRange { start, end },
    );
    wide.created_at = start - Duration::days(1);
    wide.expires_at = end + Duration::days(1);
    service.install_grant(wide)?;
    let request = search_request(
        "query-wide",
        "claude",
        "grant-wide",
        UtcRange { start, end },
        false,
        None,
    );
    assert!(matches!(
        service.execute(request, start + Duration::days(1)),
        Err(SharedServiceError::RangeLimit)
    ));
    Ok(())
}

#[test]
fn direct_id_reads_hide_out_of_scope_existence_and_cover_gap_instants() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let service = SharedService::open(root, sqlite)?;
    service.install_grant(grant(
        "direct-scope",
        "claude",
        vec![ContentClass::Metadata],
        UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T09:05:00Z"),
        },
    ))?;

    service.execute(
        event_request_id("gap-detail", "claude", "direct-scope", "evt-gap-sleep"),
        at("2026-07-13T09:04:00Z"),
    )?;

    let outside = event_request_id(
        "outside-detail",
        "claude",
        "direct-scope",
        "evt-img-user-complete",
    );
    let missing = event_request_id(
        "missing-detail",
        "claude",
        "direct-scope",
        "evt-does-not-exist",
    );
    assert!(matches!(
        service.execute(outside, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::NotFound)
    ));
    assert!(matches!(
        service.execute(missing, at("2026-07-13T09:04:00Z")),
        Err(SharedServiceError::NotFound)
    ));
    Ok(())
}

#[test]
fn query_charge_waits_for_reset_and_cannot_commit_the_old_generation() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    let range = UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    };
    service.install_grant(grant(
        "reset-query",
        "claude",
        vec![ContentClass::Metadata, ContentClass::Ocr],
        range.clone(),
    ))?;
    let locks = LockManager::new(root.clone(), std::time::Duration::from_secs(2));
    let maintenance = locks.exclusive_maintenance()?;
    let query_service = service.clone();
    let query = std::thread::spawn(move || {
        query_service.execute(
            search_request(
                "reset-racing-query",
                "claude",
                "reset-query",
                range,
                false,
                None,
            ),
            at("2026-07-13T09:04:00Z"),
        )
    });
    let generation = StoreGeneration::load(&root)?;
    assert_eq!(generation.increment(&root)?.generation, 2);
    drop(maintenance);
    assert!(matches!(
        query.join().map_err(|_| "query thread panicked")?,
        Err(SharedServiceError::StaleGeneration {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(
        service
            .grant(&GrantId::new("reset-query")?)?
            .disclosed_bytes,
        0
    );
    Ok(())
}
