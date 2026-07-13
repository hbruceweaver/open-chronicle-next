# Recovery and Administrative Verification

Recovery treats canonical journals and immutable artifact revision files as truth.
It does not infer missing work or silently skip damage.

## Startup recovery

Under the exclusive maintenance lock Chronicle:

1. Verifies complete event and chunk lines and their body checksums.
2. Copies an incomplete trailing fragment to `diagnostics/`, truncates only that
   fragment back to the last newline, syncs the shard, and records the diagnostic
   relative path.
3. Reconciles provisional, promoted, retained, and deletion-pending screenshots by
   appending lifecycle evidence when required.
4. Reads persisted byte cursors, verifies that each is on a canonical record
   boundary, and replays only complete unindexed records idempotently into SQLite.
   Row data and the journal byte cursor commit together.
5. Validates all immutable derived revision chains and projects them.
6. Reprojects authoritative store generation and agent registration receipts.

A complete malformed line, invalid contract, or checksum mismatch blocks at that
offset. The scanner leaves the shard byte-for-byte unchanged. There is no automatic
“skip bad line” path. Explicit repair requires the exact confirmation phrase,
archives the complete damaged shard and checksum, quarantines the corrupt record
and every later byte, starts a deterministic successor with only the verified
prefix, and records a factual repair/loss event. A durable pending/completed repair
receipt makes every archive, successor, unlink, and marker boundary resumable after
a process crash without creating a second successor or marker.

If the SQLite file is corrupt or cannot satisfy its schema identity at startup,
Chronicle automatically rebuilds it from the canonical sources under the same
exclusive lock. Canonical journal corruption is not treated as SQLite corruption
and continues to block until explicitly repaired. Startup also verifies the stable
row and checksum for records behind each cursor; if a valid cursor masks missing or
mismatched disposable rows, Chronicle rebuilds rather than returning incomplete
queries.

## Rebuild

`rebuild-index` takes the exclusive stable store lock. It creates a new SQLite file,
replays event/chunk journals and artifact revisions, recomputes lifecycle/current
chunk/current artifact/watermark/cursor state, imports authoritative generation and
receipts, checkpoints and syncs the new database, then atomically swaps it into
place. The prior index is retained in managed diagnostics. A stable-ID snapshot is
compared before the rebuilt projection is returned. The snapshot also carries a
deterministic digest over schema identity, cursors, typed observation rows, OCR FTS,
lifecycle state, chunk and artifact evidence tables, current pointers, retention,
generation, watermark, and registration receipts. Runtime health rows are excluded
because they describe the current recovery run rather than canonical historical
evidence.

Agent registration receipts are an exact projection of the authoritative receipt
document: each startup transaction clears prior projected receipts and inserts the
current document. Removing the document therefore clears stale projected grants
instead of leaving them active until a later rebuild.

No capture loop runs in the admin binary. It never reads or migrates predecessor
Chronicle data.

## Commands

```sh
chronicle-admin verify-journals <managed-root>
chronicle-admin rebuild-index <managed-root>
chronicle-admin repair-journal <managed-root> <events|chunks> <shard> <device-id> \
  "I UNDERSTAND THIS QUARANTINES UNVERIFIED EVIDENCE"
chronicle-admin replay <synthetic-fixture-directory> <managed-root>
```

`verify-journals` checks canonical journals and artifact chains without requiring
an existing SQLite projection. It repairs only partial tails. `rebuild-index`
returns the replay counts and stable-ID snapshot. `replay` is limited to an
explicit synthetic fixture directory and is intended for development/CI proof.
`repair-journal` performs only the explicitly confirmed repair, then rebuilds the
projection and prints both reports.

## Crash proof

The store exposes internal fault points at journal append/sync, row/cursor/current
pointer/watermark updates, commit acknowledgement, artifact rename/directory sync,
each image promotion/unlink directory-sync boundary, and each persisted repair
phase. Integration tests use both controlled errors and child-process `abort`
injection. The process cases prove synced-journal replay, SQLite rollback,
crash-resumable repair, one-winner artifact compare-and-swap, bounded cross-process
locks, and stale-generation rejection. A macOS immutable-directory test provides a
real filesystem write denial and verifies that it maps to a critical, blocked,
not-durable health result.

The seven U3 integration suites are:

- `journal`
- `crash_recovery`
- `projection_rebuild`
- `chunk_recovery`
- `artifact_recovery`
- `concurrency`
- `permissions`
