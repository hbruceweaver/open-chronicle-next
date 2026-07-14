use std::collections::BTreeSet;

use chronicle_domain::{
    CONTRACT_VERSION, ChunkId, ChunkRevision, ChunkRevisionId, ChunkWindow, DeviceId,
    EventEnvelope, EventPayload, ObservationContent, OcrChange, OcrExtract,
    UntrustedEvidenceMarker,
};
use chronicle_store::checksum::{canonical_json, checksum_bytes};
use chrono::{DateTime, Duration, Utc};

use crate::coverage::{assign_coverage, event_order};
use crate::duration::{application_transitions, duration_estimates};
use crate::{EngineError, Result};

const MAX_OCR_EXTRACTS: usize = 8;
const MAX_OCR_EXTRACT_CHARS: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkerConfig {
    pub aggregator_version: String,
    pub max_cadence_seconds: u32,
}

impl ChunkerConfig {
    pub fn validate(&self) -> Result<()> {
        if self.aggregator_version.is_empty() || !matches!(self.max_cadence_seconds, 30 | 60) {
            return Err(EngineError::Configuration(
                "aggregator version and 30/60 second maximum cadence are required".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ChunkBuild<'a> {
    pub events: &'a [EventEnvelope],
    pub bucket_start: DateTime<Utc>,
    pub prior: Option<&'a ChunkRevision>,
    pub store_generation: u64,
    /// Persisted by reconciliation for provenance-only rebuilds so retries use
    /// the same current generation instant and canonical bytes.
    pub revision_generated_at: Option<DateTime<Utc>>,
    pub config: &'a ChunkerConfig,
}

pub fn chunk_id(device_id: &DeviceId, bucket_start: DateTime<Utc>) -> Result<ChunkId> {
    if bucket_start.timestamp().rem_euclid(300) != 0 {
        return Err(EngineError::Aggregation(
            "chunk start is not a UTC epoch multiple of 300 seconds".to_owned(),
        ));
    }
    let digest = checksum_bytes(format!("{}:{}", device_id, bucket_start.timestamp()).as_bytes());
    ChunkId::new(format!("chunk-{}", &digest[..24])).map_err(EngineError::from)
}

pub fn build_chunk(request: ChunkBuild<'_>) -> Result<Option<ChunkRevision>> {
    request.config.validate()?;
    if request.store_generation == 0 {
        return Err(EngineError::Configuration(
            "store generation must be nonzero".to_owned(),
        ));
    }
    if request.bucket_start.timestamp().rem_euclid(300) != 0 {
        return Err(EngineError::Aggregation(
            "chunk start is not UTC aligned".to_owned(),
        ));
    }
    let bucket_end = request.bucket_start + Duration::seconds(300);
    let mut events = request
        .events
        .iter()
        .filter(|event| match &event.payload {
            EventPayload::ObservationAttempt(_) => event
                .scheduled_at
                .is_some_and(|at| request.bucket_start <= at && at < bucket_end),
            EventPayload::RecordingGap(gap) => {
                gap.start < bucket_end && gap.end > request.bucket_start
            }
            EventPayload::ScreenshotLifecycle(_) => false,
        })
        .cloned()
        .collect::<Vec<_>>();
    if events.is_empty() {
        return Ok(None);
    }
    events.sort_by(event_order);
    let effective_cadence_seconds = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ObservationAttempt(attempt) => Some(attempt.cadence_seconds),
            EventPayload::RecordingGap(_) | EventPayload::ScreenshotLifecycle(_) => None,
        })
        .max()
        .unwrap_or(request.config.max_cadence_seconds)
        .max(request.config.max_cadence_seconds);
    let mut devices = events
        .iter()
        .map(|event| event.device_id.clone())
        .collect::<BTreeSet<_>>();
    if devices.len() != 1 {
        return Err(EngineError::Aggregation(
            "one chunk cannot combine multiple device IDs".to_owned(),
        ));
    }
    let device_id = devices
        .pop_first()
        .ok_or_else(|| EngineError::Aggregation("chunk has no device".to_owned()))?;
    if request.prior.as_ref().is_some_and(|prior| {
        prior.window.start != request.bucket_start || prior.window.end != bucket_end
    }) {
        return Err(EngineError::Aggregation(
            "prior revision belongs to a different chunk window".to_owned(),
        ));
    }
    let coverage = assign_coverage(&events, request.bucket_start)?;
    let input_bytes = canonical_json(&events)?;
    let input_digest = checksum_bytes(&input_bytes);
    let chunk_id = chunk_id(&device_id, request.bucket_start)?;
    if request
        .prior
        .as_ref()
        .is_some_and(|prior| prior.chunk_id != chunk_id)
    {
        return Err(EngineError::Aggregation(
            "prior revision has a different deterministic chunk ID".to_owned(),
        ));
    }
    if let Some(prior) = request.prior
        && prior.input_digest == input_digest
        && prior.aggregator_version == request.config.aggregator_version
        && prior.store_generation == request.store_generation
    {
        return Ok(Some(prior.clone()));
    }
    let due = bucket_end + Duration::seconds(i64::from(effective_cadence_seconds));
    let latest_recorded = events
        .iter()
        .map(|event| event.recorded_at)
        .max()
        .unwrap_or(bucket_end);
    let provenance_only_revision = request.prior.is_some_and(|prior| {
        prior.input_digest == input_digest
            && (prior.aggregator_version != request.config.aggregator_version
                || prior.store_generation != request.store_generation)
    });
    let generated_at = if provenance_only_revision {
        request
            .revision_generated_at
            .ok_or_else(|| {
                EngineError::Aggregation(
                    "provenance-only revision requires a persisted generation instant".to_owned(),
                )
            })?
            .max(due)
            .max(latest_recorded)
    } else {
        due.max(latest_recorded)
    };
    let revision_material = format!(
        "{}:{}:{}:{}:{}",
        chunk_id,
        request.config.aggregator_version,
        input_digest,
        request.store_generation,
        generated_at.to_rfc3339(),
    );
    let revision_digest = checksum_bytes(revision_material.as_bytes());
    let revision_id = ChunkRevisionId::new(format!("chunk-rev-{}", &revision_digest[..32]))?;
    let supporting_event_ids = events.iter().map(|event| event.event_id.clone()).collect();
    let display_timezone = events
        .last()
        .map(|event| event.display_timezone.clone())
        .ok_or_else(|| EngineError::Aggregation("chunk has no timezone".to_owned()))?;
    let prior_revision_id = request.prior.map(|prior| prior.revision_id.clone());
    let late_input = latest_recorded > due
        || request
            .prior
            .is_some_and(|prior| prior.input_digest != input_digest);
    let chunk = ChunkRevision {
        schema_version: CONTRACT_VERSION.to_owned(),
        chunk_id,
        revision_id,
        prior_revision_id: prior_revision_id.clone(),
        supersedes_revision_id: prior_revision_id,
        window: ChunkWindow {
            start: request.bucket_start,
            end: bucket_end,
        },
        generated_at,
        display_timezone,
        aggregator_version: request.config.aggregator_version.clone(),
        input_digest,
        store_generation: request.store_generation,
        finalization_cadence_seconds: effective_cadence_seconds,
        evidence_seconds: coverage.evidence_seconds.clone(),
        presence_seconds: coverage.presence_seconds.clone(),
        duration_estimates: duration_estimates(&events, &coverage),
        transitions: application_transitions(&events),
        ocr_extracts: ocr_extracts(&events),
        gaps: coverage.gaps,
        supporting_event_ids,
        late_input,
    };
    chunk.validate().map_err(EngineError::Aggregation)?;
    Ok(Some(chunk))
}

fn ocr_extracts(events: &[EventEnvelope]) -> Vec<OcrExtract> {
    let mut extracts = Vec::new();
    let mut seen = BTreeSet::new();
    for event in events {
        let EventPayload::ObservationAttempt(attempt) = &event.payload else {
            continue;
        };
        let ObservationContent::Captured(content) = &attempt.content else {
            continue;
        };
        let Some(ocr) = &content.ocr else {
            continue;
        };
        let text = ocr.text.trim();
        if text.is_empty() || ocr.change == OcrChange::Unchanged || !seen.insert(text.to_owned()) {
            continue;
        }
        extracts.push(OcrExtract {
            text: text.chars().take(MAX_OCR_EXTRACT_CHARS).collect(),
            source_event_id: event.event_id.clone(),
            untrusted_evidence: UntrustedEvidenceMarker,
        });
        if extracts.len() == MAX_OCR_EXTRACTS {
            break;
        }
    }
    extracts
}
