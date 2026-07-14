# Open Chronicle MCP contract

Open Chronicle bundles a local stdio MCP server so Claude, Codex, and other
compatible clients can inspect the same factual evidence as the app. It is an
adapter over the authoritative Rust query/write service, not a second query
engine.

The server follows the official
[Model Context Protocol Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
and uses `rmcp` 2.2.0. The published contract in this document is version 1.

## Registration boundary

Launch the bundled `chronicle-mcp` executable with all three registration-bound
arguments:

```text
chronicle-mcp \
  --managed-root /absolute/path/to/the/Chronicle/root \
  --client-id <receipt-bound-client-id> \
  --grant-id <active-disclosure-grant-id>
```

The managed root must already be an absolute directory. Client and grant identity
come only from registration; tool arguments cannot replace or widen them. Missing,
duplicate, unknown, relative, or malformed arguments fail before the protocol starts.
The app creates and revokes disclosure grants; the MCP server never creates its own.

Every request opens a fresh engine-owned service handle. The engine checks store
generation, grant/client binding, active/expiry state, time scope, content classes,
per-response bytes, cumulative disclosed bytes, and cursor scope. Grant revocation
takes effect on the next request. MCP never opens SQLite or canonical journals
directly.

## Evidence and analysis remain separate

Events and five-minute chunks are factual records. OCR and window text are explicitly
untrusted evidence and never instructions. Derived artifacts are immutable,
evidence-linked analysis revisions; creating one cannot mutate an event or chunk.

The MCP surface deliberately omits capture control, pause/resume, study control,
privacy configuration, retention, deletion, factory reset, raw SQL, arbitrary files,
network calls, model calls, screenshot bytes, and filesystem paths.

## Resources

| URI | MIME type | Purpose |
| --- | --- | --- |
| `chronicle://status/v1` | `application/json` | Grant-bounded historical evidence availability and projection freshness |
| `chronicle://schemas/event/v1` | `application/schema+json` | Factual event contract |
| `chronicle://schemas/chunk/v1` | `application/schema+json` | Five-minute chunk contract |
| `chronicle://schemas/derived-artifact/v1` | `application/schema+json` | Separate analysis contract |
| `chronicle://schemas/query/v1` | `application/schema+json` | Query envelope and result contract |
| `chronicle://schemas/shared-service/v1` | `application/schema+json` | Shared health/query/write/export transport and safe MCP error contract |

Reading any resource requires the registered active grant. Schema reads perform a
metadata-granted schema query first and return executable Draft 2020-12 JSON Schema;
the shared-service schema resolves its sibling references against the other schema
resources. Status returns the same inner `QueryResponse` shape as
`chronicle_status`.

## Factual read tools

| Tool | Purpose |
| --- | --- |
| `chronicle_status` | Historical evidence availability and projection freshness; not current capture lifecycle |
| `chronicle_get_current_context` | Context for the last fully completed five-minute UTC bucket |
| `chronicle_list_chunks` | Paged factual chunk summaries for a UTC range/filter |
| `chronicle_get_chunk` | One complete chunk and opaque image lifecycle metadata |
| `chronicle_get_event` | One factual event; OCR appears only when granted |
| `chronicle_search` | Bounded OCR-index search; OCR permission is always required, while `include_ocr` controls returned OCR text |
| `chronicle_inspect_moment` | Evidence bucket containing one UTC instant |
| `chronicle_statistics` | Factual durations, coverage, gaps, apps, and transitions |
| `chronicle_compare_periods` | Factual comparison of two authorized UTC ranges |
| `chronicle_supporting_evidence` | Events that support a chunk |
| `chronicle_context_packet` | Size-bounded context with coverage, gaps, IDs, and provenance |
| `chronicle_list_artifacts` | Paged derived revisions for an authorized range |
| `chronicle_get_artifact` | One derived revision and its evidence references |

All input objects reject unknown fields. Page size must be 1 through 100 and remains
subject to the grant's lower limit. Explicit time ranges use inclusive-start,
exclusive-end RFC 3339 UTC timestamps. The current-context convenience tool never
presents an in-progress bucket as settled evidence.

Status deliberately reports `has_recorded_evidence`, `projection_current`, and the
latest projected timestamp. Current pause state, capture permission, and study expiry
belong to the app lifecycle/health surface and are not inferred by MCP.

Read tools are annotated read-only and closed-world. Their idempotency hint is false
because each successful disclosure has a new receipt/request identity and consumes
the grant's cumulative byte budget.

## Derived write tools

| Tool | Purpose |
| --- | --- |
| `chronicle_create_artifact` | Create a draft annotation, tag, hypothesis, or report |
| `chronicle_revise_artifact` | Append a revision against an exact expected prior |
| `chronicle_set_artifact_status` | Append a status-only revision while preserving prior payload/evidence |

Create and revise require caller-generated stable `request_id`, `artifact_id`, and
`revision_id` values. An exact retry has one canonical effect and is not recharged.
Revisions require at least one grant-visible event or chunk ID. The registered client
identity is injected by the server; an input can declare only `mcp-client` or `model`
authorship, and model authorship requires a model name.

Create always begins in `draft`. Revision and status transitions are compare-and-swap
operations using `expected_prior_revision_id`; concurrent writers produce one success
and one typed `artifact-conflict`. Status-only writes preserve artifact type, payload,
evidence, and confidence from the cited prior revision. Their idempotency hint remains
false because resolving that prior is itself a charged read.

A grant's time scope covers both cited evidence and artifact creation time. A grant
that can read an old historical range does not automatically authorize writing new
analysis today; the app must issue an explicit write-capable scope.

## Results and safe errors

Read tools return the versioned inner `QueryResponse`. Derived tools return the
versioned inner `DerivedArtifactWriteResponse`. Results include the effective grant,
scope/capabilities, stable cutoff, coverage/gaps where applicable, opaque IDs,
pagination/truncation, and provenance.

Successful and failed tool calls use `structuredContent` only; the text `content`
array is empty. This prevents JSON from being duplicated below the engine's charged
response boundary. The real-stdio suite asserts that the complete serialized MCP
result remains within the disclosure bytes charged by the engine.

Tool failures use a structured, content-free body:

```json
{
  "schema_version": "1.0",
  "error": {
    "code": "grant-inactive",
    "message": "The disclosure grant is expired, revoked, exhausted, or otherwise inactive."
  }
}
```

Errors never echo OCR, payloads, registration IDs, grants, managed paths, SQL, or
internal store details. Stdout is reserved for MCP frames. Startup/fatal diagnostics
write only a stable error code to stderr.

## Verification

From the repository root:

```bash
scripts/smoke-mcp.sh
```

The smoke test builds the real `chronicle-mcp` child process, completes MCP
initialization over stdio, verifies the exact tool/resource inventory, reads a
grant-gated schema, calls status, checks stderr isolation, and shuts down cleanly.
The Rust suite additionally covers language-neutral chunk/search/statistics parity,
unknown fields, bounds, no-grant leakage, dangling evidence, exact retry, immutable
revision/status behavior, sanitized malformed inputs, transport-level byte accounting,
and two real MCP child processes racing reads and immutable revisions while an app
writer projects canonical evidence. Post-race checks compare exact canonical and
SQLite identities, the one-tip artifact chain, SQLite integrity/foreign keys, chunk
count, and stale-generation rejection.
