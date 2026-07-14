use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ContractError, DerivedArtifactWriteRequest, DerivedArtifactWriteResponse,
    DiagnosticHealthSnapshot, ExportRequest, ExportResponse, QueryRequest, QueryResponse,
    RequestId, parse_versioned, validate_schema_version,
};

pub const MAX_SHARED_REQUEST_BYTES: usize = 64 * 1024;
const FORBIDDEN_TRANSPORT_KEYS: &[&str] = &[
    "managed_relative_path",
    "path",
    "bytes",
    "image_bytes",
    "screenshot_bytes",
    "encoded_image",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum SharedServiceOperation {
    Health,
    Query(Box<QueryRequest>),
    WriteDerived(Box<DerivedArtifactWriteRequest>),
    Export(Box<ExportRequest>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SharedServiceRequest {
    pub schema_version: String,
    pub request_id: RequestId,
    pub store_generation: u64,
    pub operation: SharedServiceOperation,
}

impl SharedServiceRequest {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        if json.len() > MAX_SHARED_REQUEST_BYTES {
            return Err(ContractError::Validation(format!(
                "shared request exceeds {MAX_SHARED_REQUEST_BYTES} bytes"
            )));
        }
        reject_transport_paths_and_bytes(json)?;
        let request: Self = parse_versioned(json)?;
        request.validate().map_err(ContractError::Validation)?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        if self.store_generation == 0 {
            return Err("shared request requires a nonzero store generation".to_owned());
        }
        let nested = match &self.operation {
            SharedServiceOperation::Health => return Ok(()),
            SharedServiceOperation::Query(query) => {
                query.validate()?;
                (&query.request_id, query.store_generation)
            }
            SharedServiceOperation::WriteDerived(request) => {
                request.validate()?;
                (&request.request_id, request.store_generation)
            }
            SharedServiceOperation::Export(request) => {
                request.validate()?;
                (&request.request_id, request.store_generation)
            }
        };
        if nested.0 != &self.request_id || nested.1 != self.store_generation {
            return Err(
                "shared request and nested operation identity or generation disagree".to_owned(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum SharedServiceResult {
    Health(Box<DiagnosticHealthSnapshot>),
    Query(Box<QueryResponse>),
    DerivedWritten(Box<DerivedArtifactWriteResponse>),
    Export(Box<ExportResponse>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SharedServiceResponse {
    pub schema_version: String,
    pub request_id: RequestId,
    pub generated_at: DateTime<Utc>,
    pub store_generation: u64,
    pub result: SharedServiceResult,
}

impl SharedServiceResponse {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        reject_transport_paths_and_bytes(json)?;
        let response: Self = parse_versioned(json)?;
        response.validate().map_err(ContractError::Validation)?;
        Ok(response)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        if self.store_generation == 0 {
            return Err("shared response requires a nonzero store generation".to_owned());
        }
        match &self.result {
            SharedServiceResult::Health(health) => {
                validate_schema_version(&health.schema_version)?;
                health.validate()?;
                if health.store_generation != self.store_generation
                    || health.observed_at != self.generated_at
                {
                    return Err(
                        "shared response and health generation or timestamp disagree".to_owned(),
                    );
                }
            }
            SharedServiceResult::Query(query) => {
                validate_schema_version(&query.schema_version)?;
                query.validate()?;
                if query.request_id != self.request_id
                    || query.store_generation != self.store_generation
                    || query.generated_at != self.generated_at
                {
                    return Err(
                        "shared response and query identity, generation, or timestamp disagree"
                            .to_owned(),
                    );
                }
            }
            SharedServiceResult::DerivedWritten(write) => {
                write.validate()?;
                if write.request_id != self.request_id
                    || write.store_generation != self.store_generation
                    || write.generated_at != self.generated_at
                {
                    return Err(
                        "shared response and derived write identity, generation, or timestamp disagree"
                            .to_owned(),
                    );
                }
            }
            SharedServiceResult::Export(export) => {
                export.validate()?;
                if export.request_id != self.request_id
                    || export.store_generation != self.store_generation
                    || export.generated_at != self.generated_at
                {
                    return Err(
                        "shared response and export identity, generation, or timestamp disagree"
                            .to_owned(),
                    );
                }
            }
        }
        Ok(())
    }
}

fn reject_transport_paths_and_bytes(json: &str) -> Result<(), ContractError> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| ContractError::InvalidJson(error.to_string()))?;
    if contains_forbidden_key(&value) {
        return Err(ContractError::Validation(
            "shared service transport cannot carry filesystem paths or image bytes".to_owned(),
        ));
    }
    Ok(())
}

fn contains_forbidden_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .keys()
                .any(|key| FORBIDDEN_TRANSPORT_KEYS.contains(&key.as_str()))
                || object.values().any(contains_forbidden_key)
        }
        Value::Array(values) => values.iter().any(contains_forbidden_key),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}
