use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ChunkRevision, ContentClass, GrantId, GrantSummary, QueryArtifact, QueryCapability,
    QueryCoverage, QueryEvent, RequestId, UtcRange, validate_schema_version,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportFormat {
    Json,
    Markdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExportCounts {
    pub events: u64,
    pub chunks: u64,
    pub artifacts: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalCutoff {
    pub family: String,
    pub shard: String,
    pub byte_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportChecksum {
    pub component: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextPacketManifest {
    pub included_counts: ExportCounts,
    pub available_counts: ExportCounts,
    pub included_content_classes: Vec<String>,
    pub excluded_content_classes: Vec<String>,
    pub journal_cutoffs: Vec<JournalCutoff>,
    pub content_sha256: String,
    pub truncated: bool,
}

impl ContextPacketManifest {
    pub fn validate(&self) -> Result<(), String> {
        validate_counts(&self.included_counts, &self.available_counts)?;
        validate_class_inventory(
            &self.included_content_classes,
            &self.excluded_content_classes,
        )?;
        validate_journal_cutoffs(&self.journal_cutoffs)?;
        validate_sha256(&self.content_sha256)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub schema_version: String,
    pub range: UtcRange,
    pub stable_cutoff: DateTime<Utc>,
    pub store_generation: u64,
    pub included_counts: ExportCounts,
    pub available_counts: ExportCounts,
    pub included_content_classes: Vec<String>,
    pub excluded_content_classes: Vec<String>,
    pub journal_cutoffs: Vec<JournalCutoff>,
    pub checksums: Vec<ExportChecksum>,
    pub coverage: QueryCoverage,
    pub truncated: bool,
}

impl ExportManifest {
    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        self.range.validate()?;
        self.coverage.validate()?;
        if self.store_generation == 0
            || self.coverage.range != self.range
            || self
                .journal_cutoffs
                .iter()
                .any(|cutoff| cutoff.family.is_empty() || cutoff.shard.is_empty())
            || self.checksums.is_empty()
        {
            return Err("export manifest identity, coverage, or cutoff is invalid".to_owned());
        }
        validate_journal_cutoffs(&self.journal_cutoffs)?;
        validate_counts(&self.included_counts, &self.available_counts)?;
        validate_class_inventory(
            &self.included_content_classes,
            &self.excluded_content_classes,
        )?;
        let mut checksum_components = std::collections::HashSet::new();
        for checksum in &self.checksums {
            if checksum.component.is_empty() {
                return Err("export checksum component cannot be empty".to_owned());
            }
            if !checksum_components.insert(&checksum.component) {
                return Err("export checksum components must be unique".to_owned());
            }
            validate_sha256(&checksum.sha256)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum ExportPayload {
    Json {
        events: Vec<QueryEvent>,
        chunks: Vec<ChunkRevision>,
        artifacts: Vec<QueryArtifact>,
    },
    Markdown {
        document: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportRequest {
    pub schema_version: String,
    pub request_id: RequestId,
    pub client_id: crate::ClientId,
    pub grant_id: GrantId,
    pub store_generation: u64,
    pub range: UtcRange,
    pub include_ocr: bool,
    pub include_derived: bool,
    pub format: ExportFormat,
    pub max_bytes: u64,
}

impl ExportRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        self.range.validate()?;
        if self.store_generation == 0
            || self.max_bytes == 0
            || self.max_bytes > crate::MAX_DISCLOSURE_RESPONSE_BYTES
        {
            return Err("export request generation or byte bound is invalid".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportResponse {
    pub schema_version: String,
    pub request_id: RequestId,
    pub generated_at: DateTime<Utc>,
    pub store_generation: u64,
    pub grant: GrantSummary,
    pub manifest: ExportManifest,
    pub payload: ExportPayload,
}

impl ExportResponse {
    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        self.manifest.validate()?;
        self.grant.validate_active_at(self.generated_at)?;
        if self.store_generation == 0
            || self.grant.store_generation != self.store_generation
            || self.manifest.store_generation != self.store_generation
            || self.manifest.stable_cutoff != self.generated_at
            || !self.grant.content_classes.contains(&ContentClass::Metadata)
            || !self.grant.capabilities.contains(&QueryCapability::Metadata)
        {
            return Err("export response identities, generation, or cutoff disagree".to_owned());
        }
        for (class, content, capability) in [
            ("ocr", ContentClass::Ocr, QueryCapability::Ocr),
            (
                "derived",
                ContentClass::Derived,
                QueryCapability::DerivedRead,
            ),
        ] {
            if self
                .manifest
                .included_content_classes
                .iter()
                .any(|included| included == class)
                && (!self.grant.content_classes.contains(&content)
                    || !self.grant.capabilities.contains(&capability))
            {
                return Err("export content inventory exceeds grant capabilities".to_owned());
            }
        }
        match &self.payload {
            ExportPayload::Json {
                events,
                chunks,
                artifacts,
            } => {
                if self.manifest.included_counts.events != events.len() as u64
                    || self.manifest.included_counts.chunks != chunks.len() as u64
                    || self.manifest.included_counts.artifacts != artifacts.len() as u64
                {
                    return Err("export manifest counts disagree with JSON payload".to_owned());
                }
                for artifact in artifacts {
                    artifact.validate_public()?;
                }
                for event in events {
                    event.validate()?;
                }
                for chunk in chunks {
                    crate::query::validate_query_chunk(chunk)?;
                }
            }
            ExportPayload::Markdown { document } if document.is_empty() => {
                return Err("markdown export cannot be empty".to_owned());
            }
            ExportPayload::Markdown { .. } => {}
        }
        Ok(())
    }
}

fn validate_counts(included: &ExportCounts, available: &ExportCounts) -> Result<(), String> {
    if included.events > available.events
        || included.chunks > available.chunks
        || included.artifacts > available.artifacts
    {
        return Err("included export counts exceed available snapshot counts".to_owned());
    }
    Ok(())
}

fn validate_journal_cutoffs(cutoffs: &[JournalCutoff]) -> Result<(), String> {
    let mut identities = std::collections::HashSet::new();
    if cutoffs.iter().any(|cutoff| {
        cutoff.family.is_empty()
            || cutoff.shard.is_empty()
            || !identities.insert((&cutoff.family, &cutoff.shard))
    }) {
        return Err("journal cutoffs must be unique by family and shard".to_owned());
    }
    Ok(())
}

fn validate_class_inventory(included: &[String], excluded: &[String]) -> Result<(), String> {
    let allowed = ["metadata", "ocr", "derived", "screenshots"];
    let all_known = included
        .iter()
        .chain(excluded)
        .all(|class| allowed.contains(&class.as_str()));
    let each_once = allowed.iter().all(|class| {
        usize::from(included.iter().any(|value| value == class))
            + usize::from(excluded.iter().any(|value| value == class))
            == 1
    });
    let included_unique = included
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        == included.len();
    let excluded_unique = excluded
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        == excluded.len();
    if !all_known
        || !each_once
        || !included_unique
        || !excluded_unique
        || !included.iter().any(|class| class == "metadata")
        || !excluded.iter().any(|class| class == "screenshots")
        || included.iter().any(|class| excluded.contains(class))
    {
        return Err(
            "export content-class inventory must explicitly exclude screenshots".to_owned(),
        );
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), String> {
    (value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(())
    .ok_or_else(|| "export checksum must be lowercase SHA-256 hex".to_owned())
}
