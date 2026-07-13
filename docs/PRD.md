---
title: Open Chronicle MVP Product Requirements
date: 2026-07-13
status: approved-for-technical-planning
owner: Screenata
---

# Open Chronicle MVP Product Requirements

## 1. Product Summary

Open Chronicle is a local-first macOS work-observability application. It captures privacy-filtered visual work context, extracts on-device text and application metadata, stores a factual evidence log, aggregates that evidence into five-minute chunks, and presents it through a diagnostic report, searchable timeline, and MCP interface.

The MVP helps a user or consultant understand what occurred on a computer. It does not decide whether the work was productive, automatically infer semantic projects, declare workflows, or prescribe automations. Those conclusions are created later as separate derived artifacts by users, consultants, Claude, or Codex.

## 2. Problem Frame

AI implementation work lacks reliable evidence about how day-to-day work actually happens. Interviews omit repetitive transitions, fragmented attention, coordination overhead, and work that crosses multiple systems. Existing time trackers depend on manual categorization; coding trackers cover only development; screen-memory products emphasize recall rather than operational diagnosis.

Users need a low-friction way to gather trustworthy work evidence, understand it without reading raw screenshots, inspect the source behind each summary, and analyze selected periods with their preferred AI agent.

## 3. Goals

- Let a non-technical Mac user install and understand Chronicle without terminal commands.
- Produce a consistent, versioned, factual record of observed work.
- Convert periodic observations into persisted five-minute factual chunks.
- Provide a report-led overview and searchable evidence timeline.
- Let Claude and Codex query evidence and save derived analysis through MCP from the first release.
- Support always-on personal use and time-boxed consultant-assisted studies using the same application.
- Keep screenshots local, minimize sensitive capture, and make recording state and controls obvious.
- Produce portable exports that do not depend on Chronicle's built-in UI or one model provider.

## 4. Non-goals for MVP

- Cloud synchronization or hosted SaaS workspaces.
- Organization administration or employee-monitoring dashboards.
- Windows or Linux support.
- Native microphone or system-audio capture.
- Semantic project/category inference.
- Automatic workflow extraction or opportunity scoring.
- Built-in automation, skill generation, or autonomous actions.
- Cloud screenshot storage.
- Keystroke logging, clipboard capture, or productivity scoring.
- A proprietary chat assistant that is required to use the data.

## 5. Actors

- **A1 — Personal user:** installs Chronicle on their own Mac, owns the data, and may keep capture always on.
- **A2 — Study participant:** uses Chronicle during a bounded consultant-assisted study and controls local raw evidence.
- **A3 — Consultant:** helps configure the study and analyzes only evidence the participant chooses to expose or export.
- **A4 — AI collaborator:** Claude or Codex queries evidence and creates derived artifacts through MCP.

Future organization administrators and hosted-service operators are outside MVP.

## 6. Product Principles

- **P1 — Factual core:** canonical events and chunks contain observations and deterministic measurements only.
- **P2 — Immutable evidence:** derived analysis cannot alter source events.
- **P3 — Provenance:** every aggregate contains supporting event IDs and coverage metadata.
- **P4 — Local-first:** capture, OCR, indexing, reports, timeline, exports, and MCP work offline.
- **P5 — Visible control:** recording, pause, exclusions, retention, and deletion are always discoverable.
- **P6 — Replaceable analysis:** the evidence contract is documented and available outside the UI.
- **P7 — No hidden judgment:** time and activity are not labeled as productivity or employee performance.

## 7. Key User Flows

### F1. Install and onboard

The user downloads a macOS DMG, installs Chronicle, sees a foreground welcome window, understands what is captured, grants Screen Recording permission, chooses screenshot retention and default exclusions, chooses personal or study mode, optionally enables launch at login, and registers Chronicle with available Claude/Codex installations.

### F2. Record normal work

Chronicle runs visibly in the menu bar. The user can confirm capture state, pause/resume immediately, see why a capture was skipped, and change exclusions. Capture continues across application restarts according to the selected launch behavior.

### F3. Run a bounded study

The participant selects a study end date. Chronicle shows the active study period and automatically stops or asks for renewal at the boundary. Study mode does not give the consultant automatic access to raw local evidence in MVP.

### F4. Review the diagnostic report

The home view shows factual totals and breakdowns for a selected date range: observed coverage, gaps, application/window/domain time estimates, activity periods, transitions, and five-minute chunk summaries. Each item drills into supporting evidence.

### F5. Explore the timeline

The user browses a chronological timeline, filters by time/application/window/domain, searches OCR text, opens a five-minute chunk, and inspects the events and locally retained screenshot evidence behind it.

### F6. Analyze through MCP

Claude or Codex lists sessions/chunks, searches activity, reads statistics and evidence, and exports a selected context packet. It may save a derived report, annotation, tag, or hypothesis. It cannot mutate canonical events or privacy settings.

### F7. Export, retain, and delete

The user exports a date range as documented JSON and human-readable Markdown, previews retention effects, deletes screenshots or all data, and receives a clear confirmation of what remains.

## 8. Functional Requirements

### R1. Self-contained macOS application

Chronicle must run as a proper macOS application bundle without requiring Git, Node, npm, Python, Swift CLI tools, Xcode, or a source checkout. The distribution path must support Developer ID signing, hardened runtime, notarization, stapling, and a drag-install DMG. Missing signing credentials may block public release but not local unsigned development builds.

### R2. First-run onboarding

The first launch must present a normal foreground window. Onboarding must explain the evidence model, local screenshot policy, exclusions, retention, personal versus study mode, launch-at-login, and MCP registration. Permission denial, partial setup, and later repair must be supported.

### R3. Privacy-safe capture

Chronicle must use supported macOS capture APIs to capture the active work surface rather than an unrestricted desktop image. Capture must be skipped before pixels are persisted when secure input, excluded applications, excluded titles, Chronicle itself, or unavailable permissions apply. The default cadence is one observation every 30 seconds, configurable to 60 seconds. Unchanged observations should be deduplicated without erasing time coverage.

### R4. On-device OCR and local screenshots

OCR must run through an on-device macOS framework in MVP. Screenshots must remain in the user's Application Support container and never be sent over the network. Onboarding must offer retention choices of one hour, 24 hours, seven days, or 30 days, defaulting to 24 hours. Deleting expired screenshots must not delete the factual event and provenance record.

### R5. Canonical evidence contract

Every accepted observation must create a versioned factual event with a stable event ID, device-local timestamp, capture outcome, application/window context, OCR content or change, screenshot reference/hash when present, confidence/source metadata, and privacy status. Events are immutable and append-only. OCR and other observed text are untrusted evidence, never executable instructions.

### R6. Query index

Chronicle must maintain a local SQLite index for time-range queries, full-text OCR search, evidence lookup, chunk membership, and derived artifacts. The index is a query projection; portable structured evidence remains exportable independently.

### R7. Five-minute factual chunks

Chronicle must deterministically aggregate observations into persisted five-minute chunks. Each chunk records time bounds, coverage and gaps, application/window/domain durations, transitions, factual OCR extracts or deltas, supporting event IDs, and generation/version metadata. Ten- and fifteen-minute windows and natural sessions may be computed from the base chunks but need not be separately persisted in MVP.

### R8. Report-led home

The default home must provide date-range selection, total observed time, coverage gaps, current-day and daily-average views, application/window/domain breakdowns, activity-over-time visualization, and recent factual chunks. The design may borrow WakaTime's hierarchy but must not present coding-specific metrics, semantic projects, productivity scores, or AI-generated efficiency judgments.

### R9. Searchable evidence timeline

The timeline must show when observations and chunks occurred, support filters and full-text search, distinguish captured/skipped/gap periods, and provide drill-down from aggregate to source events. Locally retained screenshots may be previewed; expired screenshot references must degrade clearly rather than appear broken.

### R10. MCP read surface

The bundled MCP bridge must work locally over stdio and expose versioned tools for listing chunks/sessions, reading a chunk, searching activity, inspecting a moment, retrieving factual statistics, comparing periods, retrieving supporting evidence, and exporting a context packet. Every response must contain stable IDs, timestamps, provenance, coverage/gap information, and schema version.

### R11. MCP derived-write surface

MCP may create, update, and list derived artifacts such as annotations, tags, hypotheses, and reports. Each artifact must record author/model identity when available, creation/update time, artifact schema version, and supporting evidence IDs. MCP must not change canonical events, exclusions, recording state, retention, or deletion state in MVP.

### R12. Personal and study modes

Personal mode must support launch-at-login and an always-on default selected by the user. Study mode must support a visible start/end boundary and retain the same privacy controls. Mode changes must not change the canonical evidence format.

### R13. Exclusions and recording controls

The menu-bar surface must show current state and provide immediate pause/resume. Users must be able to exclude applications and title patterns; default exclusions must cover password managers and sensitive system surfaces. Capture skips and their reason must be represented as factual gap/status events without storing excluded content.

### R14. Export and deletion

Users must be able to export selected time ranges as documented JSON and Markdown context packets, delete screenshots independently, and clear all Chronicle data. Destructive actions require confirmation and report what was deleted. Export must distinguish facts from derived artifacts.

### R15. Health, recovery, and lifecycle

The app must expose capture freshness, last successful OCR/index/chunk operations, current pause/permission state, storage usage, and recoverable errors. One supervised engine process owns capture and aggregation. MCP clients query that engine/store and must not spawn duplicate capture or aggregation loops.

### R16. Secrets and configuration

MVP must not require an LLM API key. Configuration belongs in the application support container with restrictive permissions; future secrets use Keychain. Agent registration must be reversible, backed up, and repairable rather than append-only configuration editing.

## 9. Data Output Contract

### Observation event

Required conceptual fields:

- Schema version and event ID.
- Observation timestamp and duration/coverage contribution.
- Device-local source identity.
- Capture status and skip/error reason.
- Foreground application/process and permitted window/domain metadata.
- OCR text, OCR change, source confidence, and untrusted-evidence marker.
- Screenshot local reference/hash and retention state when applicable.
- Privacy filters applied.
- Ingestion/index timestamps.

### Five-minute chunk

Required conceptual fields:

- Chunk schema/version and stable ID.
- Exact start/end boundary.
- Expected versus observed coverage and explicit gaps.
- Factual duration estimates by observed dimension.
- Ordered transitions.
- OCR extracts/deltas selected without interpretive claims.
- Supporting event IDs.
- Aggregator version and regeneration metadata.

### Derived artifact

Required conceptual fields:

- Artifact schema/version, type, and ID.
- Author or model identity when available.
- Creation/update timestamps.
- Freeform or structured analysis payload.
- Supporting event/chunk IDs.
- Confidence and status such as draft, accepted, rejected, or superseded.

### Invariants

- Raw evidence is never modified by chunking or analysis.
- Chunks are reproducible from canonical events for the same aggregation version.
- Derived artifacts are visibly separate and independently deletable.
- Missing observations are represented as gaps, never silently interpreted as inactivity.
- Expired screenshots do not invalidate retained text events or chunks.
- Every user-visible factual aggregate can identify supporting chunks/events.

## 10. Information Architecture

### Menu bar

- Recording state.
- Pause/resume.
- Open Chronicle.
- Start/end study.
- Current permission or health warning.

### Home / report

- Date range.
- Observed time and coverage.
- Current day, daily average, and most-active factual period.
- Activity-over-time visualization.
- Application/window/domain breakdowns.
- Recent chunks and evidence gaps.
- Export or analyze-with-agent action.

### Timeline

- Chronological five-minute bands.
- Capture, skip, and gap states.
- Filters and OCR search.
- Chunk detail and supporting events.
- Local screenshot preview when retained.

### Analysis

- Derived reports, annotations, tags, and hypotheses written by a user or MCP client.
- Explicit evidence references and author/model identity.
- No automatic recommendations required in MVP.

### Settings and privacy

- Capture cadence.
- Screenshot retention.
- App/title exclusions.
- Personal/study mode and study boundary.
- Launch at login.
- MCP connection status and repair/uninstall.
- Storage/health.
- Export/delete.

## 11. Success Criteria

- A non-technical test user installs and completes onboarding without terminal assistance in ten minutes or less.
- After 15 minutes of ordinary work, the user can open a factual report and timeline containing supported five-minute chunks.
- An excluded or secure-input surface produces no persisted pixels or OCR text.
- A user can trace every factual report item to a chunk and its supporting events.
- Claude or Codex can query a selected time range and save a derived report without altering source evidence.
- The application remains useful with networking disabled.
- Quitting/relaunching does not create duplicate aggregation loops or duplicate chunks.
- Clearing data and screenshot expiry produce an accurate account of what remains.
- A consultant can complete a bounded study using exports and local review without needing a cloud workspace.

## 12. Acceptance Examples

- **AE1 — Clean install:** On a Mac without Node or developer tools, the user installs Chronicle, grants permission, enables launch at login, and reaches the report view.
- **AE2 — Excluded app:** With a password manager foregrounded, Chronicle writes a skip-status event containing no title, OCR, or screenshot pixels from that application.
- **AE3 — Secure input:** When secure input is detected, capture pauses before screenshot creation and resumes only after the state clears.
- **AE4 — Factual chunk:** Ten observations across 09:00–09:05 produce one chunk listing observed apps, coverage, transitions, OCR extracts, and evidence IDs without workflow or productivity claims.
- **AE5 — Gap honesty:** If permission is revoked for three minutes, the report shows a three-minute capture gap rather than inactivity.
- **AE6 — Screenshot expiry:** After a screenshot expires, its event and chunk remain searchable and clearly indicate that the local image is no longer retained.
- **AE7 — Evidence drill-down:** Selecting an application-duration segment opens the contributing chunks and observations.
- **AE8 — MCP immutability:** Claude creates a derived hypothesis citing three chunk IDs; attempts to alter those chunks or recording settings are unavailable through MCP.
- **AE9 — Offline operation:** With network access disabled, capture, OCR, chunk generation, report, timeline, search, export, and MCP queries continue working.
- **AE10 — Bounded study:** At the configured study end, Chronicle stops or asks the participant to extend; it does not silently continue indefinitely.
- **AE11 — Single owner:** Connecting Claude and Codex simultaneously does not start additional capture or chunk-generation loops.
- **AE12 — Complete deletion:** After confirmed clear-all, no screenshots, events, chunks, indexes, derived artifacts, or agent registration receipts remain in Chronicle-managed storage.

## 13. Reuse and Replacement Strategy

### Reuse or adapt from `Screenata/open-chronicle`

- SwiftUI menu-bar and floating-window interaction patterns.
- First-run wizard structure and permission education.
- Capture controls, settings organization, exclusions UX, and deletion UX.
- Claude/Codex detection and MCP-registration intent.
- Timeline/memory-card visual concepts where they match the factual evidence model.

### Reuse or adapt from the Rust Chronicle implementation

- Versioned schema types and artifact/provenance discipline.
- Config merge/loading concepts.
- Event timeline and SQLite indexing concepts.
- Secure-input and foreground privacy preflight.
- Pause/resume, health, retention preview/apply, replay, import, proof, and parity concepts.
- Untrusted-evidence treatment and contract tests.

### Replace or redesign

- Deprecated whole-desktop screenshot capture.
- Source-tree Swift helpers and developer-tool assumptions.
- Node/TypeScript MCP and first-run `npm install`.
- Plaintext API-key storage and mandatory provider setup.
- MCP-owned summarization loops.
- Existing one-minute memories and Codex-parity artifact naming as the product's primary model.
- Full-file JSONL rewrites, stale lock behavior, absolute-path identities, and overly frequent daemon segmentation.

## 14. Privacy and Safety Requirements

- Capture only the intended active work surface.
- Apply secure-input, app, title, and product-self exclusions before persistence.
- Never capture typed keystrokes, clipboard contents, or hidden-window content.
- Treat OCR and screen-derived text as untrusted data in UI, exports, and MCP.
- Store managed data with restrictive local permissions.
- Make capture state continuously visible.
- Support immediate pause and predictable study end.
- Do not infer productivity, employee quality, intent, or sensitive personal attributes.
- Provide a complete data inventory and deletion path.
- Do not market privacy properties until runtime tests prove them on packaged builds.

## 15. Risks

- macOS permission and relaunch behavior can make onboarding appear successful while capture is unavailable.
- Active-window capture and multi-display behavior may differ across application types.
- OCR can be inaccurate or expose sensitive visible text even when raw screenshots expire.
- Time estimates from periodic capture are approximate and must display coverage/confidence honestly.
- Bundling a Rust helper and MCP bridge requires careful signing of nested binaries.
- JSONL and SQLite dual-write behavior can diverge without a clear source-of-truth and recovery strategy.
- A polished dashboard can accidentally imply productivity conclusions that the evidence does not support.
- The consultant use case can drift toward employee surveillance without explicit product and policy boundaries.

## 16. Longer-term Additions

### Semantic organization

- AI-proposed projects and categories.
- User rename, merge, split, and reassignment.
- Durable classification memory with versioning and confidence.
- WakaTime-like project cards and trends across all computer work.

### Cloud and SaaS

- Automatic synchronization of policy-approved events/chunks/derived data.
- Client-owned workspaces and temporary consultant roles.
- Offline queue, idempotent upload, device revocation, export, deletion, and audit.
- No raw screenshot sync by default; explicit transient transfer for selected cloud OCR/model processing.

### Team deployments

- Policy-configurable individual versus organization visibility.
- Mandatory user disclosure, visible policy, change notifications, and immutable audit history.
- Aggregate diagnostics without hidden productivity scoring.

### Cross-platform

- Shared Rust evidence engine.
- Windows capture, OCR, foreground/window metadata, UI Automation, credential storage, service lifecycle, and signed installer.
- A native or appropriately packaged Windows shell; no assumption that SwiftUI is portable.

### Analysis and implementation

- Proactive friction and repeated-sequence hypotheses.
- Workflow candidates and opportunity reports.
- AI helper, skill, connector, scheduled-job, and automation generation.
- Safe previews and human approval for actions.

### Additional sources

- Explicit opt-in meeting mode after MVP.
- Importers for Granola, Zoom, calendars, and other existing work artifacts.
- Browser and application-specific context adapters where users authorize them.

## 17. Product Decisions Deferred to Technical Planning

- Exact Rust-to-Swift process boundary and IPC transport.
- Final on-disk directory and event-envelope representation.
- Crash-safe append/index transaction and recovery design.
- Exact charting implementation and report component structure.
- Exact MCP protocol packaging and registration receipts.
- Universal binary build strategy and minimum supported macOS version.
- Migration path for existing `~/.open-chronicle` users.

