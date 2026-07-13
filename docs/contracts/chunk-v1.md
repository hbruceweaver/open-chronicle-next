# Five-minute chunk contract v1

`contracts/chunk-v1.schema.json` describes an immutable deterministic revision of a
UTC-aligned half-open five-minute window. The current version is `1.0`.

## Identity and revisions

`chunk_id` is the logical UTC-window identity. `revision_id` is physical and
immutable. Late input or a new aggregator creates another revision with prior and
supersession links; it does not overwrite the earlier record. `aggregator_version`
and `input_digest` make byte-stable regeneration testable.

The half-open interval is exactly 300 seconds and starts at a UTC epoch multiple of
300. A revision cannot be generated before the window ends, supersede itself, or
carry disagreeing prior/supersession links. Provenance and store generation are
non-empty/current.

## Coverage

Evidence seconds are one non-overlapping 300-second partition: captured, protected,
paused, unavailable, error, and gap. Presence seconds are a separate partition of
captured coverage and therefore equal captured seconds; `unknown` absorbs factual
uncertainty. Idle is not application time.

Dimension estimates are factual application, window, or explicitly authorized
domain estimates with supporting event IDs. Transitions, gaps, and extractive OCR
excerpts retain their evidence IDs. Every OCR excerpt carries
`untrusted_evidence=true`.

All nested evidence references must belong to the chunk's unique
`supporting_event_ids`. Transitions stay inside the half-open window. Gaps are
ordered, positive, non-overlapping subintervals; observed gap kinds require a
supporting record. Their whole-second durations must exactly reconcile the
non-captured evidence partition: protected to protected, paused to paused,
unavailable to unavailable, error to error, and missing-observation to gap.
Estimates are unique per dimension/key and each dimension total cannot exceed
captured coverage.

The synthetic corpus includes two dedicated acceptance packets: ten 30-second
interval-centered attempts from `09:00:15` through `09:04:45` producing exactly 300
seconds of coverage (AE4), and ten unchanged attempts reusing one context/hash/image
artifact while retaining the same 300 seconds (AE13). AE13 begins with an explicit
pre-window changed observation plus its retained `write-completed` lifecycle record;
every in-window unchanged attempt resolves its prior event and image artifact to that
seed.

## Compatibility

Major versions other than 1 fail before typed deserialization. Same-major ordinary
envelope extensions are ignored by v1 readers. Draft 2020-12 meta/fixture and
differential tests enforce schema-reader parity. Enum tokens are lowercase
kebab-case. A v1
chunk contains no semantic project, workflow, productivity, intent, recommendation,
or model-authored summary field.
