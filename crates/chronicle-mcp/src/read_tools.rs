use chronicle_domain::{
    ActivityFilter, ArtifactId, ArtifactRevisionId, ChunkId, EventId, EvidenceState, PageRequest,
    QueryOperation, UtcRange,
};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::limits::{DEFAULT_CONTEXT_BYTES, DEFAULT_PAGE_ITEMS, MAX_PAGE_ITEMS};
use crate::logging::McpServerError;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RangeParams {
    /// Inclusive UTC start timestamp in RFC 3339 form.
    pub start: String,
    /// Exclusive UTC end timestamp in RFC 3339 form.
    pub end: String,
}

impl RangeParams {
    fn parse(self) -> Result<UtcRange, McpServerError> {
        let range = UtcRange {
            start: self
                .start
                .parse()
                .map_err(|_| invalid("start must be an RFC 3339 UTC timestamp"))?,
            end: self
                .end
                .parse()
                .map_err(|_| invalid("end must be an RFC 3339 UTC timestamp"))?,
        };
        range.validate().map_err(invalid)?;
        Ok(range)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivityFilterParams {
    pub range: RangeParams,
    pub application_bundle_id: Option<String>,
    pub window_text: Option<String>,
    pub authorized_domain: Option<String>,
    /// Optional evidence states such as captured-new, protected, paused, or unavailable.
    #[serde(default)]
    pub evidence_states: Vec<String>,
}

impl ActivityFilterParams {
    fn parse(self) -> Result<ActivityFilter, McpServerError> {
        let evidence_states = self
            .evidence_states
            .into_iter()
            .map(|state| {
                serde_json::from_value::<EvidenceState>(serde_json::Value::String(state))
                    .map_err(|_| invalid("evidence_states contains an unsupported value"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ActivityFilter {
            range: self.range.parse()?,
            application_bundle_id: self.application_bundle_id,
            window_text: self.window_text,
            authorized_domain: self.authorized_domain,
            evidence_states,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListChunksParams {
    pub filter: ActivityFilterParams,
    pub cursor: Option<String>,
    #[serde(default = "default_page_items")]
    pub limit: u32,
}

impl ListChunksParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::ListChunks {
            filter: self.filter.parse()?,
            page: page(self.cursor, self.limit)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChunkParams {
    pub chunk_id: String,
}

impl ChunkParams {
    pub fn read_operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::ReadChunk {
            chunk_id: ChunkId::new(self.chunk_id).map_err(|_| invalid("invalid chunk_id"))?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventParams {
    pub event_id: String,
}

impl EventParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::GetEvent {
            event_id: EventId::new(self.event_id).map_err(|_| invalid("invalid event_id"))?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    pub filter: ActivityFilterParams,
    pub query: String,
    /// Search always uses the grant-gated OCR index. This flag controls whether
    /// matching OCR text may be returned in the result.
    #[serde(default)]
    pub include_ocr: bool,
    pub cursor: Option<String>,
    #[serde(default = "default_page_items")]
    pub limit: u32,
}

impl SearchParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        if self.query.trim().is_empty() {
            return Err(invalid("query must not be empty"));
        }
        Ok(QueryOperation::SearchActivity {
            filter: self.filter.parse()?,
            query: self.query,
            include_ocr: self.include_ocr,
            page: page(self.cursor, self.limit)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentParams {
    pub at: String,
}

impl MomentParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::InspectMoment {
            at: self
                .at
                .parse()
                .map_err(|_| invalid("at must be an RFC 3339 UTC timestamp"))?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatisticsParams {
    pub filter: ActivityFilterParams,
}

impl StatisticsParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::Statistics {
            filter: self.filter.parse()?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareParams {
    pub first: RangeParams,
    pub second: RangeParams,
}

impl CompareParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::ComparePeriods {
            first: self.first.parse()?,
            second: self.second.parse()?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SupportingEvidenceParams {
    pub chunk_id: String,
    pub cursor: Option<String>,
    #[serde(default = "default_page_items")]
    pub limit: u32,
}

impl SupportingEvidenceParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::SupportingEvidence {
            chunk_id: ChunkId::new(self.chunk_id).map_err(|_| invalid("invalid chunk_id"))?,
            page: page(self.cursor, self.limit)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextPacketParams {
    pub filter: ActivityFilterParams,
    /// Full OCR is excluded unless both this flag and the disclosure grant allow it.
    #[serde(default)]
    pub include_ocr: bool,
    #[serde(default = "default_context_bytes")]
    pub max_bytes: u64,
}

impl ContextPacketParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        if self.max_bytes == 0 {
            return Err(invalid("max_bytes must be greater than zero"));
        }
        Ok(QueryOperation::BuildContextPacket {
            filter: self.filter.parse()?,
            include_ocr: self.include_ocr,
            max_bytes: self.max_bytes,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CurrentContextParams {
    /// Full OCR is excluded unless both this flag and the disclosure grant allow it.
    #[serde(default)]
    pub include_ocr: bool,
    #[serde(default = "default_context_bytes")]
    pub max_bytes: u64,
}

impl CurrentContextParams {
    /// Builds context for the most recent fully completed five-minute UTC
    /// bucket. An in-progress bucket is never presented as settled evidence.
    pub fn operation(self, now: DateTime<Utc>) -> Result<QueryOperation, McpServerError> {
        if self.max_bytes == 0 {
            return Err(invalid("max_bytes must be greater than zero"));
        }
        let end_seconds = now.timestamp().div_euclid(300) * 300;
        let end = DateTime::from_timestamp(end_seconds, 0)
            .ok_or_else(|| invalid("current time is outside the supported range"))?;
        Ok(QueryOperation::BuildContextPacket {
            filter: ActivityFilter {
                range: UtcRange {
                    start: end - Duration::minutes(5),
                    end,
                },
                application_bundle_id: None,
                window_text: None,
                authorized_domain: None,
                evidence_states: Vec::new(),
            },
            include_ocr: self.include_ocr,
            max_bytes: self.max_bytes,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArtifactsParams {
    pub range: RangeParams,
    pub cursor: Option<String>,
    #[serde(default = "default_page_items")]
    pub limit: u32,
}

impl ListArtifactsParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::ListDerived {
            range: self.range.parse()?,
            page: page(self.cursor, self.limit)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactParams {
    pub artifact_id: String,
    pub revision_id: Option<String>,
}

impl ArtifactParams {
    pub fn operation(self) -> Result<QueryOperation, McpServerError> {
        Ok(QueryOperation::GetArtifact {
            artifact_id: ArtifactId::new(self.artifact_id)
                .map_err(|_| invalid("invalid artifact_id"))?,
            revision_id: self
                .revision_id
                .map(ArtifactRevisionId::new)
                .transpose()
                .map_err(|_| invalid("invalid revision_id"))?,
        })
    }
}

fn page(cursor: Option<String>, limit: u32) -> Result<PageRequest, McpServerError> {
    if limit == 0 {
        return Err(invalid("limit must be greater than zero"));
    }
    if limit > MAX_PAGE_ITEMS {
        return Err(invalid(format!("limit must not exceed {MAX_PAGE_ITEMS}")));
    }
    Ok(PageRequest { cursor, limit })
}

const fn default_page_items() -> u32 {
    DEFAULT_PAGE_ITEMS
}

const fn default_context_bytes() -> u64 {
    DEFAULT_CONTEXT_BYTES
}

fn invalid(message: impl Into<String>) -> McpServerError {
    McpServerError::InvalidInput(message.into())
}
