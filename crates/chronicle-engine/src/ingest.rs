use std::collections::BTreeSet;

use chronicle_domain::{
    DurableAcknowledgement, EventEnvelope, EventId, EventPayload, HealthSnapshot,
    ObservationContent, ProjectionHealth,
};
use chronicle_store::{
    CanonicalJournal, FaultInjector, FaultPoint, ManagedRoot, Projector, SqliteStore, StoreError,
};
use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::ChunkerConfig;
use crate::health::{aggregation_lag, healthy, projection_lag};
use crate::reconcile::{AggregationReconciler, ReconcileReport};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("storage failure: {0}")]
    Store(#[from] StoreError),
    #[error("contract identifier failure: {0}")]
    Identifier(#[from] chronicle_domain::IdError),
    #[error("aggregation failure: {0}")]
    Aggregation(String),
    #[error("configuration failure: {0}")]
    Configuration(String),
    #[error("cadence failure: {0}")]
    Cadence(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CadenceStamp {
    pub boot_sequence: String,
    pub monotonic_tick: u64,
}

#[derive(Clone, Debug, Default)]
pub struct CadenceGuard {
    last: Option<CadenceStamp>,
    last_event_id: Option<EventId>,
    retired_boot_sequences: BTreeSet<String>,
}

impl CadenceGuard {
    pub fn observe(&mut self, stamp: &CadenceStamp) -> Result<()> {
        if stamp.boot_sequence.is_empty() {
            return Err(EngineError::Cadence(
                "boot sequence must be non-empty".to_owned(),
            ));
        }
        if let Some(last) = &self.last {
            if last.boot_sequence == stamp.boot_sequence {
                if stamp.monotonic_tick <= last.monotonic_tick {
                    return Err(EngineError::Cadence(
                        "monotonic tick did not advance within the boot sequence".to_owned(),
                    ));
                }
            } else {
                self.retired_boot_sequences
                    .insert(last.boot_sequence.clone());
            }
        }
        if self.retired_boot_sequences.contains(&stamp.boot_sequence) {
            return Err(EngineError::Cadence(
                "retired boot sequence cannot become active again".to_owned(),
            ));
        }
        self.last = Some(stamp.clone());
        self.last_event_id = None;
        Ok(())
    }

    fn observe_event(&mut self, stamp: &CadenceStamp, event_id: &EventId) -> Result<()> {
        if self.last.as_ref() == Some(stamp) && self.last_event_id.as_ref() == Some(event_id) {
            return Ok(());
        }
        self.observe(stamp)?;
        self.last_event_id = Some(event_id.clone());
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct IngestRequest {
    pub event: EventEnvelope,
    pub cadence: Option<CadenceStamp>,
}

#[derive(Clone, Debug)]
pub struct IngestOutcome {
    pub acknowledgement: DurableAcknowledgement,
    pub projection: ProjectionHealth,
    pub health: HealthSnapshot,
    pub aggregation: Option<ReconcileReport>,
}

#[derive(Clone, Debug)]
pub struct IngestEngine {
    root: ManagedRoot,
    journal: CanonicalJournal,
    projector: Projector,
    sqlite: SqliteStore,
    chunker: ChunkerConfig,
    cadence: CadenceGuard,
    projection_lagging: bool,
}

impl IngestEngine {
    pub fn open(root: ManagedRoot, chunker: ChunkerConfig) -> Result<Self> {
        Self::open_at(root, chunker, Utc::now())
    }

    pub fn open_at(root: ManagedRoot, chunker: ChunkerConfig, now: DateTime<Utc>) -> Result<Self> {
        chunker.validate()?;
        let journal = CanonicalJournal::new(root.clone());
        chronicle_store::RecoveryManager::new(root.clone()).recover_startup()?;
        let sqlite = SqliteStore::open(root.clone())?;
        AggregationReconciler::new(root.clone(), sqlite.clone(), chunker.clone())
            .reconcile_recovered_startup(now)?;
        Ok(Self {
            journal,
            projector: Projector::new(sqlite.clone()),
            root,
            sqlite,
            chunker,
            cadence: CadenceGuard::default(),
            projection_lagging: false,
        })
    }

    pub fn ingest(&mut self, request: IngestRequest, now: DateTime<Utc>) -> Result<IngestOutcome> {
        self.ingest_with_faults(request, now, FaultInjector::none(), FaultInjector::none())
    }

    pub fn ingest_with_faults(
        &mut self,
        request: IngestRequest,
        now: DateTime<Utc>,
        event_faults: FaultInjector,
        chunk_faults: FaultInjector,
    ) -> Result<IngestOutcome> {
        if self.projection_lagging {
            self.recover_projection()?;
        }
        request.event.validate().map_err(EngineError::Aggregation)?;
        reject_transactional_event(&request.event)?;
        let event_value = serde_json::to_value(&request.event).map_err(StoreError::from)?;
        let event_checksum = chronicle_store::checksum::checksum_bytes(
            &chronicle_store::checksum::canonical_json(&event_value)?,
        );
        let existing = match self.sqlite.event_checksum(&request.event.event_id)? {
            Some(checksum) if checksum == event_checksum => true,
            Some(_) => {
                return Err(StoreError::StableIdConflict {
                    id: request.event.event_id.to_string(),
                }
                .into());
            }
            None => false,
        };
        if !existing {
            match (&request.event.payload, &request.cadence) {
                (EventPayload::ObservationAttempt(_), Some(stamp)) => {
                    self.cadence.observe_event(stamp, &request.event.event_id)?;
                }
                (EventPayload::ObservationAttempt(_), None) => {
                    return Err(EngineError::Cadence(
                        "novel observation attempts require a cadence stamp".to_owned(),
                    ));
                }
                (EventPayload::RecordingGap(_), Some(stamp)) => {
                    self.cadence.observe_event(stamp, &request.event.event_id)?;
                }
                (EventPayload::RecordingGap(_), None) => {}
                (EventPayload::ScreenshotLifecycle(_), _) => unreachable!(
                    "transactional lifecycle events are rejected before cadence validation"
                ),
            }
        }
        let record = match self.journal.append_event(&request.event, event_faults) {
            Ok(record) => record,
            Err(StoreError::InjectedFault(FaultPoint::AfterJournalSync)) => {
                self.projection_lagging = true;
                return Ok(IngestOutcome {
                    acknowledgement: DurableAcknowledgement::JournalDurableProjectionPending,
                    projection: ProjectionHealth::Lagging,
                    health: projection_lag(now),
                    aggregation: None,
                });
            }
            Err(error) => return Err(error.into()),
        };
        if self
            .projector
            .project_record(&record, event_faults)
            .is_err()
        {
            self.projection_lagging = true;
            return Ok(IngestOutcome {
                acknowledgement: DurableAcknowledgement::JournalDurableProjectionPending,
                projection: ProjectionHealth::Lagging,
                health: projection_lag(now),
                aggregation: None,
            });
        }
        let aggregation = match AggregationReconciler::new(
            self.root.clone(),
            self.sqlite.clone(),
            self.chunker.clone(),
        )
        .finalize_due_with_faults(now, chunk_faults)
        {
            Ok(report) => report,
            Err(_error) => {
                self.projection_lagging = true;
                return Ok(IngestOutcome {
                    acknowledgement: DurableAcknowledgement::Durable,
                    projection: ProjectionHealth::Lagging,
                    health: aggregation_lag(now),
                    aggregation: None,
                });
            }
        };
        if aggregation.remaining_due {
            Ok(IngestOutcome {
                acknowledgement: DurableAcknowledgement::Durable,
                projection: ProjectionHealth::Lagging,
                health: aggregation_lag(now),
                aggregation: Some(aggregation),
            })
        } else {
            Ok(IngestOutcome {
                acknowledgement: DurableAcknowledgement::Durable,
                projection: ProjectionHealth::Current,
                health: healthy(now),
                aggregation: Some(aggregation),
            })
        }
    }

    fn recover_projection(&mut self) -> Result<()> {
        chronicle_store::RecoveryManager::new(self.root.clone()).recover_startup()?;
        self.sqlite = SqliteStore::open(self.root.clone())?;
        self.projector = Projector::new(self.sqlite.clone());
        self.projection_lagging = false;
        Ok(())
    }
}

fn reject_transactional_event(event: &EventEnvelope) -> Result<()> {
    match &event.payload {
        EventPayload::ObservationAttempt(attempt) => {
            if matches!(
                &attempt.content,
                ObservationContent::Captured(content) if content.image.is_some()
            ) {
                return Err(EngineError::Aggregation(
                    "image-bearing observations require ManagedImageStore ingestion".to_owned(),
                ));
            }
        }
        EventPayload::ScreenshotLifecycle(_) => {
            return Err(EngineError::Aggregation(
                "screenshot lifecycle events require ManagedImageStore ingestion".to_owned(),
            ));
        }
        EventPayload::RecordingGap(_) => {}
    }
    Ok(())
}
