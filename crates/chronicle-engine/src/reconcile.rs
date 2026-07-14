use chronicle_domain::ChunkRevisionId;
use chronicle_store::{
    CanonicalJournal, FaultInjector, ManagedRoot, Projector, RecoveryManager, SqliteStore,
    StoreGeneration, StoreQueries,
};
use chrono::{DateTime, Utc};

use crate::{ChunkBuild, ChunkerConfig, Result, build_chunk, chunk_id};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub generated_revision_ids: Vec<ChunkRevisionId>,
    pub already_current: usize,
    pub remaining_due: bool,
}

#[derive(Clone, Debug)]
pub struct AggregationReconciler {
    root: ManagedRoot,
    sqlite: SqliteStore,
    config: ChunkerConfig,
}

impl AggregationReconciler {
    pub const fn new(root: ManagedRoot, sqlite: SqliteStore, config: ChunkerConfig) -> Self {
        Self {
            root,
            sqlite,
            config,
        }
    }

    pub fn reconcile_startup(&self, now: DateTime<Utc>) -> Result<ReconcileReport> {
        RecoveryManager::new(self.root.clone()).recover_startup()?;
        self.reconcile_recovered_startup(now)
    }

    pub fn reconcile_recovered_startup(&self, now: DateTime<Utc>) -> Result<ReconcileReport> {
        let mut combined = ReconcileReport::default();
        loop {
            let report = self.finalize_due(now)?;
            combined
                .generated_revision_ids
                .extend(report.generated_revision_ids);
            combined.already_current += report.already_current;
            if !report.remaining_due {
                return Ok(combined);
            }
        }
    }

    pub fn finalize_due(&self, now: DateTime<Utc>) -> Result<ReconcileReport> {
        self.finalize_due_with_faults(now, FaultInjector::none())
    }

    pub fn finalize_due_with_faults(
        &self,
        now: DateTime<Utc>,
        faults: FaultInjector,
    ) -> Result<ReconcileReport> {
        self.config.validate()?;
        let queries = StoreQueries::new(self.sqlite.clone());
        let generation = StoreGeneration::load(&self.root)?;
        self.sqlite.prepare_aggregation_build(
            &self.config.aggregator_version,
            generation.generation,
            now,
        )?;
        let _watermark = queries.aggregation_watermark()?;
        let (buckets, remaining_due) =
            queries.due_aggregation_bucket_batch(now, self.config.max_cadence_seconds)?;
        let journal = CanonicalJournal::new(self.root.clone());
        let projector = Projector::new(self.sqlite.clone());
        let mut report = ReconcileReport {
            remaining_due,
            ..ReconcileReport::default()
        };
        for bucket in buckets {
            let start = bucket.bucket_start;
            let device = bucket.device_id;
            let events = queries.aggregation_events_for_bucket(&device, start)?;
            if events.is_empty() {
                return Err(crate::EngineError::Aggregation(
                    "pending aggregation bucket has no indexed evidence".to_owned(),
                ));
            }
            let id = chunk_id(&device, start)?;
            let prior = queries.current_chunk(&id)?;
            let Some(chunk) = build_chunk(ChunkBuild {
                events: &events,
                bucket_start: start,
                prior: prior.as_ref(),
                store_generation: generation.generation,
                revision_generated_at: bucket.generation_at,
                config: &self.config,
            })?
            else {
                continue;
            };
            if now < chunk.generated_at {
                continue;
            }
            if prior.as_ref().is_some_and(|prior| {
                prior.input_digest == chunk.input_digest
                    && prior.aggregator_version == chunk.aggregator_version
                    && prior.store_generation == chunk.store_generation
            }) {
                report.already_current += 1;
                self.sqlite
                    .clear_pending_aggregation_bucket(&device, start)?;
                continue;
            }
            let record = journal.append_chunk(&chunk, faults)?;
            projector.project_record(&record, faults)?;
            report.generated_revision_ids.push(chunk.revision_id);
        }
        Ok(report)
    }
}
