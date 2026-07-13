# Open Chronicle Product Context

## Purpose

Open Chronicle is an easy-to-install work observability system. It gathers a factual, private record of how work happens on a computer, organizes that evidence into understandable time blocks, and makes it available to the user, Claude, Codex, or a consultant for later analysis.

The near-term product is not an automatic workflow generator. Chronicle is the evidence layer; people and more capable models are the analyst layer. Its job is to make work activity clear, queryable, portable, and trustworthy enough that later analysis can identify wasted effort, recurring friction, and practical automation opportunities.

## Product Thesis

Most AI implementation work starts with incomplete interviews and generic advice. People cannot reliably reconstruct where their time went, which systems they moved between, or where repetitive coordination occurred. Existing activity trackers usually optimize for productivity scoring; coding trackers cover only development; screen-memory products optimize for recall.

Chronicle instead creates an evidence-backed diagnostic substrate for work:

```text
Observed computer activity
  -> factual events
  -> factual five-minute chunks
  -> report and searchable timeline
  -> collaborative Claude/Codex investigation
  -> optional later recommendations and implementation work
```

Positioning: Rewind helped users remember what happened. Chronicle helps users understand how work happens.

## Intended Users and Modes

### Personal

A user installs Chronicle on their own Mac, keeps it running automatically, and uses its report, timeline, search, exports, and MCP integration. Recording, retention, and exclusions remain user-controlled.

### Consultant-assisted study

A consultant helps a client install Chronicle and understand its privacy controls. The client runs a time-boxed study, typically three to five working days, and reviews the factual outputs with the consultant weekly for approximately one month. The resulting diagnostic work can lead to separate automation, connector, skill, scheduled-job, or process-redesign engagements.

### Future team and SaaS modes

Organizations may later deploy Chronicle to work devices and automatically synchronize policy-approved derived data to a client-owned cloud workspace. Visibility is policy-configurable, disclosed to every observed user, and audited. Personal self-serve SaaS may later provide hosted reports and analysis. These are not MVP capabilities.

## Commercial Motion

Chronicle can serve as a low-friction trigger offer for an AI implementation consultancy:

1. Kickoff, installation, privacy setup, and expectations.
2. A bounded observation period using the client-owned app.
3. Weekly evidence reviews covering time allocation, repeated patterns, and operational friction.
4. A final evidence-backed AI implementation roadmap.
5. Optional follow-on services for automation, Claude skills, connectors, loops, scheduled jobs, and workflow redesign.

The diagnostic engagement and the self-serve desktop product use the same evidence engine. The consultancy is a service wrapper, not a separate surveillance product.

## Product Principles

1. **Facts before inference.** Canonical outputs contain observations and deterministic measurements, not productivity judgments, workflow claims, or automation recommendations.
2. **Evidence remains inspectable.** Every aggregate or later model conclusion identifies the observations that support it.
3. **Raw evidence is immutable.** Claude, Codex, users, and consultants may add derived artifacts without rewriting source observations.
4. **Local-first trust.** Screenshots remain local in MVP; OCR runs on-device; the product remains useful without a cloud account.
5. **Visible user control.** Recording state, exclusions, retention, deletion, and access policy are understandable and easy to change.
6. **Open analysis plane.** MCP and documented exports make Claude/Codex replaceable consumers rather than forcing analysis through one proprietary assistant.
7. **No hidden productivity scoring.** Time spent is evidence, not a measure of employee value or performance.
8. **Progressive productization.** Prove capture, evidence quality, reporting, and timeline UX before adding semantic project inference, cloud administration, or automation generation.

## Canonical Data Layers

### Raw capture

- Local screenshot.
- Raw OCR.
- Timestamp and capture status.
- Active application, process, and window title.
- Permitted domain context when available.
- Privacy decision, gap, or error metadata.

### Canonical evidence

- Versioned immutable events with stable identifiers.
- Source provenance and confidence.
- Portable structured output in addition to a query index.
- No model interpretation.

### Factual aggregation

- Persisted five-minute chunks.
- Time coverage and gaps.
- Applications/windows observed and deterministic duration estimates.
- Transitions, OCR changes, factual extracts, and supporting event IDs.
- Longer windows and natural sessions derived from the base chunks.

### Derived analysis

- User or consultant annotations.
- Claude/Codex analyses and reports.
- Hypotheses, inefficiency signals, workflow candidates, and recommendations.
- Model, author, timestamp, confidence, and supporting evidence references.

Derived artifacts can be regenerated or deleted without changing canonical evidence.

## Reference Products

### Rewind.ai / Limitless

Keep the near-zero-friction installation, always-on personal mode, local screen history, fast keyword search, visual timeline, exact historical moments, and clear pause/exclusion/deletion controls. Avoid lifetime-recording scope, continuous audio, hardware detours, a closed assistant, an opaque datastore, and cloud dependence for local value.

### Recall.ai

Adopt the infrastructure mindset: the capture and evidence plane is a stable platform; reports, consultant workflows, UI surfaces, and AI analysis are consumers. Do not turn Chronicle into a generic capture SDK before the end-user product is useful.

### WakaTime

Use WakaTime's information hierarchy as a dashboard reference:

- Date-range selection and top-level activity totals.
- Project/category trends over time.
- Parallel timeline bands showing when activity occurred.
- Breakdowns by useful dimensions.
- Detailed cards and drill-downs.

For MVP, Chronicle uses factual dimensions such as applications, windows, domains, observed time, sessions, transitions, and evidence coverage. Semantic project/category assignment is explicitly post-MVP; later models may propose assignments that users can correct.

## Existing Implementations

### `Screenata/open-chronicle`

The public Swift/Node proof of concept supplies the strongest product shell:

- SwiftUI menu-bar application and floating window.
- First-run onboarding and screen-recording permission UX.
- Capture controls, exclusions, settings, data deletion, and memory/timeline concepts.
- Claude and Codex detection and MCP-registration concepts.

It is not directly productizable as a DMG. It depends on a source checkout, Node/npm, runtime TypeScript, plaintext environment secrets, and deprecated whole-desktop capture. Its MCP process also owns summarization work, creating lifecycle and duplication risk.

### Second Brain Rust Chronicle

The Rust implementation supplies the stronger systems substrate:

- Versioned schemas, storage helpers, event timeline, and SQLite indexing.
- Config loading, pause/resume, health, retention, replay, import, and proof tooling.
- Secure-input and foreground privacy preflight.
- Window inventory and factual app-context derivation.
- Untrusted-input wrapping, validation, deterministic fallback, and hardened model execution.
- Contract and behavior tests.

It is not directly productizable as a desktop app. It is CLI-first, uses source-tree Swift helper paths, assumes developer tooling, and contains cadence/storage mechanics that need redesign for a consumer product.

### Target combination

- Preserve and adapt the public repository's SwiftUI interaction patterns and onboarding.
- Extract and redesign the Rust substrate into a packaged engine with a documented evidence contract.
- Rebuild capture around ScreenCaptureKit and on-device Vision OCR.
- Replace the Node/source-checkout MCP runtime with a bundled query bridge.
- Build the report and evidence timeline around factual events and five-minute chunks.

## MVP Boundary

MVP is a local-first macOS product for personal and consultant-assisted use. It includes a self-contained app/DMG, onboarding, visible capture controls, privacy-safe active-window capture, local OCR, immutable factual events, five-minute chunks, report-led home, searchable timeline, MCP, derived write-back, export, deletion, health, and retention.

MVP excludes cloud sync, team administration, Windows, audio recording, semantic project inference, automatic workflow extraction, built-in automation recommendations, autonomous actions, and cloud screenshot storage.

## Longer-term Direction

- AI-proposed, user-correctable project/category assignment.
- Cloud synchronization of policy-approved derived data.
- Client-owned consultancy and team workspaces.
- Policy-configurable organizational visibility with mandatory disclosure and audit.
- Windows capture and shell around the shared Rust core.
- Optional explicit meeting mode and transcript ingestion expansion.
- Proactive analysis, workflow candidates, skill generation, and safe implementation workflows.
- Aggregate team diagnostics that do not become hidden productivity scoring.

## Durable Decisions

- Five-minute factual chunks are the canonical aggregation unit.
- Report-led home and searchable timeline are both primary surfaces.
- MCP is required in the first release.
- MCP reads evidence and writes derived artifacts only.
- No native audio in MVP.
- Semantic project assignment is not an MVP feature.
- Screenshots remain local in MVP.
- Personal recording can launch automatically; consultant studies are time-boxed.
- Cloud and organizational policy depend on deployment context and are later product layers.

