# Source Provenance and Reuse Policy

Open Chronicle Next combines product and systems concepts from two predecessor
implementations. U1a copies no runtime source code from either predecessor. This
record fixes the reviewed source snapshots and the rules for later adaptation.

## Reviewed source snapshots

| Source | Reviewed commit | License/ownership at that commit | Role |
| --- | --- | --- | --- |
| [`Screenata/open-chronicle`](https://github.com/Screenata/open-chronicle) | `80437271e509c6dd2eba7be7c216e21c76aa41c5` | MIT; notice preserved in `THIRD_PARTY_NOTICES.md` | Public product-shell proof of concept |
| Local `second-brain` repository, `chronicle/` subtree | `17a6e875e39f104f15d80af8d0140f7e2657305d` | Project-owner-controlled internal source; no top-level public license file was present in the reviewed checkout | Rust systems-substrate proof |

The public source was reviewed from
`/Users/hbruceweaver/Projects/oss/open-chronicle`. The internal source was reviewed
from `/Users/hbruceweaver/Projects/second-brain`. These local paths are review
metadata only and must never become runtime paths or canonical evidence fields.

## Reuse classification

### Adapt as product or systems concepts

- Public project: SwiftUI menu-bar/floating-window interaction patterns, onboarding
  structure, permission education, capture controls, exclusions/settings layout,
  deletion UX, and Claude/Codex registration intent.
- Internal Rust project: versioned schema discipline, configuration merge concepts,
  privacy preflight, health, replay, retention preview, untrusted-input treatment,
  SQLite projection, and contract-test patterns.

Concept adaptation means new code is written against the current PRD and contracts.
When later work substantively adapts an identifiable source file, the implementer
must add a source-path/commit note to that file or this document and preserve every
required license notice.

### Rewrite for this product

- Exact-window ScreenCaptureKit capture and the shared pre/post privacy predicate.
- Canonical append-only persistence, transactional screenshot lifecycle, and
  deterministic five-minute chunking.
- The C ABI, shared query service, bundled Rust MCP adapter, diagnostic report,
  searchable timeline, and signed/notarized packaging.

### Explicitly reject

- Both predecessor whole-display capture loops and any desktop fallback.
- Deprecated Core Graphics desktop capture and shell screenshot commands.
- Node/TypeScript MCP execution, first-run package installation, and MCP-owned model
  summarization loops.
- Source-tree helper discovery, developer-tool runtime assumptions, absolute local
  paths, plaintext provider secrets, and mandatory LLM configuration.
- Full-file canonical rewrites, stale locking behavior, and model-written prose in
  factual evidence.

## Review procedure for future reuse

1. Pin and record the source repository, exact commit, source path, and applicable
   license before copying or adapting implementation code.
2. Classify the change as concept-only, adapted implementation, or verbatim reuse.
3. Prefer the smallest PRD-conforming adaptation and add focused contract coverage.
4. Update `THIRD_PARTY_NOTICES.md` and per-file attribution when the license or
   amount of copied material requires it.
5. Run the forbidden-runtime guard and confirm that no source-checkout path, real
   user evidence, credential, or rejected runtime entered the repository.

