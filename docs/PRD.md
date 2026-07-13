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

The user downloads a macOS DMG, installs Chronicle, sees a foreground welcome window, understands what is captured, grants Screen Recording permission, chooses screenshot retention and default exclusions, chooses personal or study mode, optionally enables launch at login, and may register Chronicle with a supported Claude/Codex installation that is actually present. Agent absence or an unsupported client version remains a visible, repairable optional-integration state and does not block recording.

### F2. Record normal work

Chronicle runs visibly in the menu bar. The user can confirm capture state, pause/resume immediately, see why a capture was skipped, and change exclusions. Capture resumes across normal application restarts and future logins according to the selected launch behavior. Launch at login is not crash supervision in MVP: a fatal app crash stops recording until the user relaunches Chronicle or the next login, and the next launch records the resulting factual gap.

### F3. Run a bounded study

The participant selects a study end date. Chronicle shows the active study period, warns before expiry, and automatically pauses at the exact boundary. If the Mac sleeps through the boundary, Chronicle checks expiry before the first wake capture. Continuing requires an explicit participant extension. Study mode does not give the consultant automatic access to raw local evidence in MVP.

### F4. Review the diagnostic report

The home view shows factual totals and breakdowns for a selected date range: observed-computer-time estimates, idle/protected/paused/unavailable time, evidence coverage, application/window time estimates, optional authorized-domain estimates, transitions, and five-minute chunks. Each item drills into supporting evidence.

### F5. Explore the timeline

The user browses a chronological timeline, filters by time/application/window/status and authorized domain when available, searches OCR text, opens a five-minute chunk, and inspects the events and locally retained screenshot evidence behind it.

### F6. Analyze through MCP

Claude or Codex lists chunks, searches activity, reads statistics and evidence, and exports a selected context packet only through an explicit, revocable per-client disclosure grant. A grant bounds the time horizon, permitted content classes, expiry, and response volume; full OCR is denied until the user grants it. The client may save a derived report, annotation, tag, or hypothesis revision. It cannot mutate canonical events, capture state, privacy settings, retention, or deletion state, and it never receives screenshot bytes or arbitrary local paths.

### F7. Export, retain, and delete

The user exports a date range as documented JSON and human-readable Markdown, previews retention effects, deletes screenshots or Chronicle evidence, and receives a clear confirmation of what remains. Factory reset is a separate operation that also removes only matching Chronicle-owned Claude/Codex registrations.

## 8. Functional Requirements

### R1. Self-contained macOS application

Chronicle must run as a proper macOS application bundle without requiring Git, Node, npm, Python, Swift CLI tools, Xcode, or a source checkout. The distribution path must support Developer ID signing, hardened runtime, notarization, stapling, and a drag-install DMG. Missing signing credentials may block public release but not local unsigned development builds.

### R2. First-run onboarding

The first launch must present a normal foreground window. Onboarding must explain the evidence model, local screenshot policy, exclusions, retention, personal versus study mode, launch-at-login, and MCP registration. Permission denial, partial setup, and later repair must be supported.

### R3. Privacy-safe capture

Chronicle must use ScreenCaptureKit to capture one exactly matched foreground window rather than an unrestricted desktop image. It resolves `NSWorkspace.frontmostApplication`, filters front-to-back Core Graphics window metadata to eligible on-screen normal windows owned by that PID, chooses the first eligible window, and requires that exact window number to map to one `SCWindow`. It does not require Accessibility permission and rejects no-match or ambiguous cases. One shared privacy predicate checks study expiry, pause/lock/sleep/permission/secure-input state, product-self/application/title exclusions, PID, and window identity immediately before capture and again immediately after capture. Any post-capture failure discards pixels before hashing, OCR, encoding, or persistence; a changing permitted title alone is not an identity mismatch. Chronicle never falls back to whole-display capture. The default cadence is one scheduled attempt every 30 seconds, configurable to 60 seconds. Every attempt records a factual outcome when storage is durable. After post-capture validation, Chronicle hashes normalized pixels before OCR; unchanged observations skip OCR and image encoding while preserving coverage through a reference to the prior content hash/event and OCR.

### R4. On-device OCR and local screenshots

OCR must run through Vision on-device in MVP. Screenshots must remain in Chronicle's managed Application Support directory, and Chronicle itself must never transmit them over the network. This promise does not claim to constrain unrelated software that the user independently grants filesystem access. Onboarding must offer retention choices of one hour, 24 hours, seven days, or 30 days, defaulting to 24 hours. A new image write is transactional: Rust writes and syncs a restricted provisional file, appends an observation containing a pending artifact intent, promotes the image by anchored atomic rename and directory sync, appends a lifecycle completion record, and only then acknowledges durable retained image evidence. If the observation append fails, Chronicle synchronously removes the provisional file and directory-syncs; startup and periodic reconciliation finish or remove interrupted intents. Screenshot expiry is separately two-phase: Chronicle durably records a delete request, deletes and directory-syncs the managed image, then durably records completion. Startup finishes pending requests. These lifecycle operations do not mutate the source observation or delete retained OCR, events, or chunks. OCR/events/chunks are retained until explicit evidence deletion, and onboarding must state that distinction plainly.

### R5. Canonical evidence contract

For every scheduled attempt, Chronicle must durably append one versioned factual event whenever canonical storage is writable. Event kinds are `observation-attempt`, `recording-gap`, and `screenshot-lifecycle`; lifecycle and gap records are not imaginary scheduled attempts. Each observation event has a stable event ID, device ID, scheduled/observed/recorded timestamps, local timezone, cadence, source, and independent typed axes: attempt status (`completed`, `skipped`, or `failed`), evidence state (`captured-new`, `captured-unchanged`, `protected`, `paused`, `unavailable`, or `capture-failed`), presence state (`active`, `idle`, `locked`, `asleep`, or `unknown`), and OCR state (`complete`, `empty`, `partial`, `failed`, or `not-run`). Successful events may contain permitted application/window context, explicitly authorized domain context, OCR content/change, screenshot artifact reference/hash, and confidence/source metadata. Skip payloads must not include excluded titles, OCR, screenshots, or sensitive application details. A canonical event is acknowledged only after durable journal append; acknowledgement distinguishes `durable`, `journal-durable-projection-pending`, and `not-durable`, while projection health separately distinguishes `current`, `lagging`, `rebuilding`, and `blocked`. If journal append fails, the failure remains in memory/OSLog and visible health; after recovery Chronicle appends a retrospective `storage-outage` interval event rather than pretending the failed write was stored. Sleep/quit intervals similarly become factual gap intervals on wake/relaunch, not synthetic cadence attempts. Events are immutable append-only daily JSONL journal records; OCR and other observed text are marked untrusted evidence, never executable instructions.

### R6. Query index

Chronicle must maintain a local SQLite index for time-range queries, full-text OCR search, evidence lookup, chunk membership, statistics, health, and derived artifacts. Daily append-only event and chunk JSONL journals plus immutable per-artifact revision files are canonical. Screenshot lifecycle records are typed events in the event journal; chunk supersession is recorded in the chunk journal. Shards are selected by UTC `recorded_at`/`generated_at`, so late records do not silently reactivate historical files. Ingestion must append and sync the relevant canonical record and parent directory before committing the SQLite projection, record per-shard projection cursors plus a separate chunk aggregation watermark, replay valid unindexed records on startup, and be idempotent by stable record ID. A partial trailing record is preserved for diagnostics and recovery returns to the last complete line; a corrupt complete record stops projection visibly. SQLite must be fully rebuildable from all canonical records, including lifecycle state and derived artifact history.

### R7. Five-minute factual chunks

Chronicle must deterministically aggregate observations into persisted five-minute chunks aligned to UTC half-open boundaries such as `[09:00, 09:05)`, with one maximum-cadence finalization grace. Evidence-state seconds (`captured`, `protected`, `paused`, `unavailable`, `error`, and `gap`) form one non-overlapping partition that sums to 300 seconds. Presence-state seconds (`active`, `idle`, and `unknown`) are a separate partition over captured coverage only. Each chunk also records estimated application/window and optional authorized-domain durations, ordered transitions, factual OCR extracts/deltas, supporting event IDs, aggregator version, and input digest. Duration attribution must use adjacent-sample midpoint boundaries, clamp the first/last centered sample to the bucket boundary, cap contribution at 1.5 times the configured cadence, assign idle samples to idle rather than applications, and leave uncovered remainder as a gap. A late input or new algorithm creates an immutable chunk revision with a supersession link; it never overwrites a prior version. Ten- and fifteen-minute factual windows may be composed from base chunks but need not be separately persisted in MVP.

### R8. Report-led home

The default home must provide date-range selection, observed-computer-time estimates, evidence coverage, idle/protected/paused/unavailable time, current-day and daily-average views, application/window breakdowns, activity-over-time visualization, transitions, and recent factual chunks. Domain cards appear only when an explicitly authorized adapter supplied domain data and must be hidden otherwise; domains are never inferred from window titles or OCR. The design may borrow WakaTime's hierarchy but must not present coding-specific metrics, semantic projects, productivity scores, or AI-generated efficiency judgments.

### R9. Searchable evidence timeline

The timeline must show when observations and chunks occurred, support filters and full-text search, distinguish captured/skipped/gap periods, and provide drill-down from aggregate to source events. Locally retained screenshots may be previewed; expired screenshot references must degrade clearly rather than appear broken.

### R10. MCP read surface

The bundled MCP bridge must work locally over stdio and expose versioned resources and tools for status/schemas, listing chunks, reading a chunk, searching activity, inspecting a moment, retrieving factual statistics, comparing periods, retrieving supporting evidence, and building a bounded context packet response. Registration is explicitly opt-in and each client receives a visible, revocable grant stored in Chronicle's policy/receipts. The grant bounds permitted time range or rolling horizon, metadata/OCR/derived content classes, expiry, per-response size, and cumulative disclosure; pagination cannot escape it and full OCR defaults to denied. Every response must contain stable IDs, timestamps, provenance, coverage/gap information, schema version, pagination/truncation metadata, grant/capability flags, and limits. OCR is explicitly labeled untrusted. MCP returns screenshot metadata/current projected lifecycle state only: no image bytes, arbitrary local paths, or screenshots in context packets.

### R11. MCP derived-write surface

MCP may create, list, and append revisions/status revisions to derived artifacts such as annotations, tags, hypotheses, and reports. Each immutable revision must record author/model identity when available, creation time, artifact schema version, expected prior revision, status, and supporting evidence IDs. MCP never updates a revision in place and must not change canonical events, exclusions, recording state, retention, or deletion state in MVP.

### R12. Personal and study modes

Personal mode must support launch-at-login and an always-on default selected by the user. Study mode must support a visible start/end boundary, warning/countdown, exact auto-pause at expiry, and explicit extension while retaining the same privacy controls. Expiry must be checked before capture on every tick and immediately after wake. Mode changes must not change the canonical evidence format.

### R13. Exclusions and recording controls

The menu-bar surface must show current state and provide immediate pause/resume. Users must be able to exclude applications and title patterns; default exclusions must cover password managers and sensitive system surfaces. Capture skips and their reason must be represented as factual gap/status events without storing excluded content.

### R14. Export and deletion

Users must be able to export selected time ranges as documented JSON and Markdown context packets, delete screenshots independently, and perform two distinct destructive operations. **Delete Chronicle evidence** stops capture, waits for in-flight UI/MCP store requests under an exclusive maintenance lock, increments a durable store generation, removes screenshots/journals/chunks/derived artifacts/indexes plus evidence-bearing managed diagnostics and exports unless the user explicitly preserves an export copy, and preserves settings/MCP receipts. **Factory reset** additionally unregisters only external Claude/Codex entries that still match Chronicle's receipt, then removes receipts/configuration and all Chronicle-managed evidence, diagnostics, and exports. Stale clients must reopen the new generation or receive a maintenance/reset error and may not recreate old data. Destructive actions use preview, explicit confirmation, progress, partial-failure recovery, and a final inventory of what was deleted and what remains. Export must distinguish facts from derived artifacts and excludes screenshots by default. Chronicle cannot erase copies a user moved outside its managed directory or copies retained by backups, and must say so.

### R15. Health, recovery, and lifecycle

The app must expose scheduled-attempt freshness, last successful capture/OCR/journal/projection/chunk operations, aggregation watermark, projection lag, current pause/permission/study state, storage usage/free space, MCP grants/self-test state, and recoverable errors. One authoritative signed application process and process-lifetime capture-owner lock host capture and aggregation through a serialized Rust core handle; a second app instance activates the first rather than starting a coordinator. `SMAppService.mainApp` provides launch at login only, not crash supervision; a fatal crash leaves capture stopped until manual relaunch or the next login, and recovery records the gap. MCP processes use the shared Rust query/derived-artifact service and must not spawn capture or aggregation loops.

### R16. Secrets and configuration

MVP must not require an LLM API key. Configuration belongs in the Chronicle-managed Application Support directory with restrictive permissions; future secrets use Keychain. `config.json`, `store-generation`, and `receipts/agent-registrations.json` are authoritative operational files included in repair/rebuild equivalence checks; projection cursors, watermarks, health snapshots, and current pointers are recomputed. Agent registration and disclosure grants must be reversible, backed up, visible, and repairable rather than append-only configuration editing.

## 9. Data Output Contract

### Observation event

Required conceptual fields:

- Schema version and event ID.
- Scheduled, observed, and recorded timestamps, display timezone, cadence, and coverage contribution.
- Stable device identity and source adapter.
- Attempt, evidence, presence, and OCR axes plus skip/error reason.
- Metadata-only presence state: active, idle, locked, asleep, or unknown.
- Foreground application/process and permitted window/domain metadata.
- OCR text, OCR change, source confidence, and untrusted-evidence marker.
- Screenshot artifact ID, initial managed relative reference, hash, dimensions, policy, and `expires_at` when applicable; never an absolute path. Current lifecycle state is projection-derived.
- Privacy-policy version and coarse rule category, without excluded content.
- Ingestion/index timestamps.

### Five-minute chunk

Required conceptual fields:

- Chunk schema/version and stable ID.
- Exact UTC half-open start/end boundary and display timezone.
- Expected versus observed coverage and explicit gaps.
- Non-overlapping evidence-state seconds that sum to the five-minute window.
- Separate presence-state seconds over captured coverage.
- Capped factual duration estimates by observed dimension.
- Ordered transitions.
- OCR extracts/deltas selected without interpretive claims.
- Supporting event IDs.
- Aggregator version, input digest, revision/supersession, and generation metadata.

### Derived artifact

Required conceptual fields:

- Artifact schema/version, type, and ID.
- Author or model identity when available.
- Creation timestamp and immutable prior-revision relationship.
- Freeform or structured analysis payload.
- Supporting event/chunk IDs.
- Confidence and status such as draft, accepted, rejected, or superseded.

### Invariants

- Raw evidence is never modified by chunking, retention, or analysis during retention; authorized evidence deletion is the explicit destructive exception.
- Chunks are reproducible from canonical events for the same aggregation version.
- Late input or a new aggregation algorithm creates a new immutable chunk revision.
- Derived artifacts are visibly separate, revisioned outside SQLite, and independently deletable.
- Missing observations are represented as gaps, never silently interpreted as inactivity.
- Expired screenshots do not invalidate retained text events or chunks.
- Screenshot expiry, user deletion, missing files, and write failures are separate lifecycle records rather than mutations of observations.
- SQLite is a disposable projection and never the only copy of canonical evidence.
- Every user-visible factual aggregate can identify supporting chunks/events.

## 10. Information Architecture

### Menu bar

- Recording state.
- Pause/resume.
- Open Chronicle.
- Start/end study.
- Current permission or health warning.

### Application lifecycle

- First launch and an explicit Dock launch open and focus the main window.
- A post-onboarding login launch restores the saved recording state without opening
  the main window; the menu-bar control remains visible.
- **Open Chronicle** from the menu bar focuses the existing main window and preserves
  its selected range, filters, and detail route.
- Closing the main window leaves the menu bar and capture coordinator running.
- Command-Q stops capture and exits without fabricating missed cadence events; the
  next launch records one factual quit gap.
- Logout/shutdown performs a best-effort durable flush and is reconciled as a gap on
  the next launch when needed.
- A second instance activates the first instance and never starts another capture
  coordinator.

### Home / report

- Date range.
- Observed time and coverage.
- Current day and daily average across calendar days containing at least one scheduled attempt.
- Activity-over-time visualization.
- Application/window breakdowns and capability-gated authorized-domain breakdowns.
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
- Personal/study mode, study countdown, exact expiry, and explicit extension.
- Launch at login.
- MCP connection status, active disclosure grants, revoke, and repair/uninstall.
- Storage/health.
- Export, delete evidence, and separate factory reset.

## 11. Success Criteria

- A first-time non-technical participant installs from a clean downloaded DMG and
  reaches a proven recording state without terminal assistance in ten minutes or
  less. Release qualification records elapsed time, assistance, and confusion; the
  gate fails if terminal help is needed or the limit is exceeded.
- After 15 minutes of ordinary work, the user can open a factual report and timeline containing supported five-minute chunks.
- An excluded or secure-input surface produces no persisted pixels or OCR text.
- A user can trace every factual report item to a chunk and its supporting events.
- Claude or Codex can query a selected time range and save a derived report without altering source evidence.
- The application remains useful with networking disabled.
- Quitting/relaunching does not create duplicate aggregation loops or duplicate chunks.
- Replacing an older installed MVP app with a newer DMG preserves compatible
  Application Support evidence, settings, receipts, and disclosure grants.
- Evidence deletion, factory reset, and screenshot expiry each produce an accurate account of what remains.
- A consultant can complete a bounded study using exports and local review without needing a cloud workspace.

## 12. Acceptance Examples

- **AE1 — Clean install:** On a Mac without Node or developer tools, the user installs Chronicle, grants permission, enables launch at login, and reaches the report view.
- **AE2 — Excluded app:** With a password manager foregrounded, Chronicle writes a skip-status event containing no title, OCR, or screenshot pixels from that application.
- **AE3 — Secure input:** When secure input is detected before capture, capture pauses before screenshot creation. If secure input begins after capture but before persistence, Chronicle discards the pixels before hash/OCR/encoding and persists only a coarse protected outcome.
- **AE4 — Factual chunk:** Ten 30-second interval-centered attempts at `09:00:15` through `09:04:45` produce one immutable `[09:00, 09:05)` chunk revision whose evidence-state seconds sum to 300 and which lists observed apps, separate presence coverage, transitions, OCR extracts, and evidence IDs without workflow or productivity claims.
- **AE5 — Gap honesty:** If permission is revoked for three minutes, the report shows a three-minute capture gap rather than inactivity.
- **AE6 — Screenshot expiry:** After a screenshot expires, its event and chunk remain searchable and clearly indicate that the local image is no longer retained.
- **AE7 — Evidence drill-down:** Selecting an application-duration segment opens the contributing chunks and observations.
- **AE8 — MCP immutability:** Claude creates a derived hypothesis citing three chunk IDs; attempts to alter those chunks or recording settings are unavailable through MCP.
- **AE9 — Offline operation:** With network access disabled, capture, OCR, chunk generation, report, timeline, search, export, and MCP queries continue working.
- **AE10 — Bounded study:** At the configured study end Chronicle auto-pauses; after a Mac sleeps through expiry, no wake capture occurs until the participant explicitly extends.
- **AE11 — Single owner:** Connecting Claude and Codex simultaneously does not start additional capture or chunk-generation loops.
- **AE12 — Deletion split:** Delete Evidence removes screenshots, events, chunks, indexes, derived artifacts, and evidence-bearing managed diagnostics/exports unless an export is explicitly preserved, while retaining settings and reversible registration/grant receipts; Factory Reset removes only matching registrations and then removes all Chronicle-managed receipts/configuration/evidence/diagnostics/exports while reporting external copies as outside its control.
- **AE13 — Unchanged coverage:** Ten unchanged interval-centered attempts at `:15`/`:45` create ten factual attempt records, one retained content artifact, and 300 seconds of evidence coverage.
- **AE14 — Projection rebuild:** After deleting or corrupting SQLite, Chronicle rebuilds report/search results from canonical journals without duplicate events or chunks.
- **AE15 — UI/MCP parity:** The same range and filters return identical chunk IDs, totals, and gaps in the Swift UI and MCP.
- **AE16 — Optional domain:** Without authorized browser context, Chronicle makes no domain claim and shows no misleading empty domain chart.
- **AE17 — Ambiguous active window:** When Chronicle cannot match one foreground window, it records the skip and persists no desktop or wrong-window pixels/OCR.
- **AE18 — Storage failure:** A failed canonical journal append creates no impossible canonical event, enters a visible paused/error health state, and is later represented by a retrospective storage-outage interval after recovery; Chronicle never claims the failed attempt itself was stored.
- **AE19 — Journal recovery:** After a crash following journal sync but before SQLite commit, restart projects the record exactly once from the saved journal cursor.
- **AE20 — MCP screenshot boundary:** An explicitly granted MCP client can report that an image is retained or expired but cannot return bytes or an arbitrary local path.
- **AE21 — Capture race:** If PID/window ID changes between preflight and post-capture validation, Chronicle discards pixels/OCR and records only a coarse protected/unavailable event.
- **AE22 — OCR failure:** If screenshot capture succeeds but OCR fails, the attempt remains captured with `ocr_state=failed`; coverage, screenshot policy, and health remain honest.
- **AE23 — Complete rebuild:** After an image expires and a derived report has two revisions, deleting SQLite and rebuilding restores the expired lifecycle state, both artifact revisions, and current chunk supersession.
- **AE24 — Corrupt canonical record:** A bad checksum on a complete journal line halts projection visibly at that record; Chronicle does not skip it and claim healthy state.
- **AE25 — Idle accounting:** Evidence-state seconds sum to 300, presence-state seconds sum only to captured coverage, and idle seconds never enter application-duration estimates.
- **AE26 — Deletion epoch:** An MCP client connected before Delete Evidence receives a stale-store error and cannot recreate artifacts in the new empty store generation.
- **AE27 — Registration conflict:** Factory Reset preserves and reports an MCP registration the user changed after Chronicle recorded its receipt.
- **AE28 — Restart state:** User-selected paused/recording mode and launch-at-login state survive restart without duplicate scheduled work.
- **AE29 — Managed paths:** Canonical records, exports, MCP responses, and diagnostics contain no absolute Chronicle-managed paths.
- **AE30 — Timezone travel:** UTC chunk identity remains stable while local display changes correctly across timezone travel and DST.
- **AE31 — Artifact race:** Two MCP processes revising from the same expected prior revision yield one success and one typed conflict; one canonical revision chain remains.
- **AE32 — Live-client deletion:** Delete Evidence waits for live MCP requests, increments the store generation, and leaves no descriptor or stale client capable of recreating deleted data.
- **AE33 — Chunk recovery:** Crashes around chunk append/sync/current-pointer/aggregation-watermark boundaries recover to exactly one active deterministic revision.
- **AE34 — Image-deletion recovery:** Crashes between delete request, unlink, and completion resume on startup to an accurate final screenshot lifecycle state.
- **AE35 — Transactional image write:** Failures and process kills between provisional image sync, observation append, image promotion, and lifecycle completion leave either recoverable pending evidence or no orphan; Chronicle never acknowledges a retained image that cannot be reconciled.
- **AE36 — Scoped MCP disclosure:** A client without an active grant receives no evidence. Time, content-class, expiry, response-size, and pagination limits are enforced, and immediate revoke blocks the next request without changing canonical evidence.
- **AE37 — Application lifecycle:** First/Dock launch, login launch, menu-bar Open, window close, Command-Q, logout/relaunch, and a second-instance launch exhibit the documented window/menu/capture behavior without duplicate ownership or synthetic missed ticks.

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
- Collect only aggregate idle seconds/state; never store input event values or streams.
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
- The no-Accessibility foreground-window selection algorithm must be proven on
  macOS 14, 15, and 26 across multiple windows, Spaces, full screen, Stage Manager,
  sheets, minimized windows, and multi-display use before capture contracts freeze.
- OCR can be inaccurate or expose sensitive visible text even when raw screenshots expire.
- Time estimates from periodic capture are approximate and must display coverage/confidence honestly.
- Bundling a Rust helper and MCP bridge requires careful signing of nested binaries.
- Canonical event/chunk/artifact records and SQLite projection can diverge without explicit journal-family cursors and rebuild tests.
- A foreground window can change during capture; a missing post-capture identity check could persist the wrong pixels.
- A polished dashboard can accidentally imply productivity conclusions that the evidence does not support.
- The consultant use case can drift toward employee surveillance without explicit product and policy boundaries.
- Same-user processes and backup software are inside the local-account trust
  boundary; restrictive permissions are not a defense against a compromised login
  account, and secure deletion of external backups is not promised.
- Checksums detect corruption, not hostile tampering. FileVault and a trusted local
  account remain deployment recommendations.
- Per-attempt filesystem synchronization protects acknowledged journal state from
  process/app crashes and normal OS restart semantics, not every storage-controller
  or sudden hardware-failure mode.

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

## 17. Technical Decisions Fixed for MVP Planning

- Swift owns TCC, exact-window ScreenCaptureKit capture, Vision OCR, lifecycle, onboarding, and UI.
- A narrow versioned C ABI links a Rust core into the signed app; every exported function contains panic boundaries and explicit memory ownership.
- Rust owns typed contracts, daily append-only journals, rebuildable SQLite projection, deterministic chunks, shared queries, export, retention state, derived artifacts, and health.
- A bundled Rust stdio MCP executable uses the same query service and is limited to evidence reads and immutable derived-artifact revision writes.
- Chronicle-managed files live under the macOS Application Support directory with only managed relative references in evidence.
- Public and Rust predecessor capture loops are not reused; both can expose unintended background pixels through full-display capture.
- macOS 14 is the minimum target and native Swift Charts is the MVP chart stack.
- MVP updates use manual DMG replacement with an explicit old-build-to-new-build
  compatibility test; an automatic updater is post-MVP.
- Legacy-data import and public signing credentials remain implementation/release decisions. Import is opt-in and never modifies predecessor data.
