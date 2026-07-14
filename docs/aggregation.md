# Factual aggregation and query contract

Open Chronicle v1 aggregates immutable observation facts into immutable five-minute
chunk revisions. The aggregator does not infer projects, workflows, productivity,
intent, or recommendations. Those interpretations can be stored later only as
derived artifacts with evidence references.

## Time and ordering

- Chunk windows are half-open UTC ranges whose starts are epoch multiples of 300
  seconds. Display timezones are labels only and never affect chunk identity.
- A bucket becomes eligible at `window.end + effective maximum cadence`. The
  effective cadence is the greater of the configured maximum and the 30- or
  60-second cadence recorded by any observation attempt in the bucket. This
  prevents a stale 30-second configuration from finalizing a 60-second stream
  early.
- Scheduling uses a boot/session sequence plus a strictly increasing monotonic tick.
  Wall-clock rollback, timezone travel, and DST do not authorize a second tick.
- Canonical event time is causal: an observation cannot precede its scheduled time,
  and `recorded_at` cannot precede `observed_at`.
- Inputs are ordered by `(observed_at, event_id)`. The stable event ID is the final
  tie breaker, so replay is deterministic.

## Coverage and duration

Each observation uses its required `scheduled_at` as its interval center, so capture
jitter does not move it into another bucket or create false edge gaps. Adjacent
sample boundaries use their midpoint when they are close enough; otherwise the
sample uses half its own cadence. A sample can cover at most `1.5 * cadence`, and
the bucket edges clamp the first and last interval within that cap. Explicit
recording gaps override samples.

The implementation assigns 300 whole UTC seconds exactly once:

- Evidence is a partition of `captured`, `protected`, `paused`, `unavailable`,
  `error`, and `gap`. It always sums to 300.
- Presence is a separate partition of `active`, `idle`, and `unknown` over captured
  coverage only. It always sums to `evidence.captured`.
- Missing samples become `gap`; absence is never labeled idle or active.
- Permission loss is unavailable, storage outage is error, and sleep/quit/clock
  correction is missing-observation coverage. Protected, locked, asleep,
  permission, error, and explicit gap intervals cannot contribute application time.
- Idle captured coverage remains factual evidence and presence, but is excluded
  from application, window, and authorized-domain duration estimates.

Application and window dimensions come only from captured permitted context.
Authorized-domain totals exist only when an explicit adapter supplied domain data;
the aggregator never derives a domain from a title or OCR text. Each estimate and
transition carries supporting event IDs, and aggregate drill-down uses the same IDs.

## Text and revisions

OCR extracts are deterministic, extractive, de-duplicated, and bounded to eight
excerpts of 512 Unicode scalar values. Empty and unchanged OCR does not create a new
extract. Text such as “ignore previous instructions” remains inert, untrusted
evidence and is never executed as a prompt.

The input digest covers canonical JSON for the ordered factual inputs. Chunk and
revision IDs are deterministic digests of device/window and
chunk/version/input/generation respectively. Re-running the same version and input
returns identical canonical bytes. Late evidence or a new aggregator version writes
a new physical revision with `prior_revision_id` and `supersedes_revision_id`; the
old journal line is never changed. Provenance-only rebuilds persist the current
reconciliation instant in the dirty-bucket row and include it in revision identity,
so retry bytes remain stable while the new revision is written to the actual
generation-date shard rather than reopening an old day. `late_input` is true only when factual input
arrived after an earlier revision (or after its due time); an algorithm-only revision
is not mislabeled late.

## Journal, projection, and recovery

Ingestion validates the event, appends and syncs the canonical event journal, then
projects it. A projection failure returns
`journal-durable-projection-pending`; it never converts journal failure into a
durable acknowledgement. Image-bearing observations and screenshot lifecycle events
must use the transactional managed-image path and are rejected by generic ingest
before append. A later chunk-stage failure preserves the already-durable event
acknowledgement while reporting lagging aggregation health.

Startup performs one replay of each event and chunk journal using U3 byte cursors,
then drains recovered aggregation without repeating canonical recovery before the
engine is returned. Event projection transactionally marks affected device/bucket
pairs dirty; chunk projection clears a marker only when that revision's evidence
references exactly cover current indexed bucket membership. The global
watermark bounds forward work, while dirty historical buckets cover late evidence.
An `aggregation_build_state` transaction materializes stale buckets once per
aggregator-version/store-generation change, avoiding a historical current-chunk scan
on every tick. Projection also maintains indexed `(device, bucket, event)` membership,
so each aggregation reads only its bucket. Pending work is processed in deterministic
1,024-bucket batches; startup drains all due batches, while ordinary ingest reports
lagging health if more work remains. If the current projected revision has the same input digest,
aggregator version, and store generation, no journal record is added. Otherwise an
immutable superseding revision is appended. Calculation, append, sync, row,
current-pointer, watermark, cursor, and commit crash boundaries therefore converge
on one active current revision. SQLite current pointers, pending buckets, build
state, and the aggregation watermark are rebuildable indexes, not canonical state.
The disposable projection schema adds these indexes through migration `0002`; opening
an existing v1 store upgrades it in place to schema/user version 2.

Canonical journal writers share a process-wide per-root stable-ID index seeded by
the single startup replay. Under the per-family writer lock, steady append reads an
append-only disposable manifest tail and at most the deterministic changed shard;
it does not enumerate or stat every historical shard. A durable pending-mutation
intent is written before canonical bytes change. After the canonical shard is
synced, the derived manifest is updated and the intent is cleared. Another process
therefore notices committed tails through the manifest, while an interrupted or
failed manifest update forces the named shard (or, for repair, canonical journals)
to be rescanned and re-manifested. Malformed, partial, missing, or replaced manifest
state is never authoritative and self-heals from canonical bytes. Repair writes a
full-scan intent before successor creation or original unlink. Exact retries reopen
and sync the canonical file and directory before returning a durable acknowledgement.

## Bounded factual queries

`StoreQueries`, `ActivitySearch`, and `FactualStatistics` are the common typed query
layer for the future Swift and MCP adapters:

- range event reads, current chunks, event detail, supporting evidence, and the
  aggregation watermark;
- application/window/authorized-domain totals, transitions, evidence coverage, and
  presence coverage for UTC-aligned ranges;
- literal FTS5 OCR search with typed filters, a maximum 100-item page, a maximum
  256-character/16-token query, SQL keyset pagination after typed filters, stable
  event cursors, and bounded escaped excerpts;
- statistics with an explicit 105,408 five-minute-bucket budget and coalesced
  adjacent gap intervals.

FTS operators and quotes are tokenized as literal evidence terms rather than accepted
as query syntax. Query projections can omit OCR payloads while preserving the factual
OCR outcome. Image responses contain only opaque artifact state and expiry recovered
from the source observation; managed paths and bytes never enter query types.

The query layer accepts no SQL, filesystem path, model prompt, semantic project, or
workflow grouping. Grants, response byte budgets, and MCP transport enforcement are
added by U5/U12 around these same services.
