# Derived artifact contract v1

`contracts/derived-artifact-v1.schema.json` describes analysis that is intentionally
separate from factual events/chunks. Types are annotation, tag, hypothesis, and
report.

Each revision is immutable and records artifact/revision identity, expected prior
revision, author/client/model identity when known, creation time, status, payload,
supporting event/chunk IDs, optional confidence, and store generation. A committed
revision's `prior_revision_id` must equal the expected prior revision used by the
compare operation. At least one evidence reference is required.

The payload is deliberately freeform JSON because this is the analysis plane; it may
contain hypotheses or recommendations. Its content never becomes a canonical fact
and cannot mutate cited evidence. Concurrent writers create one accepted revision or
a typed conflict in the later storage layer.

Unknown major versions fail. Same-major ordinary envelope extensions are ignored by
the v1 typed reader. Store generation must be nonzero and a revision cannot
supersede itself. Draft 2020-12 meta/fixture validation keeps the published schema
executable rather than illustrative. Status/type/author tokens are lowercase
kebab-case.
