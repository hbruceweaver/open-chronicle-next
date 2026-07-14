use std::borrow::Cow;
use std::marker::PhantomData;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

use crate::logging::McpServerError;

pub const DEFAULT_PAGE_ITEMS: u32 = 50;
pub const MAX_PAGE_ITEMS: u32 = 100;
pub const DEFAULT_CONTEXT_BYTES: u64 = 256 * 1024;

/// Keeps rmcp's generated input schema while deferring attacker-controlled JSON
/// decoding to Chronicle's content-free error boundary.
pub struct SafeInput<T> {
    value: SafeInputValue<T>,
}

enum SafeInputValue<T> {
    Raw(serde_json::Value, PhantomData<T>),
    Trusted(T),
}

impl<'de, T> Deserialize<'de> for SafeInput<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self {
            value: SafeInputValue::Raw(serde_json::Value::deserialize(deserializer)?, PhantomData),
        })
    }
}

impl<T> SafeInput<T>
where
    T: DeserializeOwned,
{
    pub fn parse(self) -> Result<T, McpServerError> {
        match self.value {
            SafeInputValue::Raw(raw, _) => serde_json::from_value(raw).map_err(|_| {
                McpServerError::InvalidInput("request did not match tool schema".to_owned())
            }),
            SafeInputValue::Trusted(value) => Ok(value),
        }
    }

    /// Constructs input for trusted in-process adapters. MCP wire input always
    /// enters through `Deserialize` and the content-free raw decoding boundary.
    #[doc(hidden)]
    pub fn trusted(value: T) -> Self {
        Self {
            value: SafeInputValue::Trusted(value),
        }
    }
}

impl<T> JsonSchema for SafeInput<T>
where
    T: JsonSchema,
{
    fn inline_schema() -> bool {
        T::inline_schema()
    }

    fn schema_name() -> Cow<'static, str> {
        T::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        T::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }
}
