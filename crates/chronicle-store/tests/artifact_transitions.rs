mod common;

use std::sync::{Arc, Barrier};

use chronicle_domain::{
    ArtifactId, ArtifactRevisionId, ArtifactStatus, ArtifactType, DerivedArtifactRevision,
};
use chronicle_store::{ArtifactStore, FaultInjector, RecoveryManager, StoreError};

fn revision(
    artifact_id: &str,
    revision_id: &str,
    prior: Option<&str>,
    status: ArtifactStatus,
    artifact_type: ArtifactType,
    second: u32,
) -> chronicle_store::Result<DerivedArtifactRevision> {
    let mut revision = common::artifact()?;
    revision.artifact_id =
        ArtifactId::new(artifact_id).map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    revision.revision_id = ArtifactRevisionId::new(revision_id)
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    let prior = prior
        .map(ArtifactRevisionId::new)
        .transpose()
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    revision.prior_revision_id = prior.clone();
    revision.expected_prior_revision_id = prior;
    revision.status = status;
    revision.artifact_type = artifact_type;
    revision.created_at = format!("2026-07-13T09:08:{second:02}Z")
        .parse()
        .map_err(|error: chrono::ParseError| StoreError::InvalidPath(error.to_string()))?;
    Ok(revision)
}

#[test]
fn artifact_type_and_status_transition_matrix_fail_closed() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let store = ArtifactStore::new(root, projector);

    let type_base = revision(
        "type-change",
        "type-change-1",
        None,
        ArtifactStatus::Draft,
        ArtifactType::Hypothesis,
        0,
    )?;
    store.write_revision(&type_base, FaultInjector::none())?;
    let type_change = revision(
        "type-change",
        "type-change-2",
        Some("type-change-1"),
        ArtifactStatus::Draft,
        ArtifactType::Report,
        1,
    )?;
    assert!(matches!(
        store.write_revision(&type_change, FaultInjector::none()),
        Err(StoreError::InvalidArtifactTransition)
    ));

    for (artifact_id, settled, forbidden) in [
        (
            "accepted-terminal",
            ArtifactStatus::Accepted,
            ArtifactStatus::Rejected,
        ),
        (
            "rejected-terminal",
            ArtifactStatus::Rejected,
            ArtifactStatus::Accepted,
        ),
        (
            "superseded-terminal",
            ArtifactStatus::Superseded,
            ArtifactStatus::Draft,
        ),
    ] {
        let first_id = format!("{artifact_id}-1");
        let second_id = format!("{artifact_id}-2");
        let third_id = format!("{artifact_id}-3");
        store.write_revision(
            &revision(
                artifact_id,
                &first_id,
                None,
                ArtifactStatus::Draft,
                ArtifactType::Hypothesis,
                2,
            )?,
            FaultInjector::none(),
        )?;
        store.write_revision(
            &revision(
                artifact_id,
                &second_id,
                Some(&first_id),
                settled,
                ArtifactType::Hypothesis,
                3,
            )?,
            FaultInjector::none(),
        )?;
        assert!(matches!(
            store.write_revision(
                &revision(
                    artifact_id,
                    &third_id,
                    Some(&second_id),
                    forbidden,
                    ArtifactType::Hypothesis,
                    4,
                )?,
                FaultInjector::none(),
            ),
            Err(StoreError::InvalidArtifactTransition)
        ));
    }
    assert_eq!(store.scan_all()?.len(), 7);
    Ok(())
}

#[test]
fn global_revision_identity_collision_has_one_canonical_winner_and_rebuilds()
-> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let store = Arc::new(ArtifactStore::new(root.clone(), projector));
    let barrier = Arc::new(Barrier::new(3));
    let handles = ["collision-a", "collision-b"].map(|artifact_id| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let candidate = revision(
                artifact_id,
                "globally-shared-revision",
                None,
                ArtifactStatus::Draft,
                ArtifactType::Hypothesis,
                5,
            )
            .expect("candidate revision");
            barrier.wait();
            store.write_revision(&candidate, FaultInjector::none())
        })
    });
    barrier.wait();
    let results = handles.map(|handle| handle.join().expect("artifact writer"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::StableIdConflict { .. })))
            .count(),
        1,
        "unexpected collision results: {results:?}"
    );
    assert_eq!(store.scan_all()?.len(), 1);
    let (_report, rebuilt) = RecoveryManager::new(root.clone()).rebuild_index()?;
    assert_eq!(rebuilt.artifact_revision_ids.len(), 1);
    let losers = [
        "derived/collision-a/globally-shared-revision.json",
        "derived/collision-b/globally-shared-revision.json",
    ]
    .into_iter()
    .filter(|path| root.exists(path).unwrap_or(false))
    .count();
    assert_eq!(losers, 1);
    Ok(())
}
