use std::cmp::Ordering;

use chronicle_domain::{
    ChunkGap, ChunkGapKind, EventEnvelope, EventId, EventPayload, EvidenceSeconds, EvidenceState,
    GapReason, ObservationContent, PresenceSeconds, PresenceState,
};
use chrono::{DateTime, Duration, Utc};

use crate::{EngineError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EvidenceClass {
    Captured,
    Protected,
    Paused,
    Unavailable,
    Error,
    Gap,
}

#[derive(Clone, Debug)]
pub(crate) struct AssignedSecond {
    pub start: DateTime<Utc>,
    pub class: EvidenceClass,
    pub presence: Option<PresenceState>,
    pub event_id: Option<EventId>,
    pub event_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct CoverageAssignment {
    pub evidence_seconds: EvidenceSeconds,
    pub presence_seconds: PresenceSeconds,
    pub gaps: Vec<ChunkGap>,
    pub(crate) seconds: Vec<AssignedSecond>,
}

#[derive(Clone, Debug)]
struct SampleSpan {
    event_index: usize,
    center_millis: i64,
    start_millis: i64,
    end_millis: i64,
    class: EvidenceClass,
    presence: Option<PresenceState>,
    event_id: EventId,
}

#[derive(Clone, Debug)]
struct ExplicitGap {
    start_millis: i64,
    end_millis: i64,
    class: EvidenceClass,
    event_id: EventId,
}

pub fn assign_coverage(
    events: &[EventEnvelope],
    bucket_start: DateTime<Utc>,
) -> Result<CoverageAssignment> {
    let bucket_end = bucket_start + Duration::seconds(300);
    let mut sample_indexes = events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(event.payload, EventPayload::ObservationAttempt(_))
                && event
                    .scheduled_at
                    .is_some_and(|at| bucket_start <= at && at < bucket_end)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    sample_indexes.sort_by(|left, right| event_order(&events[*left], &events[*right]));
    let mut spans = Vec::with_capacity(sample_indexes.len());
    for (position, event_index) in sample_indexes.iter().copied().enumerate() {
        let event = &events[event_index];
        let EventPayload::ObservationAttempt(attempt) = &event.payload else {
            continue;
        };
        let center = event
            .scheduled_at
            .ok_or_else(|| {
                EngineError::Aggregation("observation attempt has no scheduled_at".to_owned())
            })?
            .timestamp_millis();
        let cadence_millis = i64::from(attempt.cadence_seconds) * 1_000;
        let cap_half = cadence_millis * 3 / 4;
        let default_half = cadence_millis / 2;
        let previous = position
            .checked_sub(1)
            .and_then(|index| sample_indexes.get(index))
            .map(|index| &events[*index]);
        let next = sample_indexes
            .get(position + 1)
            .map(|index| &events[*index]);
        let start = previous.map_or(bucket_start.timestamp_millis(), |previous| {
            let previous_center = previous
                .scheduled_at
                .map(|at| at.timestamp_millis())
                .unwrap_or(previous.observed_at.timestamp_millis());
            let distance = center.saturating_sub(previous_center);
            if distance <= cadence_millis.saturating_mul(3) / 2 {
                previous_center + distance / 2
            } else {
                center - default_half
            }
        });
        let end = next.map_or(bucket_end.timestamp_millis(), |next| {
            let next_center = next
                .scheduled_at
                .map(|at| at.timestamp_millis())
                .unwrap_or(next.observed_at.timestamp_millis());
            let distance = next_center.saturating_sub(center);
            if distance <= cadence_millis.saturating_mul(3) / 2 {
                center + distance / 2
            } else {
                center + default_half
            }
        });
        spans.push(SampleSpan {
            event_index,
            center_millis: center,
            start_millis: start
                .max(center - cap_half)
                .max(bucket_start.timestamp_millis()),
            end_millis: end
                .min(center + cap_half)
                .min(bucket_end.timestamp_millis()),
            class: evidence_class(attempt.evidence_state),
            presence: (matches!(
                attempt.evidence_state,
                EvidenceState::CapturedNew | EvidenceState::CapturedUnchanged
            ))
            .then_some(attempt.presence_state),
            event_id: event.event_id.clone(),
        });
    }
    let explicit_gaps = events
        .iter()
        .filter_map(|event| {
            let EventPayload::RecordingGap(gap) = &event.payload else {
                return None;
            };
            (gap.start < bucket_end && gap.end > bucket_start).then(|| ExplicitGap {
                start_millis: gap.start.max(bucket_start).timestamp_millis(),
                end_millis: gap.end.min(bucket_end).timestamp_millis(),
                class: match gap.reason {
                    GapReason::PermissionLoss => EvidenceClass::Unavailable,
                    GapReason::StorageOutage => EvidenceClass::Error,
                    GapReason::Sleep | GapReason::Quit | GapReason::ClockCorrection => {
                        EvidenceClass::Gap
                    }
                },
                event_id: event.event_id.clone(),
            })
        })
        .collect::<Vec<_>>();

    let mut seconds = Vec::with_capacity(300);
    for offset in 0..300_i64 {
        let start = bucket_start + Duration::seconds(offset);
        let midpoint = start.timestamp_millis() + 500;
        if let Some(gap) = explicit_gaps
            .iter()
            .filter(|gap| gap.start_millis <= midpoint && midpoint < gap.end_millis)
            .min_by(|left, right| left.event_id.cmp(&right.event_id))
        {
            seconds.push(AssignedSecond {
                start,
                class: gap.class,
                presence: None,
                event_id: Some(gap.event_id.clone()),
                event_index: None,
            });
            continue;
        }
        let chosen = spans
            .iter()
            .filter(|span| span.start_millis <= midpoint && midpoint < span.end_millis)
            .min_by(|left, right| {
                midpoint
                    .abs_diff(left.center_millis)
                    .cmp(&midpoint.abs_diff(right.center_millis))
                    .then_with(|| left.event_id.cmp(&right.event_id))
            });
        if let Some(span) = chosen {
            seconds.push(AssignedSecond {
                start,
                class: span.class,
                presence: span.presence,
                event_id: Some(span.event_id.clone()),
                event_index: Some(span.event_index),
            });
        } else {
            seconds.push(AssignedSecond {
                start,
                class: EvidenceClass::Gap,
                presence: None,
                event_id: None,
                event_index: None,
            });
        }
    }
    let evidence_seconds = EvidenceSeconds {
        captured: count(&seconds, EvidenceClass::Captured),
        protected: count(&seconds, EvidenceClass::Protected),
        paused: count(&seconds, EvidenceClass::Paused),
        unavailable: count(&seconds, EvidenceClass::Unavailable),
        error: count(&seconds, EvidenceClass::Error),
        gap: count(&seconds, EvidenceClass::Gap),
    };
    let presence_seconds = PresenceSeconds {
        active: count_presence(&seconds, PresenceState::Active),
        idle: count_presence(&seconds, PresenceState::Idle),
        unknown: count_presence(&seconds, PresenceState::Unknown),
    };
    if evidence_seconds.total() != 300 || presence_seconds.total() != evidence_seconds.captured {
        return Err(EngineError::Aggregation(
            "coverage assignment did not partition the bucket".to_owned(),
        ));
    }
    Ok(CoverageAssignment {
        gaps: coalesce_gaps(&seconds),
        evidence_seconds,
        presence_seconds,
        seconds,
    })
}

fn evidence_class(state: EvidenceState) -> EvidenceClass {
    match state {
        EvidenceState::CapturedNew | EvidenceState::CapturedUnchanged => EvidenceClass::Captured,
        EvidenceState::Protected => EvidenceClass::Protected,
        EvidenceState::Paused => EvidenceClass::Paused,
        EvidenceState::Unavailable => EvidenceClass::Unavailable,
        EvidenceState::CaptureFailed => EvidenceClass::Error,
    }
}

fn count(seconds: &[AssignedSecond], class: EvidenceClass) -> u32 {
    u32::try_from(
        seconds
            .iter()
            .filter(|second| second.class == class)
            .count(),
    )
    .unwrap_or(u32::MAX)
}

fn count_presence(seconds: &[AssignedSecond], presence: PresenceState) -> u32 {
    u32::try_from(
        seconds
            .iter()
            .filter(|second| second.presence == Some(presence))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

fn coalesce_gaps(seconds: &[AssignedSecond]) -> Vec<ChunkGap> {
    let mut gaps = Vec::new();
    let mut index = 0;
    while index < seconds.len() {
        let second = &seconds[index];
        let Some(kind) = gap_kind(second.class) else {
            index += 1;
            continue;
        };
        let event_id = second.event_id.clone();
        let mut end = index + 1;
        while end < seconds.len()
            && gap_kind(seconds[end].class) == Some(kind)
            && seconds[end].event_id == event_id
        {
            end += 1;
        }
        gaps.push(ChunkGap {
            start: second.start,
            end: seconds[end - 1].start + Duration::seconds(1),
            kind,
            supporting_event_ids: event_id.into_iter().collect(),
        });
        index = end;
    }
    gaps
}

fn gap_kind(class: EvidenceClass) -> Option<ChunkGapKind> {
    match class {
        EvidenceClass::Captured => None,
        EvidenceClass::Protected => Some(ChunkGapKind::Protected),
        EvidenceClass::Paused => Some(ChunkGapKind::Paused),
        EvidenceClass::Unavailable => Some(ChunkGapKind::Unavailable),
        EvidenceClass::Error => Some(ChunkGapKind::Error),
        EvidenceClass::Gap => Some(ChunkGapKind::MissingObservation),
    }
}

pub(crate) fn event_order(left: &EventEnvelope, right: &EventEnvelope) -> Ordering {
    left.observed_at
        .cmp(&right.observed_at)
        .then_with(|| left.event_id.cmp(&right.event_id))
}

pub(crate) fn event_context(
    event: &EventEnvelope,
) -> Option<&chronicle_domain::PermittedWindowContext> {
    let EventPayload::ObservationAttempt(attempt) = &event.payload else {
        return None;
    };
    match &attempt.content {
        ObservationContent::Captured(content) => Some(&content.context),
        ObservationContent::Unchanged(content) => Some(&content.context),
        ObservationContent::Protected(_) | ObservationContent::NoEvidence(_) => None,
    }
}
