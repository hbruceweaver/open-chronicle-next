# Event contract v1

`contracts/event-v1.schema.json` describes immutable factual event records. The
canonical form is one compact JSON object per journal line. The current version is
`1.0`.

## Envelope

Every record carries a stable event/device identity, scheduled time when the record
represents a cadence attempt, observed and recorded UTC timestamps, the display
timezone captured at observation time, source adapter/version, kind, and a payload
whose type must match the kind.

Kinds are `observation-attempt`, `recording-gap`, and `screenshot-lifecycle`.
Gap/lifecycle records are not fabricated cadence attempts.

## Independent observation axes

- Attempt: `completed`, `skipped`, `failed`.
- Evidence: `captured-new`, `captured-unchanged`, `protected`, `paused`,
  `unavailable`, `capture-failed`.
- Presence: `active`, `idle`, `locked`, `asleep`, `unknown`.
- OCR: `complete`, `empty`, `partial`, `failed`, `not-run`.

The type validator enforces valid combinations. Captured evidence may be idle and
may have failed OCR. Captured evidence permits only active, idle, or unknown
presence. Locked/asleep outcomes must use the matching unavailable reason and
presence state. Idle presence requires a positive metadata-only idle duration;
other presence states cannot carry that field. Unchanged content is a completed
attempt with `not-run` OCR, non-empty window identity/hash, and references the prior
factual content. Referential corpus tests prove that unchanged context, hash, OCR
source, and image artifact agree with the referenced changed observation. Missing
evidence is never converted into idle.

## Privacy boundary

Protected and no-evidence payloads are closed coarse enums. They have no field that
can hold an application identity, title, OCR, image, or freeform detail. Their JSON
objects reject unknown fields.

Captured OCR always contains `"untrusted_evidence": true`; false is not
deserializable. A canonical local image reference contains an opaque artifact ID and
a validated relative managed location for recovery. Absolute/traversing paths are
invalid. The observation records only a `pending` image intent; retained state exists
only after the additive write-completed lifecycle record. Image dimensions are
bounded to a 2,560-pixel long edge and eight megapixels. Query/MCP image metadata
deliberately omits the managed location and returns only opaque identity, lifecycle
state, and expiry.

## Lifecycle

Image write/delete/expiry/missing results are additive `screenshot-lifecycle`
events. Both retention expiry and user deletion are two phase: a
`delete-requested` record carries `deletion_cause=retention-expired` or
`user-requested` and projects `delete-pending`; a later `delete-completed` record
preserves that cause and projects `expired` or `user-deleted`. Request/completion
timestamps and causes must agree. A delete-request payload time is within its
request envelope's observed/recorded interval, and every terminal completion time
is within its terminal envelope's interval. An observation is never rewritten when
the projected image state changes.

## Compatibility

- Readers reject an unknown major `schema_version` with
  `ContractError::UnsupportedMajorVersion`.
- Within major version 1, ordinary envelope extensions are ignored by the typed
  reader and disappear on typed reserialization. Closed privacy/content objects do
  not accept extensions.
- Draft 2020-12 meta-validation, fixture validation, and selected mutated-contract
  differential tests keep the JSON Schema and Rust reader acceptance rules aligned.
- Privacy-sensitive protected/no-evidence payloads are closed and reject unknown
  fields even within the same major version.
- Enum values are lowercase kebab-case contract tokens, independent of Rust or Swift
  identifier spelling.

No project, workflow, productivity, intent, or recommendation field is canonical.
