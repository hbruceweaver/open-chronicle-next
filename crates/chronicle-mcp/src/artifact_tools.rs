use chronicle_domain::{
    ArtifactId, ArtifactRevisionId, ArtifactStatus, ArtifactType, AuthorIdentity, AuthorKind,
    ChunkId, ClientId, DerivedArtifactRevision, EventId, EvidenceReferences, QueryArtifact,
    RequestId,
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::logging::McpServerError;

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactTypeParam {
    Annotation,
    Tag,
    Hypothesis,
    Report,
}

impl From<ArtifactTypeParam> for ArtifactType {
    fn from(value: ArtifactTypeParam) -> Self {
        match value {
            ArtifactTypeParam::Annotation => Self::Annotation,
            ArtifactTypeParam::Tag => Self::Tag,
            ArtifactTypeParam::Hypothesis => Self::Hypothesis,
            ArtifactTypeParam::Report => Self::Report,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactStatusParam {
    Draft,
    Accepted,
    Rejected,
    Superseded,
}

impl From<ArtifactStatusParam> for ArtifactStatus {
    fn from(value: ArtifactStatusParam) -> Self {
        match value {
            ArtifactStatusParam::Draft => Self::Draft,
            ArtifactStatusParam::Accepted => Self::Accepted,
            ArtifactStatusParam::Rejected => Self::Rejected,
            ArtifactStatusParam::Superseded => Self::Superseded,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactAuthorKindParam {
    McpClient,
    Model,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAuthorParams {
    pub kind: ArtifactAuthorKindParam,
    pub display_name: Option<String>,
    /// Required for model-authored analysis and forbidden for MCP-client authorship.
    pub model: Option<String>,
}

impl ArtifactAuthorParams {
    fn parse(self, client_id: &ClientId) -> Result<AuthorIdentity, McpServerError> {
        if self
            .display_name
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(invalid("display_name must not be blank"));
        }
        let (kind, model) = match self.kind {
            ArtifactAuthorKindParam::McpClient => {
                if self.model.is_some() {
                    return Err(invalid("model is forbidden for mcp-client authorship"));
                }
                (AuthorKind::McpClient, None)
            }
            ArtifactAuthorKindParam::Model => {
                let model = self
                    .model
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| invalid("model is required for model authorship"))?;
                (AuthorKind::Model, Some(model))
            }
        };
        Ok(AuthorIdentity {
            kind,
            display_name: self.display_name,
            client_id: Some(client_id.clone()),
            model,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReferenceParams {
    #[serde(default)]
    pub event_ids: Vec<String>,
    #[serde(default)]
    pub chunk_ids: Vec<String>,
}

impl EvidenceReferenceParams {
    fn parse(self) -> Result<EvidenceReferences, McpServerError> {
        let event_ids = self
            .event_ids
            .into_iter()
            .map(|value| EventId::new(value).map_err(|_| invalid("invalid evidence event_id")))
            .collect::<Result<Vec<_>, _>>()?;
        let chunk_ids = self
            .chunk_ids
            .into_iter()
            .map(|value| ChunkId::new(value).map_err(|_| invalid("invalid evidence chunk_id")))
            .collect::<Result<Vec<_>, _>>()?;
        if event_ids.is_empty() && chunk_ids.is_empty() {
            return Err(invalid("at least one evidence reference is required"));
        }
        Ok(EvidenceReferences {
            event_ids,
            chunk_ids,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArtifactParams {
    /// Stable caller-generated ID used to make exact retries safe.
    pub request_id: String,
    pub artifact_id: String,
    pub revision_id: String,
    pub artifact_type: ArtifactTypeParam,
    pub author: ArtifactAuthorParams,
    pub payload: serde_json::Value,
    pub evidence: EvidenceReferenceParams,
    pub confidence: Option<f32>,
}

impl CreateArtifactParams {
    pub(crate) fn prepare(
        self,
        client_id: &ClientId,
    ) -> Result<PreparedArtifactWrite, McpServerError> {
        PreparedArtifactWrite::new(
            self.request_id,
            self.artifact_id,
            self.revision_id,
            None,
            self.artifact_type.into(),
            self.author.parse(client_id)?,
            ArtifactStatus::Draft,
            self.payload,
            self.evidence.parse()?,
            self.confidence,
        )
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviseArtifactParams {
    /// Stable caller-generated ID used to make exact retries safe.
    pub request_id: String,
    pub artifact_id: String,
    pub revision_id: String,
    pub expected_prior_revision_id: String,
    pub artifact_type: ArtifactTypeParam,
    pub author: ArtifactAuthorParams,
    pub status: ArtifactStatusParam,
    pub payload: serde_json::Value,
    pub evidence: EvidenceReferenceParams,
    pub confidence: Option<f32>,
}

impl ReviseArtifactParams {
    pub(crate) fn prepare(
        self,
        client_id: &ClientId,
    ) -> Result<PreparedArtifactWrite, McpServerError> {
        PreparedArtifactWrite::new(
            self.request_id,
            self.artifact_id,
            self.revision_id,
            Some(self.expected_prior_revision_id),
            self.artifact_type.into(),
            self.author.parse(client_id)?,
            self.status.into(),
            self.payload,
            self.evidence.parse()?,
            self.confidence,
        )
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArtifactStatusParams {
    /// Stable caller-generated ID used to make exact retries safe.
    pub request_id: String,
    pub artifact_id: String,
    pub revision_id: String,
    pub expected_prior_revision_id: String,
    pub author: ArtifactAuthorParams,
    pub status: ArtifactStatusParam,
}

impl SetArtifactStatusParams {
    pub(crate) fn prepare(
        self,
        client_id: &ClientId,
    ) -> Result<PreparedStatusWrite, McpServerError> {
        let artifact_id =
            ArtifactId::new(self.artifact_id).map_err(|_| invalid("invalid artifact_id"))?;
        let revision_id = ArtifactRevisionId::new(self.revision_id)
            .map_err(|_| invalid("invalid revision_id"))?;
        let expected_prior_revision_id =
            ArtifactRevisionId::new(self.expected_prior_revision_id)
                .map_err(|_| invalid("invalid expected_prior_revision_id"))?;
        if revision_id == expected_prior_revision_id {
            return Err(invalid("revision_id must differ from the expected prior"));
        }
        Ok(PreparedStatusWrite {
            request_id: RequestId::new(self.request_id)
                .map_err(|_| invalid("invalid request_id"))?,
            artifact_id,
            revision_id,
            expected_prior_revision_id,
            author: self.author.parse(client_id)?,
            status: self.status.into(),
        })
    }
}

pub(crate) struct PreparedArtifactWrite {
    pub request_id: RequestId,
    artifact_id: ArtifactId,
    revision_id: ArtifactRevisionId,
    prior_revision_id: Option<ArtifactRevisionId>,
    artifact_type: ArtifactType,
    author: AuthorIdentity,
    status: ArtifactStatus,
    payload: serde_json::Value,
    evidence: EvidenceReferences,
    confidence: Option<f32>,
}

impl PreparedArtifactWrite {
    #[allow(clippy::too_many_arguments)]
    fn new(
        request_id: String,
        artifact_id: String,
        revision_id: String,
        prior_revision_id: Option<String>,
        artifact_type: ArtifactType,
        author: AuthorIdentity,
        status: ArtifactStatus,
        payload: serde_json::Value,
        evidence: EvidenceReferences,
        confidence: Option<f32>,
    ) -> Result<Self, McpServerError> {
        let revision_id =
            ArtifactRevisionId::new(revision_id).map_err(|_| invalid("invalid revision_id"))?;
        let prior_revision_id = prior_revision_id
            .map(ArtifactRevisionId::new)
            .transpose()
            .map_err(|_| invalid("invalid expected_prior_revision_id"))?;
        if prior_revision_id.as_ref() == Some(&revision_id) {
            return Err(invalid("revision_id must differ from the expected prior"));
        }
        if confidence.is_some_and(|value| !(0.0..=1.0).contains(&value)) {
            return Err(invalid("confidence must be between zero and one"));
        }
        Ok(Self {
            request_id: RequestId::new(request_id).map_err(|_| invalid("invalid request_id"))?,
            artifact_id: ArtifactId::new(artifact_id)
                .map_err(|_| invalid("invalid artifact_id"))?,
            revision_id,
            prior_revision_id,
            artifact_type,
            author,
            status,
            payload,
            evidence,
            confidence,
        })
    }

    pub fn revision(
        self,
        store_generation: u64,
        created_at: DateTime<Utc>,
    ) -> DerivedArtifactRevision {
        DerivedArtifactRevision {
            schema_version: "1.0".to_owned(),
            artifact_id: self.artifact_id,
            revision_id: self.revision_id,
            prior_revision_id: self.prior_revision_id.clone(),
            expected_prior_revision_id: self.prior_revision_id,
            artifact_type: self.artifact_type,
            author: self.author,
            created_at,
            status: self.status,
            payload: self.payload,
            evidence: self.evidence,
            confidence: self.confidence,
            store_generation,
        }
    }
}

pub(crate) struct PreparedStatusWrite {
    pub request_id: RequestId,
    pub artifact_id: ArtifactId,
    pub revision_id: ArtifactRevisionId,
    pub expected_prior_revision_id: ArtifactRevisionId,
    author: AuthorIdentity,
    status: ArtifactStatus,
}

impl PreparedStatusWrite {
    pub fn revision(
        self,
        prior: QueryArtifact,
        store_generation: u64,
        created_at: DateTime<Utc>,
    ) -> Result<DerivedArtifactRevision, McpServerError> {
        if prior.artifact_id != self.artifact_id
            || prior.revision_id != self.expected_prior_revision_id
        {
            return Err(McpServerError::Service(
                chronicle_engine::SharedServiceError::ArtifactConflict,
            ));
        }
        Ok(DerivedArtifactRevision {
            schema_version: "1.0".to_owned(),
            artifact_id: self.artifact_id,
            revision_id: self.revision_id,
            prior_revision_id: Some(self.expected_prior_revision_id.clone()),
            expected_prior_revision_id: Some(self.expected_prior_revision_id),
            artifact_type: prior.artifact_type,
            author: self.author,
            created_at,
            status: self.status,
            payload: prior.payload,
            evidence: prior.evidence,
            confidence: prior.confidence,
            store_generation,
        })
    }
}

fn invalid(message: impl Into<String>) -> McpServerError {
    McpServerError::InvalidInput(message.into())
}
