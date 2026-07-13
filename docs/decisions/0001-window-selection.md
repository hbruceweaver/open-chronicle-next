# Decision 0001: foreground-window selection feasibility

- Status: compile-proven; macOS 26 single-window runtime proven; broader matrix pending
- Date: 2026-07-13
- Scope: U1b capture feasibility gate

## Candidate algorithm

Open Chronicle can avoid Accessibility permission if the following public metadata
chain is reliable enough on each supported macOS version:

1. `NSWorkspace.shared.frontmostApplication` supplies the frontmost owner PID.
2. `CGWindowListCopyWindowInfo` supplies front-to-back on-screen window metadata.
3. Chronicle selects the first normal-layer, positive-size, nontransparent window
   owned by that PID.
4. The selected Core Graphics window number must match exactly one `SCWindow`.
5. `SCContentFilter(desktopIndependentWindow:)` and `SCScreenshotManager` capture
   that window only. Any missing or ambiguous mapping becomes a factual gap.

The executable in `spikes/macos-window-resolution` captures into memory and prints
only window/pixel dimensions and counts. It never persists image bytes or titles.

## Required runtime matrix

| Scenario | macOS 14 | macOS 15 | macOS 26 |
| --- | --- | --- | --- |
| One normal window | Pending | Pending | Pass on 26.2 arm64 |
| Multiple windows in one process | Pending | Pending | Pending |
| Multiple displays | Pending | Pending | Pending |
| Full-screen Space | Pending | Pending | Pending |
| Stage Manager | Pending | Pending | Pending |
| Sheet or popover foreground | Pending | Pending | Pending |
| Minimized/occluded window | Pending | Pending | Pending |
| Retina scale | Pending | Pending | Partial: 2x capture produced 2560x2168 capped output |

## Current runtime evidence

On macOS 26.2 (25C56), arm64, with Screen Recording already granted to the host,
the probe selected window ID 928 from one eligible window owned by the frontmost
application, matched exactly one `SCWindow`, and captured a 2560x2168 image in memory.
It reported `persisted_image: false` and wrote no screenshot. The selection unit tests
also passed. This proves the basic public-API chain on the current machine; it does
not satisfy the remaining macOS/window-manager matrix rows.

U2 may freeze the skip/gap schema, but production capture implementation is blocked
until the available-hardware rows prove this algorithm or the PRD permission contract
is deliberately revised. A failed row never authorizes whole-display fallback.
