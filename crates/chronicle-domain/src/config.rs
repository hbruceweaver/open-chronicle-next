use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ClientId, ContractError, GrantId, ReceiptId, parse_versioned, validate_schema_version,
};

pub const MAX_DISCLOSURE_PAGE_ITEMS: u32 = 100;
pub const MAX_DISCLOSURE_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_DISCLOSURE_CUMULATIVE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureCadence {
    ThirtySeconds,
    SixtySeconds,
}

impl CaptureCadence {
    pub const fn seconds(self) -> u32 {
        match self {
            Self::ThirtySeconds => 30,
            Self::SixtySeconds => 60,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotRetention {
    OneHour,
    TwentyFourHours,
    SevenDays,
    ThirtyDays,
}

impl ScreenshotRetention {
    pub const fn seconds(self) -> u32 {
        match self {
            Self::OneHour => 60 * 60,
            Self::TwentyFourHours => 24 * 60 * 60,
            Self::SevenDays => 7 * 24 * 60 * 60,
            Self::ThirtyDays => 30 * 24 * 60 * 60,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtcRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl UtcRange {
    pub fn validate(&self) -> Result<(), String> {
        (self.start < self.end)
            .then_some(())
            .ok_or_else(|| "time range start must precede end".to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum GrantTimeScope {
    Absolute { range: UtcRange },
    RollingHorizon { seconds: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentClass {
    Metadata,
    Ocr,
    Derived,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureLimits {
    pub max_page_items: u32,
    pub max_response_bytes: u64,
    pub max_cumulative_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantState {
    Active,
    Revoked,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureGrant {
    pub schema_version: String,
    pub grant_id: GrantId,
    pub client_id: ClientId,
    pub receipt_id: ReceiptId,
    pub time_scope: GrantTimeScope,
    pub content_classes: Vec<ContentClass>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub state: GrantState,
    pub limits: DisclosureLimits,
    pub disclosed_bytes: u64,
    pub store_generation: u64,
}

impl DisclosureGrant {
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        let grant: Self = parse_versioned(json)?;
        grant.validate().map_err(ContractError::Validation)?;
        Ok(grant)
    }

    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        self.state == GrantState::Active && self.created_at <= now && now < self.expires_at
    }

    pub fn allows_content_at(&self, class: ContentClass, now: DateTime<Utc>) -> bool {
        self.is_active_at(now) && self.content_classes.contains(&class)
    }

    pub fn allows_full_ocr_at(&self, now: DateTime<Utc>) -> bool {
        self.allows_content_at(ContentClass::Ocr, now)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_schema_version(&self.schema_version)?;
        if self.created_at >= self.expires_at {
            return Err("grant expiry must follow creation".to_owned());
        }
        if self.store_generation == 0 {
            return Err("grant requires a nonzero store generation".to_owned());
        }
        if self.content_classes.is_empty() {
            return Err("grant must explicitly name at least one content class".to_owned());
        }
        for (index, class) in self.content_classes.iter().enumerate() {
            if self.content_classes[index + 1..].contains(class) {
                return Err("grant content classes must be unique".to_owned());
            }
        }
        if self.limits.max_page_items == 0
            || self.limits.max_page_items > MAX_DISCLOSURE_PAGE_ITEMS
            || self.limits.max_response_bytes == 0
            || self.limits.max_response_bytes > MAX_DISCLOSURE_RESPONSE_BYTES
            || self.limits.max_cumulative_bytes < self.limits.max_response_bytes
            || self.limits.max_cumulative_bytes > MAX_DISCLOSURE_CUMULATIVE_BYTES
            || self.disclosed_bytes > self.limits.max_cumulative_bytes
        {
            return Err("grant disclosure limits are inconsistent".to_owned());
        }
        match &self.time_scope {
            GrantTimeScope::Absolute { range } => range.validate(),
            GrantTimeScope::RollingHorizon { seconds } if *seconds > 0 => Ok(()),
            GrantTimeScope::RollingHorizon { .. } => {
                Err("rolling horizon must be greater than zero".to_owned())
            }
        }
    }
}
