use chronicle_engine::SharedServiceError;
use rmcp::model::CallToolResult;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpServerError {
    #[error("invalid server configuration")]
    InvalidConfiguration,
    #[error("invalid tool input: {0}")]
    InvalidInput(String),
    #[error("Chronicle request was denied or unavailable")]
    Service(#[source] SharedServiceError),
    #[error("Chronicle worker did not complete")]
    Worker,
}

impl McpServerError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid-configuration",
            Self::InvalidInput(_) => "invalid-input",
            Self::Service(error) => service_code(error),
            Self::Worker => "worker-unavailable",
        }
    }

    pub fn caller_message(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "The Chronicle MCP registration is incomplete.",
            Self::InvalidInput(_) => "The tool input is invalid.",
            Self::Service(SharedServiceError::GrantNotFound) => {
                "No disclosure grant exists for this MCP client."
            }
            Self::Service(SharedServiceError::GrantClientMismatch) => {
                "The disclosure grant belongs to a different MCP client."
            }
            Self::Service(SharedServiceError::GrantInactive) => {
                "The disclosure grant is expired, revoked, exhausted, or otherwise inactive."
            }
            Self::Service(SharedServiceError::ContentDenied(_)) => {
                "The disclosure grant does not allow that content class."
            }
            Self::Service(SharedServiceError::RangeDenied | SharedServiceError::RangeLimit) => {
                "The requested time range is outside the disclosure grant."
            }
            Self::Service(SharedServiceError::ResponseByteLimit) => {
                "The response would exceed the disclosure grant's byte limit."
            }
            Self::Service(SharedServiceError::CursorNotFound)
            | Self::Service(SharedServiceError::CursorScopeMismatch) => {
                "The pagination cursor is expired or does not match this query."
            }
            Self::Service(SharedServiceError::NotFound) => {
                "No grant-visible Chronicle evidence matched that identifier."
            }
            Self::Service(SharedServiceError::StaleGeneration { .. }) => {
                "The Chronicle store was reset; this grant must be recreated."
            }
            Self::Service(_) => "Chronicle could not complete the request.",
            Self::Worker => "Chronicle could not complete the request.",
        }
    }

    pub fn tool_result(&self) -> CallToolResult {
        CallToolResult::structured_error(json!({
            "schema_version": "1.0",
            "error": {
                "code": self.code(),
                "message": self.caller_message(),
            }
        }))
    }
}

fn service_code(error: &SharedServiceError) -> &'static str {
    match error {
        SharedServiceError::GrantNotFound => "grant-not-found",
        SharedServiceError::GrantClientMismatch => "grant-client-mismatch",
        SharedServiceError::GrantInactive => "grant-inactive",
        SharedServiceError::ContentDenied(_) => "content-denied",
        SharedServiceError::RangeDenied | SharedServiceError::RangeLimit => "range-denied",
        SharedServiceError::CursorScopeMismatch | SharedServiceError::CursorNotFound => {
            "cursor-denied"
        }
        SharedServiceError::ResponseByteLimit => "response-limit",
        SharedServiceError::NotFound => "not-found",
        SharedServiceError::InvalidEvidenceReference => "invalid-evidence-reference",
        SharedServiceError::InvalidArtifactTransition => "invalid-artifact-transition",
        SharedServiceError::ArtifactConflict => "artifact-conflict",
        SharedServiceError::StaleGeneration { .. } => "stale-generation",
        SharedServiceError::Contract(_) | SharedServiceError::Store(_) => "request-unavailable",
    }
}
