use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ArtifactId, ArtifactRevisionId, ChunkId, ClientId, ContentClass, ContractError, EventId,
    GrantId, GrantSummary, QueryArtifact, QueryCapability, RequestId, parse_versioned,
    validate_schema_version,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactType {
    Annotation,
    Tag,
    Hypothesis,
    Report,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactStatus {
    Draft,
    Accepted,
    Rejected,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorKind {
    User,
    Consultant,
    McpClient,
    Model,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorIdentity {
    pub kind: AuthorKind,
    pub display_name: Option<String>,
    pub client_id: Option<ClientId>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceReferences {
    pub event_ids: Vec<EventId>,
    pub chunk_ids: Vec<ChunkId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DerivedArtifactRevision {
    pub schema_version: String,
    pub artifact_id: ArtifactId,
    pub revision_id: ArtifactRevisionId,
    pub prior_revision_id: Option<ArtifactRevisionId>,
    pub expected_prior_revision_id: Option<ArtifactRevisionId>,
    pub artifact_type: ArtifactType,
    pub author: AuthorIdentity,
    pub created_at: DateTime<Utc>,
    pub status: ArtifactStatus,
    pub payload: Value,
    pub evidence: EvidenceReferences,
    pub confidence: Option<f32>,
    pub store_generation: u64,
}

impl DerivedArtifactRevision {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        let artifact: Self = parse_versioned(json)?;
        artifact.validate().map_err(ContractError::Validation)?;
        Ok(artifact)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        if self.store_generation == 0 {
            return Err("derived artifact requires a nonzero store generation".to_owned());
        }
        if self.evidence.event_ids.is_empty() && self.evidence.chunk_ids.is_empty() {
            return Err("derived artifacts require at least one evidence reference".to_owned());
        }
        validate_unique(
            &self.evidence.event_ids,
            "derived artifact event references",
        )?;
        validate_unique(
            &self.evidence.chunk_ids,
            "derived artifact chunk references",
        )?;
        if let Some(confidence) = self.confidence
            && !(0.0..=1.0).contains(&confidence)
        {
            return Err("confidence must be between zero and one".to_owned());
        }
        if self.prior_revision_id != self.expected_prior_revision_id {
            return Err(
                "prior and expected-prior revision must match in a committed revision".to_owned(),
            );
        }
        if self
            .prior_revision_id
            .as_ref()
            .is_some_and(|prior| prior == &self.revision_id)
        {
            return Err("a derived artifact revision cannot supersede itself".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DerivedArtifactWriteRequest {
    pub schema_version: String,
    pub request_id: RequestId,
    pub client_id: ClientId,
    pub grant_id: GrantId,
    pub store_generation: u64,
    pub revision: DerivedArtifactRevision,
}

impl DerivedArtifactWriteRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        self.revision.validate()?;
        if self.store_generation == 0 || self.revision.store_generation != self.store_generation {
            return Err("derived write request and revision generations disagree".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DerivedArtifactWriteResponse {
    pub schema_version: String,
    pub request_id: RequestId,
    pub generated_at: DateTime<Utc>,
    pub store_generation: u64,
    pub grant: GrantSummary,
    pub artifact: QueryArtifact,
}

impl DerivedArtifactWriteResponse {
    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        self.grant.validate_active_at(self.generated_at)?;
        let author_client = self
            .artifact
            .author
            .client_id
            .as_ref()
            .ok_or_else(|| "derived write response requires client attribution".to_owned())?;
        if self.store_generation == 0
            || self.grant.store_generation != self.store_generation
            || self.artifact.store_generation != self.store_generation
            || &self.grant.client_id != author_client
            || !self.grant.content_classes.contains(&ContentClass::Metadata)
            || !self.grant.content_classes.contains(&ContentClass::Derived)
            || !self.grant.capabilities.contains(&QueryCapability::Metadata)
            || !self
                .grant
                .capabilities
                .contains(&QueryCapability::DerivedWrite)
        {
            return Err("derived write response identities or generations disagree".to_owned());
        }
        self.artifact.validate_public()
    }
}

fn validate_unique<T: Eq + std::hash::Hash>(items: &[T], label: &str) -> Result<(), String> {
    let unique = items.iter().collect::<std::collections::HashSet<_>>();
    (unique.len() == items.len())
        .then_some(())
        .ok_or_else(|| format!("{label} must be unique"))
}
