mod common;

use std::error::Error;
use std::sync::{Arc, Barrier};

use chronicle_domain::{
    ArtifactId, ArtifactRevisionId, ArtifactStatus, ArtifactType, AuthorIdentity, AuthorKind,
    ClientId, ContentClass, DerivedArtifactRevision, DerivedArtifactWriteRequest, DisclosureGrant,
    DisclosureLimits, EventId, EvidenceReferences, GrantId, GrantState, GrantTimeScope,
    QueryOperation, QueryRequest, ReceiptId, RequestId, SharedServiceOperation,
    SharedServiceRequest, SharedServiceResult, UtcRange,
};
use chronicle_engine::{SharedService, SharedServiceError};
use chronicle_store::{ArtifactStore, FaultInjector, FaultPoint, StoreQueries};
use chrono::{DateTime, Utc};
use serde_json::json;

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid UTC timestamp")
}

fn range() -> UtcRange {
    UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:10:00Z"),
    }
}

fn grant(id: &str) -> DisclosureGrant {
    DisclosureGrant {
        schema_version: "1.0".to_owned(),
        grant_id: GrantId::new(id).expect("grant ID"),
        client_id: ClientId::new("client-codex").expect("client ID"),
        receipt_id: ReceiptId::new(format!("receipt-{id}")).expect("receipt ID"),
        time_scope: GrantTimeScope::Absolute { range: range() },
        content_classes: vec![ContentClass::Metadata, ContentClass::Derived],
        created_at: at("2026-07-13T08:00:00Z"),
        expires_at: at("2026-07-14T08:00:00Z"),
        state: GrantState::Active,
        limits: DisclosureLimits {
            max_page_items: 10,
            max_response_bytes: 64 * 1024,
            max_cumulative_bytes: 1024 * 1024,
        },
        disclosed_bytes: 0,
        store_generation: 1,
    }
}

fn revision(
    artifact_id: &str,
    revision_id: &str,
    prior: Option<&str>,
    status: ArtifactStatus,
    now: DateTime<Utc>,
) -> DerivedArtifactRevision {
    let prior = prior.map(|value| ArtifactRevisionId::new(value).expect("prior revision ID"));
    DerivedArtifactRevision {
        schema_version: "1.0".to_owned(),
        artifact_id: ArtifactId::new(artifact_id).expect("artifact ID"),
        revision_id: ArtifactRevisionId::new(revision_id).expect("revision ID"),
        prior_revision_id: prior.clone(),
        expected_prior_revision_id: prior,
        artifact_type: ArtifactType::Hypothesis,
        author: AuthorIdentity {
            kind: AuthorKind::Model,
            display_name: Some("Synthetic analyst".to_owned()),
            client_id: Some(ClientId::new("client-codex").expect("client ID")),
            model: Some("synthetic-model".to_owned()),
        },
        created_at: now,
        status,
        payload: json!({"statement": "A derived claim, not canonical evidence."}),
        evidence: EvidenceReferences {
            event_ids: vec![EventId::new("evt-090015").expect("event ID")],
            chunk_ids: vec![
                chronicle_domain::ChunkId::new("chunk-20260713T0900Z").expect("chunk ID"),
            ],
        },
        confidence: Some(0.5),
        store_generation: 1,
    }
}

fn write_request(
    request_id: &str,
    grant_id: &str,
    revision: DerivedArtifactRevision,
) -> SharedServiceRequest {
    let request_id = RequestId::new(request_id).expect("request ID");
    SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: request_id.clone(),
        store_generation: 1,
        operation: SharedServiceOperation::WriteDerived(Box::new(DerivedArtifactWriteRequest {
            schema_version: "1.0".to_owned(),
            request_id,
            client_id: ClientId::new("client-codex").expect("client ID"),
            grant_id: GrantId::new(grant_id).expect("grant ID"),
            store_generation: 1,
            revision,
        })),
    }
}

fn query_request(request: &str, grant: &str, operation: QueryOperation) -> SharedServiceRequest {
    let request_id = RequestId::new(request).expect("request ID");
    SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: request_id.clone(),
        store_generation: 1,
        operation: SharedServiceOperation::Query(Box::new(QueryRequest {
            schema_version: "1.0".to_owned(),
            request_id,
            client_id: ClientId::new("client-codex").expect("client ID"),
            grant_id: GrantId::new(grant).expect("grant ID"),
            store_generation: 1,
            operation,
        })),
    }
}

#[test]
fn referenced_revision_write_is_immutable_and_queryable() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    service.install_grant(grant("artifact-grant"))?;
    let now = at("2026-07-13T09:08:00Z");
    let response = service.execute(
        write_request(
            "write-artifact",
            "artifact-grant",
            revision(
                "artifact-hypothesis",
                "artifact-revision-1",
                None,
                ArtifactStatus::Draft,
                now,
            ),
        ),
        now,
    )?;
    assert!(matches!(
        response.result,
        SharedServiceResult::DerivedWritten(_)
    ));
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 1);

    for operation in [
        QueryOperation::GetArtifact {
            artifact_id: ArtifactId::new("artifact-hypothesis")?,
            revision_id: None,
        },
        QueryOperation::ListDerived {
            range: range(),
            page: chronicle_domain::PageRequest {
                cursor: None,
                limit: 10,
            },
        },
    ] {
        let response = service.execute(
            query_request("read-artifact", "artifact-grant", operation),
            at("2026-07-13T09:09:00Z"),
        )?;
        assert!(matches!(response.result, SharedServiceResult::Query(_)));
    }
    Ok(())
}

#[test]
fn dangling_refs_wrong_client_and_invalid_status_transition_write_nothing()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    service.install_grant(grant("artifact-errors"))?;
    let now = at("2026-07-13T09:08:00Z");

    let mut dangling = revision("dangling", "dangling-1", None, ArtifactStatus::Draft, now);
    dangling.evidence.event_ids = vec![EventId::new("event-does-not-exist")?];
    assert!(matches!(
        service.execute(write_request("dangling", "artifact-errors", dangling), now),
        Err(SharedServiceError::InvalidEvidenceReference)
    ));

    let mut out_of_scope = revision(
        "out-of-scope",
        "out-of-scope-1",
        None,
        ArtifactStatus::Draft,
        now,
    );
    out_of_scope.evidence.event_ids = vec![EventId::new("evt-img-missing")?];
    out_of_scope.evidence.chunk_ids.clear();
    assert!(matches!(
        service.execute(
            write_request("out-of-scope", "artifact-errors", out_of_scope),
            now
        ),
        Err(SharedServiceError::InvalidEvidenceReference)
    ));

    let mut wrong_client = revision(
        "wrong-client",
        "wrong-client-1",
        None,
        ArtifactStatus::Draft,
        now,
    );
    wrong_client.author.client_id = Some(ClientId::new("another-client")?);
    assert!(matches!(
        service.execute(
            write_request("wrong-client", "artifact-errors", wrong_client),
            now
        ),
        Err(SharedServiceError::GrantClientMismatch)
    ));

    let accepted_creation = revision(
        "accepted",
        "accepted-1",
        None,
        ArtifactStatus::Accepted,
        now,
    );
    assert!(matches!(
        service.execute(
            write_request("accepted", "artifact-errors", accepted_creation),
            now
        ),
        Err(SharedServiceError::InvalidArtifactTransition)
    ));
    assert!(ArtifactStore::new(root, projector).scan_all()?.is_empty());
    Ok(())
}

#[test]
fn concurrent_expected_prior_revision_has_exactly_one_winner() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    service.install_grant(grant("artifact-race"))?;
    let created = at("2026-07-13T09:08:00Z");
    service.execute(
        write_request(
            "artifact-base",
            "artifact-race",
            revision(
                "racing-artifact",
                "racing-base",
                None,
                ArtifactStatus::Draft,
                created,
            ),
        ),
        created,
    )?;

    let service = Arc::new(service);
    let barrier = Arc::new(Barrier::new(3));
    let handles = ["racing-child-a", "racing-child-b"].map(|revision_id| {
        let service = Arc::clone(&service);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let now = at("2026-07-13T09:08:30Z");
            barrier.wait();
            service.execute(
                write_request(
                    revision_id,
                    "artifact-race",
                    revision(
                        "racing-artifact",
                        revision_id,
                        Some("racing-base"),
                        ArtifactStatus::Accepted,
                        now,
                    ),
                ),
                now,
            )
        })
    });
    barrier.wait();
    let results = handles.map(|handle| handle.join().expect("writer thread"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(SharedServiceError::ArtifactConflict)))
            .count(),
        1
    );
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 2);
    Ok(())
}

#[test]
fn stale_expected_prior_is_a_conflict_even_when_the_winner_has_a_later_timestamp()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    service.install_grant(grant("artifact-stale-prior"))?;
    let created = at("2026-07-13T09:08:00Z");
    service.execute(
        write_request(
            "artifact-stale-prior-base-request",
            "artifact-stale-prior",
            revision(
                "artifact-stale-prior-value",
                "artifact-stale-prior-base",
                None,
                ArtifactStatus::Draft,
                created,
            ),
        ),
        created,
    )?;

    let winner_time = created + chrono::Duration::seconds(30);
    service.execute(
        write_request(
            "artifact-stale-prior-winner-request",
            "artifact-stale-prior",
            revision(
                "artifact-stale-prior-value",
                "artifact-stale-prior-winner",
                Some("artifact-stale-prior-base"),
                ArtifactStatus::Accepted,
                winner_time,
            ),
        ),
        winner_time,
    )?;

    let stale_time = created + chrono::Duration::seconds(10);
    assert!(matches!(
        service.execute(
            write_request(
                "artifact-stale-prior-loser-request",
                "artifact-stale-prior",
                revision(
                    "artifact-stale-prior-value",
                    "artifact-stale-prior-loser",
                    Some("artifact-stale-prior-base"),
                    ArtifactStatus::Accepted,
                    stale_time,
                ),
            ),
            stale_time,
        ),
        Err(SharedServiceError::ArtifactConflict)
    ));
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 2);
    Ok(())
}

#[test]
fn new_revision_rejects_wall_clock_regression_but_chain_order_remains_prior_linked()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    service.install_grant(grant("artifact-clock"))?;
    let created = at("2026-07-13T09:08:00Z");
    service.execute(
        write_request(
            "artifact-clock-base-request",
            "artifact-clock",
            revision(
                "artifact-clock-value",
                "artifact-clock-base",
                None,
                ArtifactStatus::Draft,
                created,
            ),
        ),
        created,
    )?;
    let earlier = created - chrono::Duration::seconds(1);
    assert!(matches!(
        service.execute(
            write_request(
                "artifact-clock-child-request",
                "artifact-clock",
                revision(
                    "artifact-clock-value",
                    "artifact-clock-child",
                    Some("artifact-clock-base"),
                    ArtifactStatus::Accepted,
                    earlier,
                ),
            ),
            earlier,
        ),
        Err(SharedServiceError::InvalidArtifactTransition)
    ));
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 1);
    Ok(())
}

#[test]
fn exact_request_retry_is_not_recharged_and_mutated_retry_is_rejected() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    let installed = grant("artifact-idempotent");
    service.install_grant(installed.clone())?;
    let created = at("2026-07-13T09:08:00Z");
    let request = write_request(
        "artifact-idempotent-request",
        "artifact-idempotent",
        revision(
            "artifact-idempotent-value",
            "artifact-idempotent-revision",
            None,
            ArtifactStatus::Draft,
            at("2001-01-01T00:00:00Z"),
        ),
    );
    let first = service.execute(request.clone(), created)?;
    let SharedServiceResult::DerivedWritten(first) = first.result else {
        return Err("expected derived write response".into());
    };
    assert_eq!(first.artifact.created_at, created);
    let charged_once = service.grant(&installed.grant_id)?.disclosed_bytes;
    assert!(charged_once > 0);

    service.execute(request.clone(), created + chrono::Duration::seconds(1))?;
    assert_eq!(
        service.grant(&installed.grant_id)?.disclosed_bytes,
        charged_once
    );

    let mut mutated = request;
    if let SharedServiceOperation::WriteDerived(write) = &mut mutated.operation {
        write.revision.payload = json!({"statement": "A different, uncommitted claim."});
    } else {
        return Err("write request fixture changed operation".into());
    }
    assert!(matches!(
        service.execute(mutated, created + chrono::Duration::seconds(2)),
        Err(SharedServiceError::ArtifactConflict)
    ));
    assert_eq!(
        service.grant(&installed.grant_id)?.disclosed_bytes,
        charged_once
    );
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 1);
    Ok(())
}

#[test]
fn narrower_grant_cannot_read_or_revise_out_of_scope_existing_artifact()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    let mut broad = grant("artifact-broad");
    broad.time_scope = GrantTimeScope::Absolute {
        range: UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T10:05:00Z"),
        },
    };
    service.install_grant(broad)?;
    service.install_grant(grant("artifact-narrow"))?;

    let broad_now = at("2026-07-13T10:01:00Z");
    let mut broad_revision = revision(
        "broad-artifact",
        "broad-artifact-1",
        None,
        ArtifactStatus::Draft,
        broad_now,
    );
    broad_revision.evidence.event_ids = vec![EventId::new("evt-img-missing")?];
    broad_revision.evidence.chunk_ids.clear();
    service.execute(
        write_request("broad-write", "artifact-broad", broad_revision),
        broad_now,
    )?;

    assert!(matches!(
        service.execute(
            query_request(
                "narrow-read",
                "artifact-narrow",
                QueryOperation::GetArtifact {
                    artifact_id: ArtifactId::new("broad-artifact")?,
                    revision_id: None,
                },
            ),
            at("2026-07-13T09:08:00Z"),
        ),
        Err(SharedServiceError::NotFound)
    ));

    let narrow_now = at("2026-07-13T09:08:00Z");
    let narrow_revision = revision(
        "broad-artifact",
        "broad-artifact-2",
        Some("broad-artifact-1"),
        ArtifactStatus::Accepted,
        narrow_now,
    );
    assert!(matches!(
        service.execute(
            write_request("narrow-write", "artifact-narrow", narrow_revision),
            narrow_now,
        ),
        Err(SharedServiceError::InvalidEvidenceReference)
    ));
    assert_eq!(
        service
            .grant(&GrantId::new("artifact-narrow")?)?
            .disclosed_bytes,
        0
    );
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 1);
    Ok(())
}

#[test]
fn canonical_unprojected_current_revision_still_blocks_out_of_scope_child()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite.clone())?;
    let mut broad = grant("artifact-unprojected-broad");
    broad.time_scope = GrantTimeScope::Absolute {
        range: UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T10:05:00Z"),
        },
    };
    service.install_grant(broad)?;
    service.install_grant(grant("artifact-unprojected-narrow"))?;

    let created = at("2026-07-13T09:08:00Z");
    let mut base = revision(
        "unprojected-artifact",
        "unprojected-base",
        None,
        ArtifactStatus::Draft,
        created,
    );
    base.evidence.event_ids = vec![EventId::new("evt-img-missing")?];
    base.evidence.chunk_ids.clear();
    let projection_fault = service.clone().with_write_faults(
        FaultInjector::at(FaultPoint::BeforeTransactionCommit),
        FaultInjector::none(),
    );
    assert!(
        projection_fault
            .execute(
                write_request(
                    "unprojected-base-request",
                    "artifact-unprojected-broad",
                    base,
                ),
                created,
            )
            .is_err()
    );
    assert!(
        StoreQueries::new(sqlite)
            .artifact(&ArtifactId::new("unprojected-artifact")?, None)?
            .is_none()
    );
    assert_eq!(
        ArtifactStore::new(root.clone(), projector.clone())
            .scan_all()?
            .len(),
        1
    );

    let child = revision(
        "unprojected-artifact",
        "unprojected-child",
        Some("unprojected-base"),
        ArtifactStatus::Accepted,
        created + chrono::Duration::seconds(1),
    );
    assert!(matches!(
        service.execute(
            write_request(
                "unprojected-child-request",
                "artifact-unprojected-narrow",
                child,
            ),
            created + chrono::Duration::seconds(1),
        ),
        Err(SharedServiceError::InvalidEvidenceReference)
    ));
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 1);
    Ok(())
}

#[test]
fn derived_write_receipt_ceiling_fails_before_canonical_write() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root.clone(), sqlite)?;
    let installed = grant("artifact-receipt-cap");
    service.install_grant(installed.clone())?;
    let mut receipt: serde_json::Value = serde_json::from_slice(&service.grant_receipt_bytes()?)?;
    receipt["derived_writes"] = serde_json::Value::Array(
        (0..4_096)
            .map(|index| {
                json!({
                    "request_id": format!("filled-request-{index}"),
                    "grant_id": installed.grant_id,
                    "client_id": installed.client_id,
                    "artifact_id": format!("filled-artifact-{index}"),
                    "revision_id": format!("filled-revision-{index}"),
                    "store_generation": 1,
                    "committed_at": "2026-07-13T09:00:00Z"
                })
            })
            .collect(),
    );
    root.atomic_write(
        "receipts/disclosure-grants.json",
        &serde_json::to_vec(&receipt)?,
    )?;

    let now = at("2026-07-13T09:08:00Z");
    let error = service
        .execute(
            write_request(
                "over-receipt-cap",
                "artifact-receipt-cap",
                revision(
                    "over-receipt-cap-artifact",
                    "over-receipt-cap-revision",
                    None,
                    ArtifactStatus::Draft,
                    now,
                ),
            ),
            now,
        )
        .expect_err("receipt cap must reject a new write");
    assert!(matches!(
        error,
        SharedServiceError::Store(chronicle_store::StoreError::InvalidPath(message))
            if message.contains("receipt limit")
    ));
    assert!(ArtifactStore::new(root, projector).scan_all()?.is_empty());
    Ok(())
}

#[test]
fn retry_repairs_projection_failure_after_immutable_artifact_rename() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let installed = grant("artifact-project-retry");
    let normal = SharedService::open(root.clone(), sqlite.clone())?;
    normal.install_grant(installed.clone())?;
    let created = at("2026-07-13T09:08:00Z");
    let request = write_request(
        "artifact-project-retry-request",
        "artifact-project-retry",
        revision(
            "artifact-project-retry-value",
            "artifact-project-retry-revision",
            None,
            ArtifactStatus::Draft,
            created,
        ),
    );
    let faulty = normal.clone().with_write_faults(
        FaultInjector::at(FaultPoint::BeforeTransactionCommit),
        FaultInjector::none(),
    );
    assert!(faulty.execute(request.clone(), created).is_err());
    assert_eq!(normal.grant(&installed.grant_id)?.disclosed_bytes, 0);
    assert_eq!(
        ArtifactStore::new(root.clone(), projector.clone())
            .scan_all()?
            .len(),
        1
    );
    assert!(
        StoreQueries::new(sqlite.clone())
            .artifact(
                &ArtifactId::new("artifact-project-retry-value")?,
                Some(&ArtifactRevisionId::new("artifact-project-retry-revision")?),
            )?
            .is_none()
    );

    normal.execute(request, created + chrono::Duration::seconds(1))?;
    assert!(normal.grant(&installed.grant_id)?.disclosed_bytes > 0);
    assert!(
        StoreQueries::new(sqlite)
            .artifact(
                &ArtifactId::new("artifact-project-retry-value")?,
                Some(&ArtifactRevisionId::new("artifact-project-retry-revision")?),
            )?
            .is_some()
    );
    assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 1);
    Ok(())
}

#[test]
fn retry_reconciles_receipt_failure_before_or_after_atomic_rename() -> Result<(), Box<dyn Error>> {
    for (suffix, receipt_fault) in [
        ("before", FaultPoint::BeforeTransactionCommit),
        ("after", FaultPoint::AfterArtifactRename),
    ] {
        let (_temporary, root, sqlite, projector) = common::store()?;
        common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
        common::seed_chunks(&root, &projector)?;
        let grant_id = format!("artifact-receipt-{suffix}");
        let installed = grant(&grant_id);
        let normal = SharedService::open(root.clone(), sqlite.clone())?;
        normal.install_grant(installed.clone())?;
        let created = at("2026-07-13T09:08:00Z");
        let request = write_request(
            &format!("artifact-receipt-{suffix}-request"),
            &grant_id,
            revision(
                &format!("artifact-receipt-{suffix}-value"),
                &format!("artifact-receipt-{suffix}-revision"),
                None,
                ArtifactStatus::Draft,
                created,
            ),
        );
        let faulty = normal
            .clone()
            .with_write_faults(FaultInjector::none(), FaultInjector::at(receipt_fault));
        assert!(faulty.execute(request.clone(), created).is_err());
        assert_eq!(
            ArtifactStore::new(root.clone(), projector.clone())
                .scan_all()?
                .len(),
            1
        );

        let before_retry = normal.grant(&installed.grant_id)?.disclosed_bytes;
        if receipt_fault == FaultPoint::AfterArtifactRename {
            let durability_probe = normal.clone().with_write_faults(
                FaultInjector::none(),
                FaultInjector::at(FaultPoint::AfterArtifactDirectorySync),
            );
            assert!(
                durability_probe
                    .execute(request.clone(), created + chrono::Duration::seconds(1))
                    .is_err()
            );
            assert_eq!(
                normal.grant(&installed.grant_id)?.disclosed_bytes,
                before_retry
            );
        }
        normal.execute(request.clone(), created + chrono::Duration::seconds(1))?;
        let after_retry = normal.grant(&installed.grant_id)?.disclosed_bytes;
        assert!(after_retry > 0);
        if receipt_fault == FaultPoint::AfterArtifactRename {
            assert_eq!(before_retry, after_retry);
        } else {
            assert_eq!(before_retry, 0);
        }
        normal.execute(request, created + chrono::Duration::seconds(2))?;
        assert_eq!(
            normal.grant(&installed.grant_id)?.disclosed_bytes,
            after_retry
        );
        assert_eq!(ArtifactStore::new(root, projector).scan_all()?.len(), 1);
    }
    Ok(())
}
