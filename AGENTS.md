# Open Chronicle agent instructions

Read `context.md` and `docs/PRD.md` before planning or implementation.

## Product invariants

- Canonical observations and five-minute chunks are factual, versioned, provenance-linked, and immutable.
- Hypotheses, annotations, reports, workflow candidates, and recommendations are separate derived artifacts.
- Screenshots remain local in MVP; OCR runs on-device.
- Capture privacy checks happen before pixels or OCR are persisted.
- Missing evidence is represented as a gap, never inferred as inactivity.
- MCP may read evidence and write derived artifacts; it may not mutate evidence or privacy settings.
- Semantic project inference, cloud/team features, Windows, audio, and automation generation are post-MVP.

## Engineering

- Simplicity first; prefer a single supervised engine and explicit contracts.
- Add regression or contract coverage for behavior changes.
- Prove packaged/runtime behavior, not only library compilation.
- Preserve source attribution when adapting code from the two predecessor implementations.
- Never commit screenshots, OCR captures, local databases, credentials, or other real user evidence.
- Use `rtk` in front of shell commands.
- Never use `rm -rf`; move disposable files with `trash`.

