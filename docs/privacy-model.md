# Local privacy and disclosure model

## Trust boundary

The MVP trusts signed Open Chronicle processes and the current macOS login account.
Restrictive managed-directory permissions, anchored file operations, checksums,
and atomic receipts reduce accidental exposure and corruption. They do not defend
against a compromised process running as the same user or prove hostile tampering.

Screenshots remain local. Agent-facing contracts expose only opaque image artifact
IDs, projected retained/expired/deleted states, and expiry time. They contain no
image bytes, absolute paths, or managed relative screenshot paths.

## Evidence versus operations

Canonical events and five-minute chunks are immutable factual evidence. The U5b
shared API offers bounded evidence reads, explicit export, immutable derived writes,
and content-free health. Capture, privacy,
retention, deletion, and evidence mutation are absent from the agent-facing
operation enum. Derived artifacts cite existing in-scope evidence and never become
canonical facts.

## Grant lifecycle

Installing Chronicle or registering MCP does not authorize evidence disclosure.
The user must create a client-specific grant. Grants are visible operational
receipts with bounded time, content, expiry, page volume, response volume, total
volume, and store generation.

Authorization is checked on every request. Expiry fails closed. Revocation takes
effect for the next request, removes outstanding cursor receipts, and does not
change evidence. A query and its receipt charge hold the grant lock together, so a
concurrent revoke cannot permit an uncharged response or a later request using an
old cursor.

Derived-write request receipts and cumulative byte charges are committed together.
An exact retry after an artifact/projection/receipt crash compares the complete
canonical revision and is not charged twice. Reusing the request ID with mutated
payload, status, attribution, or evidence fails closed.
Receipt history is bounded to 4,096 derived writes. The service refuses a new write
before creating its canonical artifact once that bound is reached; revocation and
store-generation cleanup remove receipts that can no longer authorize a retry.

OCR is treated as a distinct disclosure class. In particular, FTS search requires
OCR authorization even if text snippets are omitted, since the existence and IDs of
matches are themselves OCR-derived disclosure.

## Store generation

Every grant and query is bound to the durable store generation. Shared reads hold a
store request lock while authorizing, querying, charging, and issuing cursors.
Delete Evidence/reset advances generation under the exclusive maintenance lock.
Stale clients fail before reading or updating receipts and cannot recreate deleted
data in the new generation. Health classifies receipts from an earlier generation
as stale rather than active, revoked, expired, or exhausted in the current store.

## Content-free health

Diagnostic health is a typed structure of timestamps, numeric counts/bytes,
projection state, and enumerated issue codes. It intentionally has no arbitrary
message strings, application identities, window titles, domains, OCR, screenshot
references, or filesystem paths. Its fields cover operation freshness,
aggregation watermark/backlog, projection lag, storage capacity, and aggregate MCP
grant state without exposing grant/client IDs.

Projection health compares canonical journal tails with durable projection
cursors. Journal-durable records that are not projected produce a typed lagging
state, pending-record count, and acknowledgement; a current state requires zero
pending records and zero projection lag. Operation timestamps later than the
health observation time are omitted, so a supported wall-clock rollback cannot
turn content-free diagnostics into an error or report a future operation time.
Lag detection reads shard sizes plus bytes after durable projection cursors; it
does not scan historical records. Managed-size accounting runs outside the
capture consistency lock and is cached for 30 seconds, so health polling cannot
hold capture behind a filesystem walk.

Latest scheduled-attempt, capture, OCR, event-projection, and chunk-projection
times come from a rebuildable `health_operation_facts` projection. The projector
updates those facts in the same transaction as their source row, and health uses
the fact-type/time index with `ORDER BY … LIMIT 1`; it does not aggregate across
the historical events or chunks tables while capture is waiting.
