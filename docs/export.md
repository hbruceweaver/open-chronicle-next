# Stable-cutoff export

Every context packet and full export reads one pinned SQLite WAL snapshot. The
manifest records the projected journal byte cutoff for every included family/shard,
the current store generation, effective UTC range, coverage and explicit gaps,
included/available counts, content-class inventory, component SHA-256 checksums,
and whether the byte budget truncated the result. Projection writes that commit
after the snapshot is pinned are excluded and remain available to capture.

JSON export returns typed path-safe event, chunk-revision, and optional derived
artifact DTOs. Markdown renders those same DTOs and facts as indented JSON blocks,
so untrusted OCR or derived text cannot terminate a Markdown code span; it is not a
second query implementation. Context packets contain current fully-contained chunks plus only
their individually range-contained supporting events. Full export contains all
individually contained events and fully-contained chunk revisions in the range.
An included derived revision must have its creation time and every cited event and
current chunk wholly inside the effective range.

Screenshots are excluded in every mode. No image bytes, absolute paths, or managed
relative paths enter an export. OCR is included only when the explicit OCR grant
class permits it. Derived artifacts are opt-in, remain visibly separate from facts,
and are never used to rewrite event/chunk evidence.

`max_bytes` bounds the complete serialized shared-service response, including the
manifest, provenance, and grant summary, not just the selected evidence payload.
The service selects a deterministic prefix, recomputes counts and checksums after
truncation, and fails before charging when even an empty response envelope cannot
fit. Selection is also bounded by fixed row ceilings. Available
counts describe the pinned snapshot before byte truncation; included counts describe
the emitted payload. Rebuilding SQLite from the immutable journals and artifact
files produces the same selection and checksums for the same cutoff-equivalent data.
