mod common;

use std::error::Error;

use chronicle_engine::{AggregationReconciler, ChunkerConfig};
use chronicle_store::{
    CanonicalJournal, FaultInjector, FaultPoint, JournalFamily, RecoveryManager, SqliteStore,
    StoreGeneration, StoreQueries,
};
use chrono::{DateTime, Utc};

const CHUNK_FAULTS: &[FaultPoint] = &[
    FaultPoint::AfterJournalAppend,
    FaultPoint::AfterJournalSync,
    FaultPoint::AfterRowInsert,
    FaultPoint::AfterCurrentPointerUpdate,
    FaultPoint::AfterWatermarkUpdate,
    FaultPoint::AfterCursorUpdate,
    FaultPoint::BeforeTransactionCommit,
    FaultPoint::AfterTransactionCommit,
];

#[test]
fn every_chunk_crash_boundary_recovers_one_current_revision() -> Result<(), Box<dyn Error>> {
    let now: DateTime<Utc> = "2026-07-13T09:05:30Z".parse()?;
    for point in CHUNK_FAULTS {
        let (_temporary, root, sqlite, projector) = common::store()?;
        let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
        common::seed_events(&root, &projector, &events)?;
        let config = ChunkerConfig {
            aggregator_version: "crash-reconcile-1".to_owned(),
            max_cadence_seconds: 30,
        };
        let reconciler = AggregationReconciler::new(root.clone(), sqlite, config.clone());
        assert!(
            reconciler
                .finalize_due_with_faults(now, FaultInjector::at(*point))
                .is_err(),
            "fault {point:?} did not interrupt the boundary"
        );

        RecoveryManager::new(root.clone()).recover_startup()?;
        let sqlite = SqliteStore::open(root.clone())?;
        let report =
            AggregationReconciler::new(root.clone(), sqlite.clone(), config).finalize_due(now)?;
        assert!(report.generated_revision_ids.len() <= 1);
        let snapshot = sqlite.snapshot_ids()?;
        assert_eq!(snapshot.current_chunks.len(), 1, "fault {point:?}");
        assert_eq!(snapshot.chunk_revision_ids.len(), 1, "fault {point:?}");
        let journal = CanonicalJournal::new(root.clone());
        assert_eq!(
            journal
                .scan_all(JournalFamily::Chunks, false)?
                .records
                .len(),
            1,
            "fault {point:?} duplicated canonical chunk bytes"
        );
        let watermark = StoreQueries::new(sqlite)
            .aggregation_watermark()?
            .ok_or("watermark missing")?;
        assert_eq!(watermark.1.as_str(), snapshot.current_chunks[0].1);
    }
    Ok(())
}

#[test]
fn calculation_before_append_is_recomputed_without_an_empty_revision() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    common::seed_events(&root, &projector, &events)?;
    let config = ChunkerConfig {
        aggregator_version: "calculation-boundary-1".to_owned(),
        max_cadence_seconds: 30,
    };
    let reconciler = AggregationReconciler::new(root.clone(), sqlite.clone(), config);
    let before_due: DateTime<Utc> = "2026-07-13T09:05:29Z".parse()?;
    assert!(
        reconciler
            .finalize_due(before_due)?
            .generated_revision_ids
            .is_empty()
    );
    assert!(sqlite.snapshot_ids()?.chunk_revision_ids.is_empty());
    let due: DateTime<Utc> = "2026-07-13T09:05:30Z".parse()?;
    assert_eq!(
        reconciler.finalize_due(due)?.generated_revision_ids.len(),
        1
    );
    assert_eq!(sqlite.snapshot_ids()?.chunk_revision_ids.len(), 1);
    let clean = reconciler.finalize_due(due)?;
    assert!(clean.generated_revision_ids.is_empty());
    assert_eq!(clean.already_current, 0);
    assert!(
        StoreQueries::new(sqlite.clone())
            .pending_aggregation_buckets()?
            .is_empty()
    );

    let upgraded = AggregationReconciler::new(
        root.clone(),
        sqlite.clone(),
        ChunkerConfig {
            aggregator_version: "calculation-boundary-2".to_owned(),
            max_cadence_seconds: 30,
        },
    );
    let upgrade_now: DateTime<Utc> = "2026-07-14T10:00:00Z".parse()?;
    assert_eq!(
        upgraded
            .finalize_due(upgrade_now)?
            .generated_revision_ids
            .len(),
        1
    );
    let snapshot = sqlite.snapshot_ids()?;
    assert_eq!(snapshot.current_chunks.len(), 1);
    assert_eq!(snapshot.chunk_revision_ids.len(), 2);
    let current = StoreQueries::new(sqlite.clone())
        .current_chunks_in_range(&chronicle_domain::UtcRange {
            start: "2026-07-13T09:00:00Z".parse()?,
            end: "2026-07-13T09:05:00Z".parse()?,
        })?
        .pop()
        .ok_or("upgraded current chunk missing")?;
    assert_eq!(current.aggregator_version, "calculation-boundary-2");
    assert!(!current.late_input);
    assert_eq!(current.generated_at, upgrade_now);
    let shards = std::fs::read_dir(root.path().join("aggregates/chunks"))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    assert!(shards.iter().any(|shard| shard.starts_with("2026-07-13")));
    assert!(shards.iter().any(|shard| shard.starts_with("2026-07-14")));
    assert_eq!(upgraded.finalize_due(upgrade_now)?.already_current, 0);
    Ok(())
}

#[test]
fn successive_build_transitions_use_the_latest_build_generation_instant()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    common::seed_events(&root, &projector, &events)?;
    let v1 = ChunkerConfig {
        aggregator_version: "successive-build-v1".to_owned(),
        max_cadence_seconds: 30,
    };
    assert_eq!(
        AggregationReconciler::new(root.clone(), sqlite.clone(), v1)
            .finalize_due("2026-07-13T09:05:30Z".parse()?)?
            .generated_revision_ids
            .len(),
        1
    );
    let generation = StoreGeneration::load(&root)?.generation;
    let v2_at: DateTime<Utc> = "2026-07-14T10:00:00Z".parse()?;
    sqlite.prepare_aggregation_build("successive-build-v2", generation, v2_at)?;
    sqlite.prepare_aggregation_build(
        "successive-build-v2",
        generation,
        "2026-07-14T11:00:00Z".parse()?,
    )?;
    let pending = StoreQueries::new(sqlite.clone()).pending_aggregation_buckets()?;
    assert_eq!(pending[0].generation_at, Some(v2_at));

    let v3_at: DateTime<Utc> = "2026-07-15T12:00:00Z".parse()?;
    sqlite.prepare_aggregation_build("successive-build-v3", generation, v3_at)?;
    let pending = StoreQueries::new(sqlite.clone()).pending_aggregation_buckets()?;
    assert_eq!(pending[0].generation_at, Some(v3_at));
    let v3 = AggregationReconciler::new(
        root.clone(),
        sqlite.clone(),
        ChunkerConfig {
            aggregator_version: "successive-build-v3".to_owned(),
            max_cadence_seconds: 30,
        },
    );
    assert_eq!(v3.finalize_due(v3_at)?.generated_revision_ids.len(), 1);
    let current = StoreQueries::new(sqlite)
        .current_chunks_in_range(&chronicle_domain::UtcRange {
            start: "2026-07-13T09:00:00Z".parse()?,
            end: "2026-07-13T09:05:00Z".parse()?,
        })?
        .pop()
        .ok_or("successive build chunk missing")?;
    assert_eq!(current.aggregator_version, "successive-build-v3");
    assert_eq!(current.generated_at, v3_at);
    assert!(!current.late_input);
    let shards = std::fs::read_dir(root.path().join("aggregates/chunks"))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    assert!(shards.iter().any(|shard| shard.starts_with("2026-07-15")));
    Ok(())
}

#[test]
fn observed_sixty_second_cadence_prevents_early_finalization() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let mut events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    for event in &mut events[5..] {
        let chronicle_domain::EventPayload::ObservationAttempt(attempt) = &mut event.payload else {
            return Err("fixture contains non-attempt".into());
        };
        attempt.cadence_seconds = 60;
    }
    common::seed_events(&root, &projector, &events)?;
    let reconciler = AggregationReconciler::new(
        root,
        sqlite.clone(),
        ChunkerConfig {
            aggregator_version: "observed-cadence-1".to_owned(),
            max_cadence_seconds: 30,
        },
    );

    let too_early: DateTime<Utc> = "2026-07-13T09:05:30Z".parse()?;
    assert!(
        reconciler
            .finalize_due(too_early)?
            .generated_revision_ids
            .is_empty()
    );
    assert!(sqlite.snapshot_ids()?.chunk_revision_ids.is_empty());

    let due: DateTime<Utc> = "2026-07-13T09:06:00Z".parse()?;
    assert_eq!(
        reconciler.finalize_due(due)?.generated_revision_ids.len(),
        1
    );
    assert_eq!(sqlite.snapshot_ids()?.chunk_revision_ids.len(), 1);
    Ok(())
}

#[test]
fn v1_migration_preserves_late_event_dirty_marker_and_supersedes_stale_chunk()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let mut events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let late = events.pop().ok_or("fixture missing late event")?;
    common::seed_events(&root, &projector, &events)?;
    let config = ChunkerConfig {
        aggregator_version: "v1-migration-late-1".to_owned(),
        max_cadence_seconds: 30,
    };
    let now: DateTime<Utc> = "2026-07-13T09:05:30Z".parse()?;
    assert_eq!(
        AggregationReconciler::new(root.clone(), sqlite.clone(), config.clone())
            .finalize_due(now)?
            .generated_revision_ids
            .len(),
        1
    );
    let stale_revision_id = sqlite.snapshot_ids()?.current_chunks[0].1.clone();

    let mut late_value = serde_json::to_value(late)?;
    late_value["recorded_at"] = serde_json::json!("2026-07-13T09:06:00Z");
    let late = chronicle_domain::EventEnvelope::parse(&serde_json::to_string(&late_value)?)?;
    let record = CanonicalJournal::new(root.clone()).append_event(&late, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;

    // Recreate the projection shape present immediately before the v2 migration.
    // The stale current chunk does not reference the subsequently projected event.
    sqlite.checkpoint()?;
    {
        let connection = sqlite.connection()?;
        let stale_ref_count: i64 = connection.query_row(
            "SELECT count(*) FROM chunk_evidence_refs
             WHERE revision_id=?1 AND event_id=?2",
            [&stale_revision_id, late.event_id.as_str()],
            |row| row.get(0),
        )?;
        assert_eq!(stale_ref_count, 0);
        connection.execute_batch(
            "DROP TABLE aggregation_build_state;
             DROP TABLE aggregation_bucket_events;
             DROP TABLE aggregation_pending_buckets;
             PRAGMA user_version = 1;
             UPDATE schema_versions
             SET version=1, build_id='v1-migration-fixture'
             WHERE component='store';",
        )?;
    }
    drop(projector);
    drop(sqlite);

    let migrated = SqliteStore::open(root.clone())?;
    assert_eq!(
        StoreQueries::new(migrated.clone())
            .pending_aggregation_buckets()?
            .len(),
        1,
        "migration must retain dirty buckets even when a current chunk exists"
    );
    let report = AggregationReconciler::new(root.clone(), migrated.clone(), config)
        .reconcile_startup("2026-07-13T09:06:30Z".parse()?)?;
    assert_eq!(report.generated_revision_ids.len(), 1);

    let snapshot = migrated.snapshot_ids()?;
    assert_eq!(snapshot.current_chunks.len(), 1);
    assert_eq!(snapshot.chunk_revision_ids.len(), 2);
    assert_ne!(snapshot.current_chunks[0].1, stale_revision_id);
    let current = StoreQueries::new(migrated)
        .current_chunks_in_range(&chronicle_domain::UtcRange {
            start: "2026-07-13T09:00:00Z".parse()?,
            end: "2026-07-13T09:05:00Z".parse()?,
        })?
        .pop()
        .ok_or("superseding migrated chunk missing")?;
    assert_eq!(
        current.prior_revision_id.as_ref().map(ToString::to_string),
        Some(stale_revision_id)
    );
    assert!(current.supporting_event_ids.contains(&late.event_id));
    assert!(current.late_input);
    Ok(())
}

#[test]
fn fresh_projection_rebuild_retains_late_event_and_supersedes_stale_chunk()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let mut events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let late = events.pop().ok_or("fixture missing late event")?;
    common::seed_events(&root, &projector, &events)?;
    let config = ChunkerConfig {
        aggregator_version: "rebuild-late-1".to_owned(),
        max_cadence_seconds: 30,
    };
    assert_eq!(
        AggregationReconciler::new(root.clone(), sqlite.clone(), config.clone())
            .finalize_due("2026-07-13T09:05:30Z".parse()?)?
            .generated_revision_ids
            .len(),
        1
    );
    let stale_revision_id = sqlite.snapshot_ids()?.current_chunks[0].1.clone();
    let mut late_value = serde_json::to_value(late)?;
    late_value["recorded_at"] = serde_json::json!("2026-07-13T09:06:00Z");
    let late = chronicle_domain::EventEnvelope::parse(&serde_json::to_string(&late_value)?)?;
    let record = CanonicalJournal::new(root.clone()).append_event(&late, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;
    sqlite.checkpoint()?;
    drop(projector);
    drop(sqlite);

    let (_recovery, rebuilt_snapshot) = RecoveryManager::new(root.clone()).rebuild_index()?;
    assert_eq!(rebuilt_snapshot.chunk_revision_ids.len(), 1);
    let rebuilt = SqliteStore::open(root.clone())?;
    assert_eq!(
        StoreQueries::new(rebuilt.clone())
            .pending_aggregation_buckets()?
            .len(),
        1,
        "stale canonical chunk must not clear the rebuilt late-event marker"
    );
    assert_eq!(
        AggregationReconciler::new(root, rebuilt.clone(), config)
            .reconcile_recovered_startup("2026-07-13T09:06:30Z".parse()?)?
            .generated_revision_ids
            .len(),
        1
    );
    let snapshot = rebuilt.snapshot_ids()?;
    assert_eq!(snapshot.chunk_revision_ids.len(), 2);
    assert_ne!(snapshot.current_chunks[0].1, stale_revision_id);
    let current = StoreQueries::new(rebuilt)
        .current_chunks_in_range(&chronicle_domain::UtcRange {
            start: "2026-07-13T09:00:00Z".parse()?,
            end: "2026-07-13T09:05:00Z".parse()?,
        })?
        .pop()
        .ok_or("rebuilt superseding chunk missing")?;
    assert!(current.supporting_event_ids.contains(&late.event_id));
    assert!(current.late_input);
    Ok(())
}
