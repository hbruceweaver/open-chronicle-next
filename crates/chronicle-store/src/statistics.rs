use std::collections::{BTreeMap, BTreeSet};

use chronicle_domain::{
    ChunkGap, ChunkGapKind, ChunkId, ChunkRevision, ChunkRevisionId, DimensionKind,
    EvidenceSeconds, FactualTotal, PresenceSeconds, QueryCoverage, Transition, UtcRange,
};
use chrono::Duration;

use crate::{Result, StoreError, StoreQueries};

const MAX_STATISTICS_BUCKETS: i64 = 366 * 24 * 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatisticsReport {
    pub coverage: QueryCoverage,
    pub factual_totals: Vec<FactualTotal>,
    pub transitions: Vec<Transition>,
    pub source_chunk_revision_ids: Vec<ChunkRevisionId>,
    /// Current chunk revisions read from the same pinned query source as the
    /// aggregate fields. App-private report projections may derive slim
    /// activity buckets from these without issuing a second, racy query.
    pub activity_chunks: Vec<ChunkRevision>,
}

#[derive(Clone, Debug)]
pub struct FactualStatistics {
    queries: StoreQueries,
}

impl FactualStatistics {
    pub const fn new(queries: StoreQueries) -> Self {
        Self { queries }
    }

    pub fn range(&self, range: &UtcRange) -> Result<StatisticsReport> {
        range.validate().map_err(StoreError::InvalidPath)?;
        if range.start.timestamp().rem_euclid(300) != 0
            || range.end.timestamp().rem_euclid(300) != 0
        {
            return Err(StoreError::InvalidPath(
                "statistics range must align to UTC five-minute boundaries".to_owned(),
            ));
        }
        let duration = (range.end - range.start).num_seconds();
        let bucket_count = duration / 300;
        if bucket_count > MAX_STATISTICS_BUCKETS {
            return Err(StoreError::InvalidPath(
                "statistics range exceeds the 105408-bucket factual query budget".to_owned(),
            ));
        }
        let chunks = self.queries.current_chunks_in_range(range)?;
        let mut chunks_by_start = BTreeMap::new();
        for chunk in &chunks {
            if chunks_by_start.insert(chunk.window.start, chunk).is_some() {
                return Err(StoreError::SqliteIdentity(
                    "multiple current chunks occupy one local UTC bucket".to_owned(),
                ));
            }
        }
        let mut evidence = EvidenceSeconds {
            captured: 0,
            protected: 0,
            paused: 0,
            unavailable: 0,
            error: 0,
            gap: 0,
        };
        let mut presence = PresenceSeconds {
            active: 0,
            idle: 0,
            unknown: 0,
        };
        let mut gaps = Vec::new();
        let mut totals = BTreeMap::<(DimensionKind, String), (u32, BTreeSet<ChunkId>)>::new();
        let mut transitions = Vec::new();
        let mut revision_ids = Vec::new();
        let mut cursor = range.start;
        while cursor < range.end {
            let end = cursor + Duration::seconds(300);
            if let Some(chunk) = chunks_by_start.get(&cursor) {
                add_evidence(&mut evidence, &chunk.evidence_seconds)?;
                add_presence(&mut presence, &chunk.presence_seconds)?;
                for gap in &chunk.gaps {
                    push_coalesced_gap(&mut gaps, gap.clone());
                }
                for estimate in &chunk.duration_estimates {
                    let entry = totals
                        .entry((estimate.dimension, estimate.key.clone()))
                        .or_insert_with(|| (0, BTreeSet::new()));
                    entry.0 = entry
                        .0
                        .checked_add(estimate.estimated_seconds)
                        .ok_or_else(|| {
                            StoreError::InvalidPath("statistics total overflow".to_owned())
                        })?;
                    entry.1.insert(chunk.chunk_id.clone());
                }
                transitions.extend(chunk.transitions.clone());
                revision_ids.push(chunk.revision_id.clone());
            } else {
                evidence.gap = evidence
                    .gap
                    .checked_add(300)
                    .ok_or_else(|| StoreError::InvalidPath("statistics gap overflow".to_owned()))?;
                push_coalesced_gap(
                    &mut gaps,
                    ChunkGap {
                        start: cursor,
                        end,
                        kind: ChunkGapKind::MissingObservation,
                        supporting_event_ids: Vec::new(),
                    },
                );
            }
            cursor = end;
        }
        transitions.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.supporting_event_id.cmp(&right.supporting_event_id))
        });
        let factual_totals = totals
            .into_iter()
            .map(
                |((dimension, key), (estimated_seconds, supporting_chunk_ids))| FactualTotal {
                    dimension,
                    key,
                    estimated_seconds,
                    supporting_chunk_ids: supporting_chunk_ids.into_iter().collect(),
                },
            )
            .collect();
        let coverage = QueryCoverage {
            range: range.clone(),
            evidence_seconds: evidence,
            presence_seconds: presence,
            gaps,
        };
        coverage.validate().map_err(StoreError::SqliteIdentity)?;
        Ok(StatisticsReport {
            coverage,
            factual_totals,
            transitions,
            source_chunk_revision_ids: revision_ids,
            activity_chunks: chunks,
        })
    }
}

fn push_coalesced_gap(gaps: &mut Vec<ChunkGap>, mut next: ChunkGap) {
    if let Some(last) = gaps.last_mut()
        && last.end == next.start
        && last.kind == next.kind
    {
        last.end = next.end;
        last.supporting_event_ids
            .append(&mut next.supporting_event_ids);
        last.supporting_event_ids.sort();
        last.supporting_event_ids.dedup();
        return;
    }
    gaps.push(next);
}

fn add_evidence(total: &mut EvidenceSeconds, add: &EvidenceSeconds) -> Result<()> {
    total.captured = checked(total.captured, add.captured)?;
    total.protected = checked(total.protected, add.protected)?;
    total.paused = checked(total.paused, add.paused)?;
    total.unavailable = checked(total.unavailable, add.unavailable)?;
    total.error = checked(total.error, add.error)?;
    total.gap = checked(total.gap, add.gap)?;
    Ok(())
}

fn add_presence(total: &mut PresenceSeconds, add: &PresenceSeconds) -> Result<()> {
    total.active = checked(total.active, add.active)?;
    total.idle = checked(total.idle, add.idle)?;
    total.unknown = checked(total.unknown, add.unknown)?;
    Ok(())
}

fn checked(left: u32, right: u32) -> Result<u32> {
    left.checked_add(right)
        .ok_or_else(|| StoreError::InvalidPath("statistics coverage overflow".to_owned()))
}
