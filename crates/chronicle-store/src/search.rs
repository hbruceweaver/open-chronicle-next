use chronicle_domain::{ActivityFilter, EventEnvelope, PageInfo, PageRequest, QueryEvent};
use rusqlite::{OptionalExtension, params};

use crate::{Result, SqliteStore, StoreError, StoreQueries};

const MAX_QUERY_CHARS: usize = 256;
const MAX_QUERY_TOKENS: usize = 16;
const MAX_PAGE_ITEMS: u32 = 100;
const MAX_HIGHLIGHT_CHARS: usize = 120;
const MAX_HIGHLIGHT_MARKS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub event: QueryEvent,
    /// Escaped, bounded evidence excerpt. This is display markup only; callers
    /// must continue to treat it as untrusted evidence.
    pub highlight_html: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub page: PageInfo,
}

#[derive(Clone, Debug)]
pub struct ActivitySearch {
    sqlite: SqliteStore,
}

impl ActivitySearch {
    pub const fn new(sqlite: SqliteStore) -> Self {
        Self { sqlite }
    }

    pub fn search(
        &self,
        filter: &ActivityFilter,
        query: &str,
        include_ocr: bool,
        page: &PageRequest,
    ) -> Result<SearchPage> {
        filter.range.validate().map_err(StoreError::InvalidPath)?;
        if page.limit == 0 || page.limit > MAX_PAGE_ITEMS {
            return Err(StoreError::InvalidPath(format!(
                "search page limit must be 1..={MAX_PAGE_ITEMS}"
            )));
        }
        let tokens = literal_tokens(query)?;
        if tokens.is_empty() {
            return Ok(SearchPage {
                hits: Vec::new(),
                page: PageInfo {
                    next_cursor: None,
                    returned_items: 0,
                    truncated: false,
                },
            });
        }
        let expression = tokens
            .iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let connection = self.sqlite.connection()?;
        let cursor_position = page
            .cursor
            .as_deref()
            .map(|cursor| {
                let projected: Option<(String, String)> = connection
                    .query_row(
                        "SELECT json_extract(body_json, '$.observed_at'), body_json
                         FROM events WHERE event_id=?1",
                        [cursor],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let (raw_observed_at, body) = projected.ok_or_else(|| {
                    StoreError::InvalidPath("search cursor event is missing".to_owned())
                })?;
                EventEnvelope::parse(&body)?;
                Ok::<_, StoreError>((raw_observed_at, cursor.to_owned()))
            })
            .transpose()?;
        let (cursor_at, cursor_id) = cursor_position
            .map(|(at, id)| (Some(at), Some(id)))
            .unwrap_or((None, None));
        let evidence_states = serde_json::to_string(&filter.evidence_states)?;
        let evidence_state_count = i64::try_from(filter.evidence_states.len())
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
        let limit = usize::try_from(page.limit)
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
        let sql_limit =
            i64::try_from(limit + 1).map_err(|error| StoreError::InvalidPath(error.to_string()))?;
        let mut statement = connection.prepare(
            "SELECT events.body_json, ocr_fts.text
             FROM ocr_fts
             JOIN events ON events.event_id=ocr_fts.event_id
             JOIN observations ON observations.event_id=events.event_id
             WHERE ocr_fts MATCH ?1
               AND json_extract(events.body_json, '$.observed_at') >= ?2
               AND json_extract(events.body_json, '$.observed_at') < ?3
               AND (?4 IS NULL
                    OR json_extract(events.body_json, '$.observed_at') > ?4
                    OR (json_extract(events.body_json, '$.observed_at') = ?4
                        AND events.event_id > ?5))
               AND (?6 IS NULL OR observations.application_bundle_id = ?6)
               AND (?7 IS NULL
                    OR instr(lower(coalesce(observations.window_title, '')), lower(?7)) > 0)
               AND (?8 IS NULL OR observations.authorized_domain = ?8)
               AND (?9 = 0
                    OR observations.evidence_state IN (SELECT value FROM json_each(?10)))
             ORDER BY json_extract(events.body_json, '$.observed_at'), events.event_id
             LIMIT ?11",
        )?;
        let rows = statement.query_map(
            params![
                expression,
                filter.range.start.to_rfc3339(),
                filter.range.end.to_rfc3339(),
                cursor_at,
                cursor_id,
                filter.application_bundle_id,
                filter.window_text,
                filter.authorized_domain,
                evidence_state_count,
                evidence_states,
                sql_limit,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let candidates = rows
            .map(|row| {
                let (body, text) = row?;
                Ok((EventEnvelope::parse(&body)?, text))
            })
            .collect::<Result<Vec<_>>>()?;
        let end = limit.min(candidates.len());
        let truncated = end < candidates.len();
        let next_cursor = if truncated {
            candidates
                .get(end.saturating_sub(1))
                .map(|(event, _)| event.event_id.to_string())
        } else {
            None
        };
        let queries = StoreQueries::new(self.sqlite.clone());
        let mut hits = Vec::with_capacity(end);
        for (event, text) in &candidates[..end] {
            let event = queries
                .event(&event.event_id, include_ocr)?
                .ok_or_else(|| StoreError::SqliteIdentity("FTS event disappeared".to_owned()))?;
            hits.push(SearchHit {
                event,
                highlight_html: include_ocr.then(|| bounded_highlight(text, &tokens)),
            });
        }
        let returned_items = u32::try_from(hits.len())
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
        Ok(SearchPage {
            hits,
            page: PageInfo {
                next_cursor,
                returned_items,
                truncated,
            },
        })
    }
}

fn literal_tokens(query: &str) -> Result<Vec<String>> {
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(StoreError::InvalidPath(format!(
            "search query exceeds {MAX_QUERY_CHARS} characters"
        )));
    }
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in query.chars() {
        if character.is_alphanumeric() {
            current.push(character);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens.dedup();
    if tokens.len() > MAX_QUERY_TOKENS {
        return Err(StoreError::InvalidPath(format!(
            "search query exceeds {MAX_QUERY_TOKENS} tokens"
        )));
    }
    Ok(tokens)
}

fn bounded_highlight(text: &str, tokens: &[String]) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let first_byte = tokens
        .iter()
        .filter_map(|token| find_token(text, token))
        .min()
        .unwrap_or_default();
    let first_character = text[..first_byte].chars().count();
    let half = MAX_HIGHLIGHT_CHARS / 2;
    let start = first_character.saturating_sub(half);
    let end = start
        .saturating_add(MAX_HIGHLIGHT_CHARS)
        .min(characters.len());
    let mut excerpt = characters[start..end].iter().collect::<String>();
    excerpt = escape_html(&excerpt);
    let mut remaining_marks = MAX_HIGHLIGHT_MARKS;
    for token in tokens {
        excerpt = mark_case_insensitive(&excerpt, &escape_html(token), &mut remaining_marks);
        if remaining_marks == 0 {
            break;
        }
    }
    if start > 0 {
        excerpt.insert(0, '…');
    }
    if end < characters.len() {
        excerpt.push('…');
    }
    excerpt
}

fn mark_case_insensitive(text: &str, token: &str, remaining_marks: &mut usize) -> String {
    if token.is_empty() {
        return text.to_owned();
    }
    let mut output = String::new();
    let mut remainder = text;
    loop {
        if *remaining_marks == 0 {
            output.push_str(remainder);
            break;
        }
        let Some(byte) = find_token(remainder, token) else {
            output.push_str(remainder);
            break;
        };
        let matched_end = byte.saturating_add(token.len()).min(remainder.len());
        output.push_str(&remainder[..byte]);
        output.push_str("<mark>");
        output.push_str(&remainder[byte..matched_end]);
        output.push_str("</mark>");
        *remaining_marks = remaining_marks.saturating_sub(1);
        remainder = &remainder[matched_end..];
    }
    output
}

fn find_token(text: &str, token: &str) -> Option<usize> {
    if token.is_ascii() {
        text.as_bytes()
            .windows(token.len())
            .position(|window| window.eq_ignore_ascii_case(token.as_bytes()))
    } else {
        text.find(token)
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
