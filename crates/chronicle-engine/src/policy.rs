use chronicle_domain::{
    ContentClass, DisclosureGrant, GrantTimeScope, QueryOperation, QueryOperationKind, UtcRange,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};

pub const MAX_QUERY_RANGE_SECONDS: i64 = 31 * 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("query content class is not granted: {0:?}")]
    ContentDenied(ContentClass),
    #[error("query range has no grant-authorized five-minute coverage")]
    RangeDenied,
    #[error("query effective range exceeds the bounded service range")]
    RangeLimit,
    #[error("query range could not be represented safely")]
    InvalidRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyDecision {
    pub requested_ranges: Vec<UtcRange>,
    pub effective_ranges: Vec<UtcRange>,
    pub content_classes: Vec<ContentClass>,
    pub include_ocr: bool,
    pub page_limit: Option<u32>,
    pub cursor_scope_digest: String,
}

pub fn authorize_query(
    grant: &DisclosureGrant,
    operation: &QueryOperation,
    requested_ranges: Vec<UtcRange>,
    now: DateTime<Utc>,
) -> Result<PolicyDecision, PolicyError> {
    let (search_uses_ocr, include_ocr) = authorize_query_content(grant, operation)?;
    let allowed = allowed_range(grant, now)?;
    let mut effective_ranges = Vec::with_capacity(requested_ranges.len());
    for requested in &requested_ranges {
        requested
            .validate()
            .map_err(|_| PolicyError::InvalidRange)?;
        let start = requested.start.max(allowed.start);
        let end = requested.end.min(allowed.end);
        let Some(aligned) = align_inward(start, end)? else {
            return Err(PolicyError::RangeDenied);
        };
        if (aligned.end - aligned.start) > Duration::seconds(MAX_QUERY_RANGE_SECONDS) {
            return Err(PolicyError::RangeLimit);
        }
        effective_ranges.push(aligned);
    }
    let page_limit =
        requested_page_limit(operation).map(|limit| limit.min(grant.limits.max_page_items));
    let mut content_classes = vec![ContentClass::Metadata];
    if search_uses_ocr || include_ocr {
        content_classes.push(ContentClass::Ocr);
    }
    let cursor_scope_digest = scope_digest(operation, &effective_ranges, &content_classes)?;
    Ok(PolicyDecision {
        requested_ranges,
        effective_ranges,
        content_classes,
        include_ocr,
        page_limit,
        cursor_scope_digest,
    })
}

pub fn authorize_query_content(
    grant: &DisclosureGrant,
    operation: &QueryOperation,
) -> Result<(bool, bool), PolicyError> {
    if !grant.content_classes.contains(&ContentClass::Metadata) {
        return Err(PolicyError::ContentDenied(ContentClass::Metadata));
    }
    // Even without snippets, OCR FTS discloses which event IDs matched OCR.
    // Therefore every search consumes OCR capability. Evidence-detail reads
    // include OCR when the user explicitly granted that content class.
    let search_uses_ocr = matches!(operation, QueryOperation::SearchActivity { .. });
    let include_ocr = match operation {
        QueryOperation::SearchActivity { include_ocr, .. } => *include_ocr,
        QueryOperation::ReadChunk { .. }
        | QueryOperation::GetEvent { .. }
        | QueryOperation::InspectMoment { .. }
        | QueryOperation::SupportingEvidence { .. } => {
            grant.content_classes.contains(&ContentClass::Ocr)
        }
        _ => false,
    };
    if (search_uses_ocr || include_ocr) && !grant.content_classes.contains(&ContentClass::Ocr) {
        return Err(PolicyError::ContentDenied(ContentClass::Ocr));
    }
    Ok((search_uses_ocr, include_ocr))
}

fn allowed_range(grant: &DisclosureGrant, now: DateTime<Utc>) -> Result<UtcRange, PolicyError> {
    match &grant.time_scope {
        GrantTimeScope::Absolute { range } => Ok(range.clone()),
        GrantTimeScope::RollingHorizon { seconds } => {
            let seconds = i64::try_from(*seconds).map_err(|_| PolicyError::InvalidRange)?;
            let start = now
                .checked_sub_signed(Duration::seconds(seconds))
                .ok_or(PolicyError::InvalidRange)?;
            Ok(UtcRange { start, end: now })
        }
    }
}

fn align_inward(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Option<UtcRange>, PolicyError> {
    let start_epoch = start.timestamp();
    let start_remainder = start_epoch.rem_euclid(300);
    let aligned_start_epoch = if start_remainder == 0 {
        start_epoch
    } else {
        start_epoch
            .checked_add(300 - start_remainder)
            .ok_or(PolicyError::InvalidRange)?
    };
    let aligned_end_epoch = end.timestamp() - end.timestamp().rem_euclid(300);
    if aligned_start_epoch >= aligned_end_epoch {
        return Ok(None);
    }
    let aligned_start =
        DateTime::from_timestamp(aligned_start_epoch, 0).ok_or(PolicyError::InvalidRange)?;
    let aligned_end =
        DateTime::from_timestamp(aligned_end_epoch, 0).ok_or(PolicyError::InvalidRange)?;
    Ok(Some(UtcRange {
        start: aligned_start,
        end: aligned_end,
    }))
}

fn requested_page_limit(operation: &QueryOperation) -> Option<u32> {
    match operation {
        QueryOperation::ListChunks { page, .. }
        | QueryOperation::SearchActivity { page, .. }
        | QueryOperation::SupportingEvidence { page, .. }
        | QueryOperation::ListDerived { page, .. } => Some(page.limit),
        _ => None,
    }
}

fn scope_digest(
    operation: &QueryOperation,
    effective_ranges: &[UtcRange],
    content_classes: &[ContentClass],
) -> Result<String, PolicyError> {
    let normalized = match operation {
        QueryOperation::ListChunks { filter, .. } => json!({"filter": filter}),
        QueryOperation::SearchActivity {
            filter,
            query,
            include_ocr,
            ..
        } => json!({"filter": filter, "query": query, "include_ocr": include_ocr}),
        QueryOperation::SupportingEvidence { chunk_id, .. } => json!({"chunk_id": chunk_id}),
        QueryOperation::ListDerived { range, .. } => json!({"range": range}),
        other => serde_json::to_value(other).map_err(|_| PolicyError::InvalidRange)?,
    };
    let bytes = serde_json::to_vec(&json!({
        "operation": operation.kind(),
        "normalized": normalized,
        "effective_ranges": effective_ranges,
        "content_classes": content_classes,
    }))
    .map_err(|_| PolicyError::InvalidRange)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn operation_requires_full_range(operation: QueryOperationKind) -> bool {
    matches!(
        operation,
        QueryOperationKind::ReadChunk
            | QueryOperationKind::GetEvent
            | QueryOperationKind::InspectMoment
            | QueryOperationKind::SupportingEvidence
    )
}
