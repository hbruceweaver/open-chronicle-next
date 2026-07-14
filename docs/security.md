# Capture security boundary

Open Chronicle MVP trusts the signed Chronicle processes and the current macOS
login account. It is designed to prevent accidental capture and disclosure, not to
defend evidence from a compromised same-user process. FileVault remains a deployment
recommendation.

## Exact-window rule

The capture path resolves the frontmost process, selects the first eligible normal
on-screen Core Graphics window for that PID, and requires exactly one ScreenCaptureKit
window with the same PID and `CGWindowID`. It captures only with
`SCContentFilter(desktopIndependentWindow:)` and
`SCScreenshotManager.captureImage`. Missing or ambiguous matches are coarse gaps.
There is no whole-display, region, command-line screenshot, or Accessibility fallback.

One shared predicate runs immediately before and after capture. It checks pause and
study state, lock/sleep, Screen Recording permission, secure input, exact PID/window
identity, Chronicle-self protection, and application/title exclusions. A permitted
title may change while PID and window identity remain stable. A post-capture denial
ends the in-memory image lifetime before the coarse outcome is journaled; pixels never
reach normalization, hashing, OCR, encoding, or ingest.

## Bounded local processing

Approved images are normalized deterministically to sRGB BGRA8, a 2,560-pixel long
edge, and at most eight megapixels. Content hashes cover the normalized dimensions and
pixels. Vision OCR runs locally with a fixed request revision and deterministic reading
order. HEIC encoding uses an in-memory quality/downscale ladder and never passes more
than 4 MiB to Rust. It creates no temporary screenshot file. If a bounded encoding
cannot be produced, the captured/OCR fact may still be stored with no image.

Only Rust writes managed evidence files. Swift passes an immutable byte copy through
the versioned C ABI; Rust owns provisional write, journal append, promotion, lifecycle
completion, recovery, and acknowledgement. Dedupe state advances only after a
journal-durable acknowledgement, including projection-pending acknowledgements.

## Onboarding proof

Normal Chronicle-self exclusion is unconditional. Onboarding may mint one in-memory
token scoped to a single test-window ID and the SHA-256 digest of fixed synthetic text.
The token is consumed on the first attempt, including a failed or mismatched attempt,
and does not survive relaunch. Proof pixels and OCR are discarded in memory. Success
persists nothing; failure records only the coarse `chronicle-self` outcome.

## Qualification status

CI exercises injected window, environment, capture, Vision, encoder, and ingest fakes,
plus a real Swift-to-Rust contract round trip. The supported release still requires
packaged runtime qualification on macOS 14, 15, and 26 across multiple displays and
windows, full-screen Spaces, Stage Manager, sheets/popovers, occlusion/minimization,
and Retina scaling. No untested row authorizes a broader capture fallback.
