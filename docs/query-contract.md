# Shared query contract

Open Chronicle exposes one additive, versioned Rust request/response contract to
the future Swift bridge and bundled MCP server. U5a deliberately contains only
`health` and `query` operations. It has no operation that pauses capture, changes
privacy settings, deletes evidence, or mutates canonical events or chunks.

## Protocol boundary

- Shared request and response schema version: `1.x`.
- Maximum serialized shared request: 64 KiB.
- Every request carries an opaque request ID and nonzero store generation.
- Nested query identity and generation must equal the outer request.
- A stale generation fails before any evidence or receipt mutation.
- Health is content-free and does not require an evidence disclosure grant.
- Every agent query, including status and schema discovery, requires an active
  per-client grant.

U5a implements status, schema discovery, chunk listing/detail, event detail,
OCR search, moment inspection, statistics, period comparison, and supporting
evidence. Derived-artifact reads/writes and stable-cutoff context export are U5b.

## Bounds

The service applies the narrowest of its fixed ceiling, the request, and the
grant:

- Effective UTC range: at most 31 days per requested range.
- Factual query ranges: inward-aligned to complete UTC five-minute buckets. A
  request with no complete authorized bucket fails without disclosure.
- Page size: at most 100 items and never above the grant or requested page size.
- Chunk-list filters, keyset cursor, and page-plus-one limit are applied in SQL
  before canonical JSON is materialized.
- Moment inspection is limited in SQL to 1,000 returned events and fails rather
  than materializing a larger response.
- Response size: at most 4 MiB and never above the grant.
- Cumulative disclosure: at most 1 GiB and never above the grant.
- Active cursor receipts: at most 512, expiring after one hour or at grant expiry,
  whichever comes first.

An oversized response fails atomically. It does not advance cumulative usage or
persist a staged cursor. Byte accounting covers the complete shared response,
including its provenance and grant summary.

## Disclosure policy

No grant means no evidence. A grant binds:

- client ID and grant/receipt IDs;
- absolute UTC range or rolling horizon;
- metadata, OCR, and/or derived content classes;
- expiry and immediate revocation state;
- page, response-byte, and cumulative-byte limits;
- store generation.

Metadata is required for factual queries. OCR search always requires the OCR
content class, even when `include_ocr=false`, because matching event IDs reveal
facts derived from OCR. The flag controls whether OCR text/snippets are returned;
it does not turn OCR search into metadata search. Event, chunk, moment, and
supporting-evidence detail include OCR only when the grant contains OCR; otherwise
those payloads are structurally stripped of OCR.

Requested ranges are recorded in the response and intersected with the current
grant horizon. Direct ID/moment/supporting-evidence reads fail when the complete
source bucket is not authorized; they are never partially disclosed.

## Cursor containment

Callers receive an opaque random cursor token, never the projection's raw row
position. Its durable receipt binds the token to:

- client and grant;
- store generation;
- operation kind;
- normalized filters/search and effective ranges;
- content-class scope;
- raw local projection position and expiry.

Changing any bound field rejects the cursor before querying. Revocation removes
that grant's cursors; expiry and generation changes also make them unusable.

## Response shape

Every successful query response contains:

- schema/request/operation/store-generation identity;
- generated time and stable projection cutoff;
- active grant summary and remaining cumulative budget;
- requested/effective ranges and disclosed content classes;
- exact page/truncation metadata where applicable;
- query-engine, contract, projection, and pinned SQLite provenance;
- source event/chunk-revision IDs;
- factual coverage for single-range evidence operations.

The service uses the same `StoreQueries`, `ActivitySearch`, and
`FactualStatistics` implementations intended for the Swift UI, avoiding a second
MCP-specific SQL or aggregation path.

## Snapshot consistency

Each shared query pins one SQLite WAL read transaction before resolving direct
IDs. Result rows, coverage, filters, and provenance use that same read snapshot.
Projection writers therefore continue capturing without waiting for an
agent-side range scan, while the reported stable cutoff still describes one
projection state. Health alone takes the short query-snapshot lock needed to
compare projection cursors with canonical journal tails; it reads only per-shard
sizes and unprojected tail bytes. Recovery/reset takes the stronger exclusive
store lock.

## Receipt durability and reset ordering

`receipts/disclosure-grants.json` is the authoritative operational receipt for
grants, cumulative usage, and active cursor bindings. Writes use same-directory
temporary-file sync, atomic rename, and parent-directory sync under a dedicated
grant-receipt lock. SQLite remains a disposable evidence projection, so U5a does
not put disclosure receipts in SQLite. Additive projection migration `0003`
contains only rebuildable, indexed health-operation facts; it backfills v2 once
and avoids whole-history health scans. Migrations `0001` and `0002` are unchanged.

Receipt mutation lock order is always shared store lock, current generation read,
then exclusive grant-receipt lock. A reset takes the exclusive store lock. An old
process waiting behind reset therefore observes the new generation and cannot
install, revoke, charge, or issue a cursor under the deleted generation.
