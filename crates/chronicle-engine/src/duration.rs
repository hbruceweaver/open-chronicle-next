use std::collections::{BTreeMap, BTreeSet};

use chronicle_domain::{
    DimensionKind, DurationEstimate, EventEnvelope, EventPayload, ObservationContent,
    PresenceState, Transition,
};

use crate::coverage::{CoverageAssignment, EvidenceClass, event_context, event_order};

pub fn duration_estimates(
    events: &[EventEnvelope],
    coverage: &CoverageAssignment,
) -> Vec<DurationEstimate> {
    let mut totals = BTreeMap::<
        (DimensionKind, String, String),
        (u32, BTreeSet<chronicle_domain::EventId>),
    >::new();
    for second in &coverage.seconds {
        if second.class != EvidenceClass::Captured || second.presence == Some(PresenceState::Idle) {
            continue;
        }
        let Some(event_index) = second.event_index else {
            continue;
        };
        let event = &events[event_index];
        let Some(context) = event_context(event) else {
            continue;
        };
        add(
            &mut totals,
            DimensionKind::Application,
            context.application_bundle_id.clone(),
            context.process_name.clone(),
            event.event_id.clone(),
        );
        let window_label = context
            .window_title
            .clone()
            .unwrap_or_else(|| format!("{} — Untitled", context.process_name));
        let window_key = format!(
            "{}:{}",
            context.application_bundle_id,
            context.window_title.as_deref().unwrap_or("untitled")
        );
        add(
            &mut totals,
            DimensionKind::Window,
            window_key,
            window_label,
            event.event_id.clone(),
        );
        if let Some(domain) = &context.authorized_domain {
            add(
                &mut totals,
                DimensionKind::AuthorizedDomain,
                domain.domain.clone(),
                domain.domain.clone(),
                event.event_id.clone(),
            );
        }
    }
    totals
        .into_iter()
        .map(
            |((dimension, key, label), (estimated_seconds, supporting_event_ids))| {
                DurationEstimate {
                    dimension,
                    key,
                    label,
                    estimated_seconds,
                    supporting_event_ids: supporting_event_ids.into_iter().collect(),
                }
            },
        )
        .collect()
}

fn add(
    totals: &mut BTreeMap<
        (DimensionKind, String, String),
        (u32, BTreeSet<chronicle_domain::EventId>),
    >,
    dimension: DimensionKind,
    key: String,
    label: String,
    event_id: chronicle_domain::EventId,
) {
    let entry = totals
        .entry((dimension, key, label))
        .or_insert_with(|| (0, BTreeSet::new()));
    entry.0 = entry.0.saturating_add(1);
    entry.1.insert(event_id);
}

pub fn application_transitions(events: &[EventEnvelope]) -> Vec<Transition> {
    let mut observations = events
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                EventPayload::ObservationAttempt(attempt)
                    if matches!(
                        attempt.content,
                        ObservationContent::Captured(_) | ObservationContent::Unchanged(_)
                    )
            )
        })
        .collect::<Vec<_>>();
    observations.sort_by(|left, right| event_order(left, right));
    let mut transitions = Vec::new();
    let mut prior_key: Option<String> = None;
    for event in observations {
        let Some(context) = event_context(event) else {
            continue;
        };
        let current = context.application_bundle_id.clone();
        if prior_key.as_ref().is_some_and(|prior| prior != &current) {
            transitions.push(Transition {
                at: event.observed_at,
                from_key: prior_key.clone(),
                to_key: current.clone(),
                supporting_event_id: event.event_id.clone(),
            });
        }
        prior_key = Some(current);
    }
    transitions
}
