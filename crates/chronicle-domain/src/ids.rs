use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::path::{Component, Path};
use std::str::FromStr;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                validate_opaque_id(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("identifier must be non-empty and contain only opaque identifier characters")]
    InvalidIdentifier,
    #[error("managed path must be relative and contain only normal components")]
    InvalidManagedPath,
}

fn validate_opaque_id(value: &str) -> Result<(), IdError> {
    let valid = !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        && !value.contains("..")
        && !value.contains('/')
        && !value.contains('\\');
    valid.then_some(()).ok_or(IdError::InvalidIdentifier)
}

string_id!(EventId);
string_id!(ChunkId);
string_id!(ChunkRevisionId);
string_id!(ArtifactId);
string_id!(ArtifactRevisionId);
string_id!(ImageArtifactId);
string_id!(DeviceId);
string_id!(ClientId);
string_id!(GrantId);
string_id!(ReceiptId);
string_id!(RequestId);

/// Storage-boundary type used only by local canonical image references. Query/MCP
/// image metadata deliberately exposes only `ImageArtifactId` and lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ManagedRelativePath(String);

impl ManagedRelativePath {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        let path = Path::new(&value);
        let valid = !value.is_empty()
            && !value.contains('\\')
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
            })
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        valid
            .then_some(Self(value))
            .ok_or(IdError::InvalidManagedPath)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ManagedRelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl FromStr for ManagedRelativePath {
    type Err = IdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}
