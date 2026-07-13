use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::{
    ChunkId, ChunkRevisionId, ContractError, EventId, UntrustedEvidenceMarker, parse_versioned,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSeconds {
    pub captured: u32,
    pub protected: u32,
    pub paused: u32,
    pub unavailable: u32,
    pub error: u32,
    pub gap: u32,
}

impl EvidenceSeconds {
    pub const fn total(&self) -> u32 {
        self.captured + self.protected + self.paused + self.unavailable + self.error + self.gap
    }

    pub const fn for_gap_kind(&self, kind: ChunkGapKind) -> u32 {
        match kind {
            ChunkGapKind::Protected => self.protected,
            ChunkGapKind::Paused => self.paused,
            ChunkGapKind::Unavailable => self.unavailable,
            ChunkGapKind::Error => self.error,
            ChunkGapKind::MissingObservation => self.gap,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceSeconds {
    pub active: u32,
    pub idle: u32,
    pub unknown: u32,
}

impl PresenceSeconds {
    pub const fn total(&self) -> u32 {
        self.active + self.idle + self.unknown
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DimensionKind {
    Application,
    Window,
    AuthorizedDomain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationEstimate {
    pub dimension: DimensionKind,
    pub key: String,
    pub label: String,
    pub estimated_seconds: u32,
    pub supporting_event_ids: Vec<EventId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub at: DateTime<Utc>,
    pub from_key: Option<String>,
    pub to_key: String,
    pub supporting_event_id: EventId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrExtract {
    pub text: String,
    pub source_event_id: EventId,
    pub untrusted_evidence: UntrustedEvidenceMarker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkGapKind {
    Protected,
    Paused,
    Unavailable,
    Error,
    MissingObservation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkGap {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub kind: ChunkGapKind,
    pub supporting_event_ids: Vec<EventId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRevision {
    pub schema_version: String,
    pub chunk_id: ChunkId,
    pub revision_id: ChunkRevisionId,
    pub prior_revision_id: Option<ChunkRevisionId>,
    pub supersedes_revision_id: Option<ChunkRevisionId>,
    pub window: ChunkWindow,
    pub generated_at: DateTime<Utc>,
    pub display_timezone: String,
    pub aggregator_version: String,
    pub input_digest: String,
    pub store_generation: u64,
    pub finalization_cadence_seconds: u32,
    pub evidence_seconds: EvidenceSeconds,
    pub presence_seconds: PresenceSeconds,
    pub duration_estimates: Vec<DurationEstimate>,
    pub transitions: Vec<Transition>,
    pub ocr_extracts: Vec<OcrExtract>,
    pub gaps: Vec<ChunkGap>,
    pub supporting_event_ids: Vec<EventId>,
    pub late_input: bool,
}

impl ChunkRevision {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        let chunk: Self = parse_versioned(json)?;
        chunk.validate().map_err(ContractError::Validation)?;
        Ok(chunk)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.window.start >= self.window.end {
            return Err("chunk window start must precede end".to_owned());
        }
        let duration = self.window.end - self.window.start;
        if duration.num_seconds() != 300 || self.window.start.timestamp().rem_euclid(300) != 0 {
            return Err("chunk must use a UTC-aligned 300-second window".to_owned());
        }
        if self.evidence_seconds.total() != 300 {
            return Err("evidence-state seconds must sum to 300".to_owned());
        }
        if self.presence_seconds.total() != self.evidence_seconds.captured {
            return Err("presence-state seconds must partition captured coverage".to_owned());
        }
        if !matches!(self.finalization_cadence_seconds, 30 | 60) {
            return Err("finalization cadence must be 30 or 60 seconds".to_owned());
        }
        if self.generated_at < self.window.end {
            return Err("chunk cannot be generated before its window ends".to_owned());
        }
        if self.display_timezone.is_empty()
            || self.aggregator_version.is_empty()
            || self.input_digest.is_empty()
            || self.store_generation == 0
        {
            return Err("chunk provenance fields must be non-empty and current".to_owned());
        }
        if self.prior_revision_id != self.supersedes_revision_id {
            return Err("prior and superseded revision links must agree".to_owned());
        }
        if self
            .prior_revision_id
            .as_ref()
            .is_some_and(|prior| prior == &self.revision_id)
        {
            return Err("a chunk revision cannot supersede itself".to_owned());
        }
        let supporting: HashSet<_> = self.supporting_event_ids.iter().collect();
        if supporting.len() != self.supporting_event_ids.len() {
            return Err("supporting event IDs must be unique".to_owned());
        }
        let references_supported = |event_id: &EventId| supporting.contains(event_id);
        let mut totals_by_dimension = HashMap::<DimensionKind, u32>::new();
        let mut dimension_keys = HashSet::new();
        for estimate in &self.duration_estimates {
            if estimate.key.is_empty()
                || estimate.label.is_empty()
                || estimate.supporting_event_ids.is_empty()
                || estimate
                    .supporting_event_ids
                    .iter()
                    .any(|event_id| !references_supported(event_id))
            {
                return Err("duration estimates require supported factual identity".to_owned());
            }
            if !dimension_keys.insert((estimate.dimension, estimate.key.as_str())) {
                return Err("dimension estimates must be unique by dimension and key".to_owned());
            }
            let total = totals_by_dimension.entry(estimate.dimension).or_default();
            *total = total
                .checked_add(estimate.estimated_seconds)
                .ok_or_else(|| "dimension duration overflow".to_owned())?;
        }
        if totals_by_dimension
            .values()
            .any(|total| *total > self.evidence_seconds.captured)
        {
            return Err("dimension totals cannot exceed captured coverage".to_owned());
        }
        for transition in &self.transitions {
            if transition.at < self.window.start
                || transition.at >= self.window.end
                || transition.to_key.is_empty()
                || transition
                    .from_key
                    .as_ref()
                    .is_some_and(|key| key.is_empty())
                || !references_supported(&transition.supporting_event_id)
            {
                return Err("transition must be inside the chunk and reference evidence".to_owned());
            }
        }
        for extract in &self.ocr_extracts {
            if extract.text.is_empty() || !references_supported(&extract.source_event_id) {
                return Err("OCR extracts must contain text and reference evidence".to_owned());
            }
        }
        let mut prior_gap_end = None;
        let mut gap_seconds = HashMap::<ChunkGapKind, u32>::new();
        for gap in &self.gaps {
            if gap.start < self.window.start || gap.end > self.window.end || gap.start >= gap.end {
                return Err("chunk gaps must be positive intervals inside the chunk".to_owned());
            }
            if prior_gap_end.is_some_and(|end| gap.start < end) {
                return Err("chunk gaps must be ordered and non-overlapping".to_owned());
            }
            if gap.kind != ChunkGapKind::MissingObservation && gap.supporting_event_ids.is_empty() {
                return Err("observed gaps require supporting event IDs".to_owned());
            }
            if gap
                .supporting_event_ids
                .iter()
                .any(|event_id| !references_supported(event_id))
            {
                return Err("chunk gap references evidence outside the chunk".to_owned());
            }
            let duration = gap.end - gap.start;
            let seconds = duration.num_seconds();
            if seconds <= 0 || chrono::Duration::seconds(seconds) != duration {
                return Err("chunk gaps must use whole-second durations".to_owned());
            }
            let seconds = u32::try_from(seconds)
                .map_err(|_| "chunk gap duration exceeds v1 bounds".to_owned())?;
            let total = gap_seconds.entry(gap.kind).or_default();
            *total = total
                .checked_add(seconds)
                .ok_or_else(|| "chunk gap duration overflow".to_owned())?;
            prior_gap_end = Some(gap.end);
        }
        for kind in [
            ChunkGapKind::Protected,
            ChunkGapKind::Paused,
            ChunkGapKind::Unavailable,
            ChunkGapKind::Error,
            ChunkGapKind::MissingObservation,
        ] {
            if gap_seconds.get(&kind).copied().unwrap_or_default()
                != self.evidence_seconds.for_gap_kind(kind)
            {
                return Err(
                    "chunk gap intervals must exactly reconcile non-captured evidence seconds"
                        .to_owned(),
                );
            }
        }
        Ok(())
    }
}
