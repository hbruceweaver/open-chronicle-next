# Query and disclosure contract v1

`contracts/query-v1.schema.json` describes typed local queries, responses, and
per-client disclosure grants. The Swift UI and bundled MCP server must adapt the same
Rust query service rather than implement independent SQL.

## Grants

No client receives evidence merely because it is installed. A grant binds an opaque
client/receipt identity to an absolute UTC range or rolling horizon, explicit
metadata/OCR/derived content classes, expiry, page/response/cumulative byte limits,
revocation state, and store generation. Authorization helpers require an explicit
evaluation time and fail closed before creation, at expiry, and for revoked/expired
state. OCR is not permitted unless both the content class and capability are present.
Pagination and context-packet sizing remain inside these limits.

## Requests and responses

Requests are typed operations: status/schemas, chunk list/read, activity search,
moment inspection, factual statistics/comparison, supporting evidence, bounded
context packet, derived listing, and direct `get-event` / `get-artifact` evidence
drill-down. Inputs accept typed filters/IDs/ranges, never SQL or filesystem paths.
Schema discovery names the published event, chunk, derived-artifact, and query v1
contracts. A later MCP resource layer will publish the separately typed shared
write/export transport contract rather than claiming a JSON schema before it exists.

Context packets carry a manifest with included and available event/chunk counts,
included/excluded content classes, pinned projection journal cutoffs, a SHA-256
content checksum, and truncation. They
contain only fully range-contained chunks and individually contained supporting
events; screenshots and derived artifacts are explicitly excluded.

Responses use a tagged `QueryResult` with detailed, path-safe event, complete chunk,
and artifact DTOs rather than ID-only placeholders. Every response identifies the
request operation, stable cutoff, store generation, complete grant state/time scope,
effective disclosure scope, pagination/truncation, factual coverage (including
presence), and engine/schema/projection/SQLite/source-record provenance. Direct and
paged results validate their requested IDs/ranges/limits against an explicit paired
request/response exchange. Result-aware validation also requires every returned
event timestamp, chunk window, coverage range, and artifact creation time to remain
inside the effective disclosure ranges. Returned content classes must be declared by
the scope, and paged `returned_items` must equal the actual top-level list length
while remaining within both the request and grant limits.

OCR retains the untrusted marker and factual engine/request-language provenance and
cannot appear outside an OCR-enabled scope. Recognition languages are the configured
or requested values, not a claim that Vision detected those languages.
Image metadata contains only opaque artifact ID, lifecycle state, and expiry. Query
parsing recursively rejects managed/file path and image-byte keys, including inside
derived payloads. The query schema closes every result DTO against those extensions.

Unknown major versions fail before typed deserialization. Same-major ordinary
envelope extensions are ignored by v1 readers. Draft 2020-12 meta-validation, every
synthetic fixture, and mutated-contract differential tests verify schema/Rust
acceptance parity. Operation, result, capability, state, and content-class tokens are
lowercase kebab-case.
