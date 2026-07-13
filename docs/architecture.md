# Open Chronicle Architecture

Status: U1a scaffold. Runtime contracts and implementations intentionally begin in
later implementation units.

## Product boundary

Open Chronicle is a native macOS application backed by a reusable Rust evidence
core. The installed product must run without a source checkout, developer tools,
Node, Python, a shell setup step, or an LLM credential. The Swift application is the
only process allowed to own capture cadence and factual aggregation.

```text
SwiftUI macOS app
  - lifecycle, TCC, ScreenCaptureKit, Vision, controls, UI
  - one serialized Rust core handle
                  |
                  | versioned C ABI (static universal library)
                  v
chronicle-ffi -> chronicle-engine -> chronicle-store -> chronicle-domain
                         ^                 ^
                         |                 |
                 chronicle-mcp      chronicle-admin
```

`chronicle-mcp` is a separately signed, bundled stdio adapter. It links the same
domain, store, and query services but can neither capture nor aggregate. The
development-only `chronicle-admin` binary will prove journal verification, replay,
and recovery; it is not distributed inside the consumer app.

## Rust workspace

| Crate | Responsibility | May depend on |
| --- | --- | --- |
| `chronicle-domain` | Versioned factual contracts, IDs, configuration, health | Shared external value libraries only |
| `chronicle-store` | Canonical journals, managed artifacts, SQLite projection, locking | `chronicle-domain` |
| `chronicle-engine` | Ingestion, deterministic chunks, policy, shared queries | `chronicle-domain`, `chronicle-store` |
| `chronicle-ffi` | Panic-contained, explicitly owned app ABI | `chronicle-engine` |
| `chronicle-mcp` | Grant-bounded stdio protocol adapter | `chronicle-domain`, `chronicle-engine` |
| `chronicle-admin` | Development verification and repair commands | Domain, store, and engine |

Dependencies point inward. Swift must not issue SQL, MCP must not reimplement
queries, and no Rust crate may start an independent capture or aggregation loop.

## Durable data boundary

Daily append-only event/chunk journals and immutable derived-artifact revision files
will be canonical. SQLite is a disposable projection. Authoritative operational
files are `config.json`, `store-generation`, and
`receipts/agent-registrations.json`; cursors, watermarks, health snapshots, and
current pointers are recomputed. Screenshot paths are managed relative references,
never arbitrary or absolute paths.

The MVP trusts the signed Chronicle processes and the user's macOS login account.
Checksums detect corruption, not hostile tampering. A future `durable`
acknowledgement means the required file and directory synchronization completed for
process-crash and ordinary-restart recovery; it is not a promise against every
hardware-controller failure.

## Distribution boundary

- Minimum deployment target: macOS 14.
- Rust is pinned in `rust-toolchain.toml`; application builds link universal
  `aarch64-apple-darwin` and `x86_64-apple-darwin` static libraries.
- The app and nested `chronicle-mcp` executable are signed inside-out with Hardened
  Runtime before notarization and DMG stapling.
- `SMAppService.mainApp` provides launch at login, not crash supervision.
- Release builds keep `panic=unwind`; every future exported ABI function must catch
  panics before crossing into Swift and must document buffer ownership.
- The app is authoritative. A process-lifetime capture-owner lock prevents a second
  app instance from starting another coordinator.

## Deliberately excluded runtime paths

The predecessor capture loops, whole-display image APIs, shell screenshot commands,
runtime JavaScript/Python helpers, source-tree Swift helpers, MCP-owned summarizers,
and plaintext provider secrets are not part of this architecture. The CI guard in
`scripts/check-forbidden-runtime.sh` rejects their reintroduction into runtime
sources. The U1b exact-window ScreenCaptureKit spike is a separate proof and does
not adapt predecessor capture code.

