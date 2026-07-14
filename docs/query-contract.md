# Shared query contract

Open Chronicle exposes one additive, versioned Rust request/response contract to
the future Swift bridge and bundled MCP server. U5b adds bounded `write-derived`
and `export` operations to U5a's `health` and `query` operations. It has no
operation that pauses capture, changes
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

The U5 schema-discovery result enumerates every published JSON contract available
at this layer: event v1, chunk v1, derived-artifact v1, and query v1. The outer
shared-service/write/export envelope is already a typed and validated Rust contract,
but U5b does not advertise a JSON schema that does not yet exist. U12 owns publishing
that transport contract as an MCP schema resource alongside these four contracts.

The shared service implements status, schema discovery, chunk listing/detail, event detail,
OCR search, moment inspection, statistics, period comparison, and supporting
evidence, derived-artifact reads/writes, bounded context packets, and stable-cutoff
JSON/Markdown export.

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
- Export work: at most 100,000 events, 105,408 chunk revisions, and 10,000
  artifact revisions in one pinned snapshot, in addition to the byte bound.

An oversized response fails atomically. It does not advance cumulative usage or
persist a staged cursor. Byte accounting covers the complete shared response,
including its provenance and grant summary.
Context packets and exports deterministically shrink a stable selection prefix,
recomputing manifest counts, checksums, truncation, and provenance IDs until the
complete serialized response fits. They fail uncharged only when the empty bounded
envelope itself cannot fit.

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
Context packets and exports include only chunks fully contained by the effective
range and only events whose observed/scheduled instants and recording-gap interval
are contained by that range. Derived results are returned only when their creation
bucket and every cited event/chunk range are authorized. Missing and unauthorized
artifact references produce the same non-disclosing failure.

Agent-authored derived writes require metadata plus derived authorization, exact
client attribution, the current store generation, a server-owned creation stamp,
and existing evidence wholly inside the grant. New artifacts start as draft.
Drafts may remain draft or become accepted/rejected/superseded; accepted and
rejected revisions may retain that state or become superseded; superseded is
terminal. Prior links and artifact type are immutable. A wall-clock regression
behind the current revision is rejected for a new shared write. The service ignores
a client's proposed creation time for a new write and returns the canonical server
time. An exact request retry is normalized to the already-stored canonical time
before its full immutable comparison.
Before appending a child, authorization covers both the projected current revision
and any newer canonical chain tip. A projection failure after canonical rename
therefore cannot hide an out-of-scope parent from a later client.

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
Derived-artifact pages use the immutable artifact ID as their raw keyset anchor, so
revising the anchor between pages cannot skip or duplicate another artifact.

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

Context packets additionally contain included/available fact counts, explicit
included/excluded content classes, the pinned projection journal cutoffs, a content
checksum, and truncation. Screenshots
and derived artifacts are always excluded from context packets. Full export has a
manifest with pinned projection journal cutoffs, coverage/gaps, counts, checksums,
and truncation; screenshots are excluded in every format.

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
grants, cumulative usage, active cursor bindings, and committed derived-write
request IDs. A derived write's request receipt and byte charge commit atomically
after its immutable artifact and projection. If the artifact rename/projection or
receipt boundary fails, an exact request retry compares the full canonical revision,
repairs projection if needed, and charges at most once; a mutated retry fails.
The receipt document holds at most 4,096 derived-write receipts; new writes fail
before their canonical artifact is created at that bound. Revocation removes the
grant's cursor and derived-write receipts, and beginning a query prunes receipts
from obsolete store generations. Writes use same-directory
temporary-file sync, atomic rename, and parent-directory sync under a dedicated
grant-receipt lock. SQLite remains a disposable evidence projection, so the service does
not put disclosure receipts in SQLite. Additive projection migration `0003`
contains only rebuildable, indexed health-operation facts; it backfills v2 once
and avoids whole-history health scans. Migrations `0001` and `0002` are unchanged.

Receipt mutation lock order is always shared store lock, current generation read,
then exclusive grant-receipt lock. A reset takes the exclusive store lock. An old
process waiting behind reset therefore observes the new generation and cannot
install, revoke, charge, or issue a cursor under the deleted generation.
