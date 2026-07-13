use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DurableAcknowledgement {
    Durable,
    JournalDurableProjectionPending,
    NotDurable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionHealth {
    Current,
    Lagging,
    Rebuilding,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthCode {
    Healthy,
    ProjectionLag,
    StorageUnavailable,
    CorruptCanonicalRecord,
    PermissionDenied,
    CaptureUnavailable,
    StudyExpired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub observed_at: DateTime<Utc>,
    pub severity: HealthSeverity,
    pub code: HealthCode,
    pub projection: ProjectionHealth,
    pub acknowledgement: Option<DurableAcknowledgement>,
    pub factual_message: String,
}
