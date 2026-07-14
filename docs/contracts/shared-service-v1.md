# Shared service transport contract v1

`contracts/shared-service-v1.schema.json` publishes the language-neutral outer
request/response transport used by the Swift bridge and local MCP adapter. It covers
content-free health, grant-bounded factual queries, immutable derived writes, stable
cutoff exports, and the content-free MCP error envelope.

The schema references the separately published event, chunk, derived-artifact, and
query v1 schemas by relative URI. MCP clients can resolve the complete bundle from the
five `chronicle://schemas/*/v1` resources without filesystem access (the sixth MCP
resource is status).

Every outer request carries the same request identity and nonzero store generation as
its nested operation. Every response repeats that identity and generation. Rust
validation additionally enforces these cross-field equalities, the 64 KiB request
ceiling, grant/capability rules, stable cutoff relationships, and recursive rejection
of filesystem-path and image-byte transport keys.

This transport has no capture, pause, privacy, retention, deletion, factory-reset,
raw SQL, arbitrary file, screenshot-byte, network, or model-call operation.
