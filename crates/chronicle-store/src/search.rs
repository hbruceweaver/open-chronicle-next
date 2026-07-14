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
    /// Plain untrusted text plus character-offset highlight ranges. App UI
    /// must render this as inert text, never markup.
    pub snippet: Option<SearchSnippet>,
    /// Escaped, bounded evidence excerpt. This is display markup only; callers
    /// must continue to treat it as untrusted evidence.
    pub highlight_html: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchSnippet {
    pub text: String,
    pub highlights: Vec<SearchHighlightRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchHighlightRange {
    pub start: u32,
    pub length: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub page: PageInfo,
}

#[derive(Clone, Debug)]
pub struct ActivitySearch {
    queries: StoreQueries,
}

impl ActivitySearch {
    pub const fn new(sqlite: SqliteStore) -> Self {
        Self {
            queries: StoreQueries::new(sqlite),
        }
    }

    pub const fn from_queries(queries: StoreQueries) -> Self {
        Self { queries }
    }

    pub fn search(
        &self,
        filter: &ActivityFilter,
        query: &str,
        include_ocr: bool,
        page: &PageRequest,
    ) -> Result<SearchPage> {
        self.search_inner(filter, query, include_ocr, page, None, None)
    }

    /// Runs the same literal FTS query while excluding observations recorded
    /// after one stable app snapshot cutoff.
    pub fn search_at_cutoff(
        &self,
        filter: &ActivityFilter,
        query: &str,
        include_ocr: bool,
        page: &PageRequest,
        stable_cutoff: chrono::DateTime<chrono::Utc>,
        event_rowid_high_water: u64,
    ) -> Result<SearchPage> {
        self.search_inner(
            filter,
            query,
            include_ocr,
            page,
            Some(stable_cutoff),
            Some(event_rowid_high_water),
        )
    }

    fn search_inner(
        &self,
        filter: &ActivityFilter,
        query: &str,
        include_ocr: bool,
        page: &PageRequest,
        stable_cutoff: Option<chrono::DateTime<chrono::Utc>>,
        event_rowid_high_water: Option<u64>,
    ) -> Result<SearchPage> {
        filter.range.validate().map_err(StoreError::InvalidPath)?;
        if page.limit == 0 || page.limit > MAX_PAGE_ITEMS {
            return Err(StoreError::InvalidPath(format!(
                "search page limit must be 1..={MAX_PAGE_ITEMS}"
            )));
        }
        let tokens = literal_tokens(query)?;
        if tokens.is_empty() {
            if page.cursor.is_some() {
                return Err(StoreError::CursorScopeMismatch);
            }
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
        self.queries.with_connection(|connection| {
            let stable_cutoff = stable_cutoff.map(|value| value.to_rfc3339());
            let event_rowid_high_water = event_rowid_high_water
                .map(i64::try_from)
                .transpose()
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let cursor_position = page
                .cursor
                .as_deref()
                .map(|cursor| {
                    let projected: Option<(String, String)> = connection
                        .query_row(
                            "SELECT json_extract(events.body_json, '$.observed_at'),
                                    events.body_json
                             FROM ocr_fts
                             JOIN events ON events.event_id=ocr_fts.event_id
                             JOIN observations ON observations.event_id=events.event_id
                             WHERE events.event_id=?1 AND ocr_fts MATCH ?2
                               AND json_extract(events.body_json, '$.observed_at') >= ?3
                               AND json_extract(events.body_json, '$.observed_at') < ?4
                               AND (?5 IS NULL OR observations.application_bundle_id=?5)
                               AND (?6 IS NULL OR instr(
                                    lower(coalesce(observations.window_title, '')),
                                    lower(?6)) > 0)
                               AND (?7 IS NULL OR observations.authorized_domain=?7)
                               AND (?8=0 OR observations.evidence_state IN (
                                    SELECT value FROM json_each(?9)))
                               AND (?10 IS NULL OR json_extract(
                                    events.body_json, '$.recorded_at') <= ?10)
                               AND (?11 IS NULL OR events.rowid <= ?11)",
                            params![
                                cursor,
                                &expression,
                                filter.range.start.to_rfc3339(),
                                filter.range.end.to_rfc3339(),
                                filter.application_bundle_id.as_deref(),
                                filter.window_text.as_deref(),
                                filter.authorized_domain.as_deref(),
                                i64::try_from(filter.evidence_states.len()).map_err(|error| {
                                    StoreError::InvalidPath(error.to_string())
                                })?,
                                serde_json::to_string(&filter.evidence_states)?,
                                stable_cutoff,
                                event_rowid_high_water,
                            ],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?;
                    let (raw_observed_at, body) =
                        projected.ok_or_else(|| StoreError::CursorScopeMismatch)?;
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
            let sql_limit = i64::try_from(limit + 1)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
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
               AND (?11 IS NULL
                    OR json_extract(events.body_json, '$.recorded_at') <= ?11)
               AND (?12 IS NULL OR events.rowid <= ?12)
             ORDER BY json_extract(events.body_json, '$.observed_at'), events.event_id
             LIMIT ?13",
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
                    stable_cutoff,
                    event_rowid_high_water,
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
            let mut hits = Vec::with_capacity(end);
            for (event, text) in &candidates[..end] {
                let event = self
                    .queries
                    .query_event(connection, event.clone(), include_ocr)?;
                hits.push(SearchHit {
                    event,
                    snippet: include_ocr.then(|| bounded_snippet(text, &tokens)),
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
        })
    }
}

fn bounded_snippet(text: &str, tokens: &[String]) -> SearchSnippet {
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
    let excerpt = characters[start..end].iter().collect::<String>();
    let prefix = usize::from(start > 0);
    let mut highlights = Vec::new();
    for token in tokens {
        let mut searched_bytes = 0;
        while highlights.len() < MAX_HIGHLIGHT_MARKS && searched_bytes < excerpt.len() {
            let remainder = &excerpt[searched_bytes..];
            let Some(relative_byte) = find_token(remainder, token) else {
                break;
            };
            let byte = searched_bytes + relative_byte;
            let matched_end = byte.saturating_add(token.len()).min(excerpt.len());
            let character_start = prefix + excerpt[..byte].chars().count();
            let character_length = excerpt[byte..matched_end].chars().count();
            if character_length > 0 {
                highlights.push(SearchHighlightRange {
                    start: u32::try_from(character_start).unwrap_or(u32::MAX),
                    length: u32::try_from(character_length).unwrap_or(u32::MAX),
                });
            }
            searched_bytes = matched_end;
        }
        if highlights.len() == MAX_HIGHLIGHT_MARKS {
            break;
        }
    }
    highlights.sort_by_key(|highlight| (highlight.start, highlight.length));
    highlights.dedup();
    let mut snippet = excerpt;
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < characters.len() {
        snippet.push('…');
    }
    SearchSnippet {
        text: snippet,
        highlights,
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
