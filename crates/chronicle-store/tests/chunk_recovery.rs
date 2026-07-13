mod common;

use chronicle_domain::{ChunkId, ChunkRevisionId};
use chronicle_store::{CanonicalJournal, FaultInjector, FaultPoint, RecoveryManager, StoreError};

#[test]
fn current_chunk_and_watermark_reconcile_after_crash() -> chronicle_store::Result<()> {
    for point in [
        FaultPoint::AfterRowInsert,
        FaultPoint::AfterCurrentPointerUpdate,
        FaultPoint::AfterWatermarkUpdate,
        FaultPoint::AfterCursorUpdate,
        FaultPoint::BeforeTransactionCommit,
        FaultPoint::AfterTransactionCommit,
    ] {
        let (_temporary, root, sqlite, projector) = common::store()?;
        let journal = CanonicalJournal::new(root.clone());
        let chunks = common::chunks()?;
        let first = journal.append_chunk(&chunks[0], FaultInjector::none())?;
        projector.project_record(&first, FaultInjector::none())?;
        let second = journal.append_chunk(&chunks[1], FaultInjector::none())?;
        assert!(matches!(
            projector.project_record(&second, FaultInjector::at(point)),
            Err(StoreError::InjectedFault(actual)) if actual == point
        ));
        RecoveryManager::new(root).recover_startup()?;
        let snapshot = sqlite.snapshot_ids()?;
        assert_eq!(
            snapshot.current_chunks,
            vec![(
                "chunk-20260713T0900Z".to_owned(),
                "chunk-rev-002".to_owned()
            )],
            "fault point {point:?}"
        );
        let watermark: (String, String) = sqlite.connection()?.query_row(
            "SELECT through_utc, revision_id FROM aggregation_watermark WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(
            watermark,
            (
                chunks[1].window.end.to_rfc3339(),
                chunks[1].revision_id.to_string()
            ),
            "fault point {point:?}"
        );
    }
    Ok(())
}

#[test]
fn late_earlier_bucket_does_not_regress_global_watermark() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let journal = CanonicalJournal::new(root.clone());
    let base = common::chunks()?.remove(0);
    let mut later = base.clone();
    later.chunk_id = ChunkId::new("chunk-20260713T0905Z")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    later.revision_id = ChunkRevisionId::new("chunk-later-rev-001")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    later.input_digest = "sha256-later-window".to_owned();
    let shift = chrono::Duration::minutes(5);
    later.window.start += shift;
    later.window.end += shift;
    later.generated_at += shift;
    for transition in &mut later.transitions {
        transition.at += shift;
    }
    for gap in &mut later.gaps {
        gap.start += shift;
        gap.end += shift;
    }
    let mut early_late = base;
    early_late.revision_id = ChunkRevisionId::new("chunk-early-late-rev-001")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    early_late.input_digest = "sha256-early-late-window".to_owned();
    early_late.late_input = true;

    let later_record = journal.append_chunk(&later, FaultInjector::none())?;
    projector.project_record(&later_record, FaultInjector::none())?;
    let early_record = journal.append_chunk(&early_late, FaultInjector::none())?;
    projector.project_record(&early_record, FaultInjector::none())?;
    let watermark: (String, String) = sqlite.connection()?.query_row(
        "SELECT through_utc, revision_id FROM aggregation_watermark WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        watermark,
        (later.window.end.to_rfc3339(), later.revision_id.to_string())
    );
    assert_eq!(sqlite.snapshot_ids()?.current_chunks.len(), 2);
    let before = sqlite.snapshot_ids()?;
    let (_report, rebuilt) = RecoveryManager::new(root).rebuild_index()?;
    assert_eq!(before, rebuilt);
    Ok(())
}
