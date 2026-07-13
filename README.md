# Open Chronicle Next

Open Chronicle is a local-first macOS work-observability application. It turns privacy-filtered screen observations into a factual, queryable evidence record, five-minute work chunks, a diagnostic report, a searchable timeline, and MCP context for Claude and Codex.

This repository is the productized successor to the original [`Screenata/open-chronicle`](https://github.com/Screenata/open-chronicle) proof of concept. It combines that project's SwiftUI product shell with the stronger evidence, privacy, health, and retention substrate proven in the Rust Chronicle implementation.

The MVP is currently in planning and initial implementation.

## Start here

- [Product context](context.md)
- [MVP product requirements](docs/PRD.md)

## MVP boundary

- macOS first
- self-contained DMG and onboarding
- privacy-safe local capture and on-device OCR
- immutable factual events and five-minute chunks
- report-led home and searchable evidence timeline
- MCP read access plus derived-artifact writes
- personal and time-boxed consultant-study modes

Cloud sync, team administration, Windows, audio capture, semantic project inference, and workflow/automation generation are post-MVP.

## License

MIT

