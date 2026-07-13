# macOS foreground-window resolution spike

This package tests the U1b feasibility assumption without persisting a screenshot:

1. Read the frontmost application PID from `NSWorkspace`.
2. Preserve Core Graphics' front-to-back ordering and select the first eligible
   on-screen, normal-layer window owned by that PID.
3. Require its window number to map to exactly one ScreenCaptureKit `SCWindow`.
4. Capture only that desktop-independent window into memory.
5. Print content-free identity/dimension proof and exit without encoding or writing
   image bytes.

Build and test:

```sh
rtk swift test --package-path spikes/macos-window-resolution
```

Run from a host that already has Screen Recording permission:

```sh
rtk swift run --package-path spikes/macos-window-resolution window-resolution-spike
```

The probe intentionally does not request permission. A missing permission is a
reported matrix result, not permission to broaden capture or store pixels.
