//! Versioned factual evidence contracts shared by every Open Chronicle surface.
//!
//! These types contain factual evidence and immutable derived-artifact revisions.
//! Storage, capture, projection, and transport code depend on this crate, never the
//! other way around.

pub mod artifact;
pub mod chunk;
pub mod config;
pub mod event;
pub mod health;
pub mod ids;
pub mod query;

use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

pub use artifact::*;
pub use chunk::*;
pub use config::*;
pub use event::*;
pub use health::*;
pub use ids::*;
pub use query::*;

pub const CONTRACT_MAJOR_VERSION: u16 = 1;
pub const CONTRACT_VERSION: &str = "1.0";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("contract is missing schema_version")]
    MissingSchemaVersion,
    #[error("invalid schema version: {0}")]
    InvalidSchemaVersion(String),
    #[error("unsupported contract major version {actual}; expected {expected}")]
    UnsupportedMajorVersion { expected: u16, actual: u16 },
    #[error("contract validation failed: {0}")]
    Validation(String),
}

/// Parses a versioned JSON contract after rejecting unknown major versions with a
/// domain error. Unknown fields within the same major version are ignored by the
/// typed API unless a privacy-sensitive payload explicitly denies them.
pub fn parse_versioned<T: DeserializeOwned>(json: &str) -> Result<T, ContractError> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| ContractError::InvalidJson(error.to_string()))?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_str)
        .ok_or(ContractError::MissingSchemaVersion)?;
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .ok_or_else(|| ContractError::InvalidSchemaVersion(version.to_owned()))?;
    let minor_is_valid = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .is_some()
        && parts.next().is_none();
    if !minor_is_valid {
        return Err(ContractError::InvalidSchemaVersion(version.to_owned()));
    }
    if major != CONTRACT_MAJOR_VERSION {
        return Err(ContractError::UnsupportedMajorVersion {
            expected: CONTRACT_MAJOR_VERSION,
            actual: major,
        });
    }
    serde_json::from_value(value).map_err(|error| ContractError::InvalidJson(error.to_string()))
}
