use chronicle_domain::{
    DurableAcknowledgement, HealthCode, HealthSeverity, HealthSnapshot, ProjectionHealth,
};
use chrono::{DateTime, Utc};

pub fn healthy(observed_at: DateTime<Utc>) -> HealthSnapshot {
    HealthSnapshot {
        observed_at,
        severity: HealthSeverity::Info,
        code: HealthCode::Healthy,
        projection: ProjectionHealth::Current,
        acknowledgement: Some(DurableAcknowledgement::Durable),
        factual_message: "Canonical evidence and projection are current.".to_owned(),
    }
}

pub fn projection_lag(observed_at: DateTime<Utc>) -> HealthSnapshot {
    HealthSnapshot {
        observed_at,
        severity: HealthSeverity::Warning,
        code: HealthCode::ProjectionLag,
        projection: ProjectionHealth::Lagging,
        acknowledgement: Some(DurableAcknowledgement::JournalDurableProjectionPending),
        factual_message: "Canonical evidence is durable and projection recovery is pending."
            .to_owned(),
    }
}

pub fn aggregation_lag(observed_at: DateTime<Utc>) -> HealthSnapshot {
    HealthSnapshot {
        observed_at,
        severity: HealthSeverity::Warning,
        code: HealthCode::ProjectionLag,
        projection: ProjectionHealth::Lagging,
        acknowledgement: Some(DurableAcknowledgement::Durable),
        factual_message:
            "Canonical event evidence is durable; factual aggregation recovery is pending."
                .to_owned(),
    }
}
