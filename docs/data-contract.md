# Durable Data Contract

Open Chronicle acknowledges factual evidence only after its canonical record has
been appended and `sync_all` has completed. SQLite is a query accelerator. It is
never the only copy of an event, chunk, screenshot lifecycle transition, or derived
artifact revision.

## Managed root

The store is a fixed Application Support root owned by the current non-root login
user. Initialization applies process `umask 077`, repairs managed directories to
`0700` and files to `0600`, and rejects ownership mismatches. Managed operations
walk relative path components from an opened root descriptor with `openat`,
`O_NOFOLLOW`, and `O_CLOEXEC`. Absolute paths, parent traversal, and symlink
components are rejected.

SQLite is the sole pathname-based exception. Chronicle resolves the already-opened
root and proves the resolved path has the same device/inode before opening the fixed
`index.sqlite3` name with SQLite `NOFOLLOW` and private-cache flags.

## Canonical records

Event and chunk journals are daily append-only shards:

- `evidence/events/YYYY-MM-DD.jsonl`
- `aggregates/chunks/YYYY-MM-DD.jsonl`

Each line is one JSON object:

```json
{"body":{"schema_version":"1.0"},"checksum":"sha256-hex"}
```

`body` is serialized to canonical compact JSON first. `checksum` is SHA-256 over
those exact body bytes. The complete envelope plus newline is written through an
`O_APPEND` descriptor, followed by `sync_all`. Creation of a new shard also syncs
its parent directory. Stable IDs are unique across an entire journal family, not
merely within one daily shard. A same-checksum replay finds and synchronizes the
existing record before returning; the same ID with different bytes is critical
corruption.

Derived artifacts are immutable files at
`derived/<artifact-id>/<revision-id>.json`. Revision IDs are globally unique across
artifacts because the SQLite projection is rebuildable around that identity. A write
takes the shared store lock, then a fixed process-and-file global revision lock, then
the stable per-artifact exclusive lock, checks global revision ownership and
`expected_prior_revision_id`, writes a same-directory temporary file, syncs it,
atomically renames it, and syncs the directory. Revision chains are rebuilt by their explicit prior links, never by
authored timestamp or file name. Missing parents, multiple roots, branches, and
cycles are rejected. A retry of the identical revision is idempotent.

The shared derived-write surface separately enforces current-generation client
attribution, server-time and monotonic creation timestamps, existing in-scope
event/chunk references, immutable artifact type/prior links, and the draft status
state machine. Recovery still orders historical/imported revisions only by prior
links, so authored timestamps never become a substitute chain pointer. Projection
failure after the immutable rename is repaired by exact retry or startup rebuild.
Until that repair, the shared write surface reads and authorizes the canonical chain
tip directly; projection lag cannot turn an out-of-scope parent into an appendable
artifact for another grant.

## Screenshot transaction

A changed screenshot uses an additive transaction:

1. Write and sync a restricted provisional image.
2. Append and sync the observation containing a pending image intent.
3. Promote the provisional file to its final managed relative path and sync the
   directory.
4. Append and sync a `write-completed` lifecycle event.
5. Project both records, then acknowledge retained image durability.

The final path is derived only from the canonical observation date and artifact
ID (`screenshots/YYYY-MM-DD/<artifact-id>.heic`). Caller-provided paths must match
that derivation exactly. Existing final or provisional files are never overwritten.
Deletion resolves the path from the canonical source observation rather than
accepting a filesystem path from the caller.

Failure before step 2 removes the provisional image synchronously when the process
is alive. Startup removes an orphaned provisional with no canonical observation.
If an observation is durable, startup promotes its provisional or recognizes the
promoted final file, then appends a deterministic completion. If neither exists it
appends `write-failed`; it never manufactures a retained acknowledgement.
If a previously `write-completed` final file is later absent, startup appends the
additive `missing` lifecycle transition. Lifecycle projection verifies stable source
provenance and matching delete request/cause/timestamps before accepting completion.

Deletion is likewise additive: append `delete-requested`, unlink and sync the
directory, then append `delete-completed`. Startup finishes an interrupted request.
The original observation is not rewritten.

## Projection

Every SQLite write uses an immediate transaction. A journal projection inserts the
stable row and its typed detail, then advances that shard's byte cursor in the same
transaction. Chunk current pointers and aggregation watermark changes share that
transaction. The global aggregation watermark advances monotonically by UTC window
end, so late revisions for older buckets update their chunk without reopening later
buckets. Startup consumes the persisted byte cursor and requires it to match a
verified record boundary before skipping indexed bytes. Direct projection likewise
requires the current cursor to equal the next record's start offset, so a later
record cannot leapfrog an unprojected earlier record. Idempotent records behind the
cursor are accepted only when their projected stable ID and checksum still match.

Every connection asserts all of the following:

- bundled SQLite `3.53.2` and its exact source ID;
- runtime version at least `3.51.3`;
- foreign keys enabled;
- WAL journal mode;
- `synchronous=FULL`;
- one-second busy timeout.

The initial schema includes journal cursors, typed events/observations, OCR FTS,
chunk revisions/current pointers/evidence/dimensions/transitions, screenshot
lifecycle, artifact revision chains, health/retention state, store generation,
aggregation watermark, and registration receipts.

## Locks and generations

`locks/store.lock` is a stable inode. Normal app/MCP requests take it shared.
Rebuild, evidence deletion, and reset take it exclusive. A derived write takes its
artifact lock only after the shared store lock; lock upgrades are not supported.
All acquisitions use bounded waits.

`store-generation` is an authoritative, atomically replaced JSON document with a
monotonic generation and a UUID epoch. Destructive maintenance increments both.
Handles compare the complete document and return a stale-generation error rather
than writing into a replacement store. Write paths that wait for the shared lock
recheck generation after acquiring it; an exclusive maintenance operation cannot
be crossed using a generation sampled before the wait.

## Durability boundary

`durable` means the required `sync_all` calls and parent-directory syncs completed
under normal operating-system semantics. It covers process/app crashes and ordinary
restart recovery. It does not claim protection against every storage-controller
failure, a compromised same-user process, or external backup copies. Checksums
detect accidental corruption, not hostile tampering. Per-record `F_FULLFSYNC` is
not part of the MVP contract.
