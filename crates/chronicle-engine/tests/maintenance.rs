mod common;

use std::error::Error;
use std::time::Duration;

use chronicle_domain::{DerivedArtifactRevision, UtcRange};
use chronicle_engine::{
    AppMaintenance, ChunkerConfig, IngestEngine, SharedService, SharedServiceError,
};
use chronicle_store::{
    CanonicalJournal, EvidenceDeletionConfirmation, EvidenceDeletionOptions, EvidenceDeletionState,
    FaultInjector, MaintenanceFaultInjector, MaintenanceFaultPoint, RetentionConfirmation,
    ScreenshotStore, StoreError, StoreGeneration,
};
use chrono::{DateTime, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid timestamp")
}

fn range() -> UtcRange {
    UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    }
}

fn export_clock() -> DateTime<Utc> {
    at("2026-07-14T09:00:00Z")
}

fn stale_export_clock() -> DateTime<Utc> {
    at("2026-07-13T09:04:59Z")
}

fn fixture_artifact() -> Result<DerivedArtifactRevision, Box<dyn Error>> {
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/session-v1/queries.json"),
    )?)?;
    Ok(DerivedArtifactRevision::parse(&serde_json::to_string(
        &value["artifact"],
    )?)?)
}

#[test]
fn app_export_is_stable_checksummed_and_contains_no_managed_paths() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let maintenance =
        AppMaintenance::open(root, Duration::from_millis(100))?.with_clock(export_clock);
    let cutoff = export_clock();

    let first = maintenance.export_snapshot(range(), false, false, 512 * 1024)?;
    let second = maintenance.export_snapshot(range(), false, false, 512 * 1024)?;
    assert_eq!(first, second);
    assert_eq!(first.store_generation, 1);
    assert_eq!(first.stable_cutoff, cutoff);
    assert_eq!(first.included_counts.events, first.events.len() as u64);
    assert_eq!(first.included_counts.chunks, first.chunks.len() as u64);
    assert!(!first.journal_cutoffs.is_empty());
    assert_eq!(first.checksums.len(), 3);
    assert!(
        first
            .checksums
            .iter()
            .all(|checksum| checksum.sha256.len() == 64)
    );
    let encoded = serde_json::to_string(&first)?;
    assert!(!encoded.contains("managed_relative_path"));
    assert!(!encoded.contains("screenshots/"));
    Ok(())
}

#[test]
fn app_export_rejects_a_clock_cutoff_before_the_requested_range() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let maintenance =
        AppMaintenance::open(root, Duration::from_millis(100))?.with_clock(stale_export_clock);

    assert!(
        maintenance
            .export_snapshot(range(), false, false, 512 * 1024)
            .is_err()
    );
    Ok(())
}

#[test]
fn app_export_rejects_projection_pending_canonical_records() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let event = common::fixture_events("events.jsonl")?
        .into_iter()
        .next()
        .expect("fixture event");
    CanonicalJournal::new(root.clone()).append_event(&event, FaultInjector::none())?;
    let maintenance =
        AppMaintenance::open(root, Duration::from_millis(100))?.with_clock(export_clock);

    assert!(
        maintenance
            .export_snapshot(range(), false, false, 512 * 1024)
            .is_err(),
        "an export must not claim completeness while canonical records await projection"
    );
    Ok(())
}

#[test]
fn app_export_reconciles_canonical_derived_revision_missing_from_projection()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let artifact = fixture_artifact()?;
    let directory = format!("derived/{}", artifact.artifact_id);
    root.ensure_directory(&directory)?;
    root.atomic_write(
        &format!("{directory}/{}.json", artifact.revision_id),
        &chronicle_store::checksum::canonical_json(&artifact)?,
    )?;
    assert!(sqlite.snapshot_ids()?.artifact_revision_ids.is_empty());
    let maintenance =
        AppMaintenance::open(root, Duration::from_millis(100))?.with_clock(export_clock);
    let snapshot = maintenance.export_snapshot(
        UtcRange {
            start: at("2026-07-13T09:00:00Z"),
            end: at("2026-07-13T09:10:00Z"),
        },
        false,
        true,
        512 * 1024,
    )?;

    assert_eq!(snapshot.included_counts.artifacts, 1);
    assert_eq!(snapshot.available_counts.artifacts, 1);
    assert_eq!(snapshot.artifacts.len(), 1);
    assert_eq!(snapshot.artifacts[0].revision_id, artifact.revision_id);
    let derived_checksum = snapshot
        .checksums
        .iter()
        .find(|checksum| checksum.component == "derived-artifacts")
        .expect("derived checksum");
    assert_eq!(
        derived_checksum.sha256,
        chronicle_store::checksum::checksum_bytes(&chronicle_store::checksum::canonical_json(
            &snapshot.artifacts
        )?,)
    );
    assert_eq!(sqlite.snapshot_ids()?.artifact_revision_ids.len(), 1);
    Ok(())
}

#[test]
fn app_maintenance_composes_existing_retention_preview_and_apply() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::fixture_events("events.jsonl")?;
    ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    let maintenance = AppMaintenance::open(root.clone(), Duration::from_millis(100))?;

    let preview = maintenance.preview_retention(at("2026-07-14T09:00:16Z"))?;
    assert_eq!(preview.candidate_artifact_ids.len(), 1);
    let result = maintenance.apply_retention(
        RetentionConfirmation::confirmed(preview),
        at("2026-07-14T09:00:17Z"),
        FaultInjector::none(),
    )?;
    assert_eq!(result.deleted_artifact_ids.len(), 1);
    assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);
    assert!(root.exists("evidence/events/2026-07-13.jsonl")?);
    Ok(())
}

#[test]
fn app_evidence_deletion_preserves_settings_registration_and_grants() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, _sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    root.atomic_write("config.json", br#"{"recording_preference":false}"#)?;
    root.atomic_write("receipts/agent-registrations.json", br#"{"agents":[]}"#)?;
    root.atomic_write("receipts/disclosure-grants.json", br#"{"grants":[]}"#)?;
    let maintenance = AppMaintenance::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T10:00:00Z"),
    )?;

    let result = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T10:01:00Z"),
        MaintenanceFaultInjector::none(),
    )?;
    assert_eq!(result.receipt.state, EvidenceDeletionState::Complete);
    assert_eq!(StoreGeneration::load(&root)?.generation, 2);
    assert!(root.exists("config.json")?);
    assert!(root.exists("receipts/agent-registrations.json")?);
    assert!(root.exists("receipts/disclosure-grants.json")?);
    assert!(result.remaining_evidence.files.is_empty());
    Ok(())
}

#[test]
fn committed_deletion_fence_blocks_normal_engine_and_shared_service_open_until_resume()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, stale_sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    let maintenance = AppMaintenance::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T11:00:00Z"),
    )?;
    let interrupted = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T11:01:00Z"),
        MaintenanceFaultInjector::at(MaintenanceFaultPoint::AfterGenerationIncrement),
    );
    assert!(interrupted.is_err());

    let committed_receipt = root.read("receipts/evidence-deletion.json")?;
    let mut forged: serde_json::Value = serde_json::from_slice(&committed_receipt)?;
    forged["state"] = serde_json::Value::String("complete".to_owned());
    forged["committed_generation"] = serde_json::Value::from(2_u64);
    forged["completed_at"] = serde_json::Value::String("2026-07-14T11:01:30Z".to_owned());
    forged["deleted_relative_paths"] = serde_json::Value::Array(
        preview
            .deletion
            .files
            .iter()
            .map(|item| serde_json::Value::String(item.relative_path.clone()))
            .collect(),
    );
    root.atomic_write(
        "receipts/evidence-deletion.json",
        &serde_json::to_vec(&forged)?,
    )?;

    assert!(matches!(
        SharedService::open(root.clone(), stale_sqlite),
        Err(SharedServiceError::Store(StoreError::MaintenanceInProgress))
    ));
    assert!(matches!(
        IngestEngine::open(
            root.clone(),
            ChunkerConfig {
                aggregator_version: "maintenance-fence-1".to_owned(),
                max_cadence_seconds: 30,
            },
        ),
        Err(chronicle_engine::EngineError::Store(
            StoreError::MaintenanceInProgress
        ))
    ));

    root.atomic_write("receipts/evidence-deletion.json", &committed_receipt)?;
    let completed = maintenance
        .resume_evidence_deletion(at("2026-07-14T11:02:00Z"), MaintenanceFaultInjector::none())?;
    assert_eq!(completed.receipt.state, EvidenceDeletionState::Complete);
    let sqlite = chronicle_store::SqliteStore::open(root.clone())?;
    SharedService::open(root.clone(), sqlite)?;
    IngestEngine::open(
        root,
        ChunkerConfig {
            aggregator_version: "maintenance-fence-1".to_owned(),
            max_cadence_seconds: 30,
        },
    )?;
    Ok(())
}
