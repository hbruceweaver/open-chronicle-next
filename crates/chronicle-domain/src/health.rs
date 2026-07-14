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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthOperationTimes {
    pub last_scheduled_attempt_at: Option<DateTime<Utc>>,
    pub last_successful_capture_at: Option<DateTime<Utc>>,
    pub last_successful_ocr_at: Option<DateTime<Utc>>,
    pub last_journal_at: Option<DateTime<Utc>>,
    pub last_projection_at: Option<DateTime<Utc>>,
    pub last_chunk_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageHealthSummary {
    pub managed_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpHealthSummary {
    pub active_grants: u32,
    pub revoked_grants: u32,
    pub expired_grants: u32,
    pub exhausted_grants: u32,
    pub stale_generation_grants: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StudyHealthState {
    Personal,
    Scheduled,
    Active,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyHealthSummary {
    pub state: StudyHealthState,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub expired_at: Option<DateTime<Utc>>,
}

impl StudyHealthSummary {
    pub fn validate(&self) -> Result<(), String> {
        match self.state {
            StudyHealthState::Personal
                if self.start.is_none() && self.end.is_none() && self.expired_at.is_none() =>
            {
                Ok(())
            }
            StudyHealthState::Scheduled | StudyHealthState::Active
                if self
                    .start
                    .zip(self.end)
                    .is_some_and(|(start, end)| start < end)
                    && self.expired_at.is_none() =>
            {
                Ok(())
            }
            StudyHealthState::Expired
                if self
                    .start
                    .zip(self.end)
                    .is_some_and(|(start, end)| start < end)
                    && self
                        .expired_at
                        .is_none_or(|expired_at| self.end.is_some_and(|end| expired_at >= end)) =>
            {
                Ok(())
            }
            _ => Err("study health state and boundaries are inconsistent".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotRetentionHealthSummary {
    pub write_pending: u64,
    pub retained: u64,
    pub delete_pending: u64,
    pub expired: u64,
    pub user_deleted: u64,
    pub missing: u64,
    pub write_failed: u64,
    pub next_expiry_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthIssue {
    pub severity: HealthSeverity,
    pub code: HealthCode,
}

/// Content-free operational diagnostics. This contract intentionally has no
/// arbitrary strings, application identities, titles, OCR, or managed paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticHealthSnapshot {
    pub schema_version: String,
    pub observed_at: DateTime<Utc>,
    pub store_generation: u64,
    pub projection: ProjectionHealth,
    pub acknowledgement: DurableAcknowledgement,
    pub latest: HealthOperationTimes,
    pub aggregation_watermark: Option<DateTime<Utc>>,
    pub aggregation_pending_buckets: u64,
    pub projection_lag_seconds: u64,
    pub projection_pending_records: u64,
    pub storage: StorageHealthSummary,
    pub study: StudyHealthSummary,
    pub screenshot_retention: ScreenshotRetentionHealthSummary,
    pub mcp: McpHealthSummary,
    pub issues: Vec<HealthIssue>,
}

impl DiagnosticHealthSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.store_generation == 0 {
            return Err("diagnostic health requires a nonzero store generation".to_owned());
        }
        self.study.validate()?;
        match self.projection {
            ProjectionHealth::Current => {
                if self.acknowledgement != DurableAcknowledgement::Durable
                    || self.projection_pending_records != 0
                    || self.projection_lag_seconds != 0
                {
                    return Err(
                        "current projection health requires durable acknowledgement and no lag"
                            .to_owned(),
                    );
                }
            }
            ProjectionHealth::Lagging => {
                if self.acknowledgement != DurableAcknowledgement::JournalDurableProjectionPending
                    || self.projection_pending_records == 0
                {
                    return Err(
                        "lagging projection health requires pending journal records".to_owned()
                    );
                }
            }
            ProjectionHealth::Rebuilding => {
                if self.acknowledgement != DurableAcknowledgement::JournalDurableProjectionPending {
                    return Err(
                        "rebuilding projection health requires journal-durable acknowledgement"
                            .to_owned(),
                    );
                }
            }
            ProjectionHealth::Blocked => {
                if self.acknowledgement != DurableAcknowledgement::NotDurable {
                    return Err(
                        "blocked projection health requires a not-durable acknowledgement"
                            .to_owned(),
                    );
                }
            }
        }
        for timestamp in [
            self.latest.last_scheduled_attempt_at,
            self.latest.last_successful_capture_at,
            self.latest.last_successful_ocr_at,
            self.latest.last_journal_at,
            self.latest.last_projection_at,
            self.latest.last_chunk_at,
            self.aggregation_watermark,
        ]
        .into_iter()
        .flatten()
        {
            if timestamp > self.observed_at {
                return Err("diagnostic health cannot report future operation times".to_owned());
            }
        }
        Ok(())
    }
}
