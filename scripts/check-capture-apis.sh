#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
production="$root/macos/OpenChronicle"

# Whitespace is removed before matching so split Swift and Objective-C calls are
# covered identically. Keep the Objective-C selector spellings explicit: merely
# scanning .m/.mm/.h files is not sufficient when their API names differ.
forbidden_capture='CGWindowListCreateImage|screencapture|SCContentFilter\(display:|initWithDisplay:|SCScreenshotManager\.captureImage\(in:|captureImageInRect:|SCStream(\(|\*|alloc|new)|CGDisplayCreateImage|CGDisplayStream|CGEventTapCreate|CGEvent\.tapCreate|NSEvent\.add(Global|Local)MonitorForEvents|NSEventadd(Global|Local)MonitorForEventsMatchingMask:'
forbidden_pixel_persistence='CGImageDestinationCreateWithURL|\.write\(to:|writeTo(File|URL):|temporaryDirectory|NSTemporaryDirectory'
forbidden="$forbidden_capture|$forbidden_pixel_persistence"

rejects() {
  printf '%s' "$1" | grep -E "$forbidden" >/dev/null
}

if [ "${1:-}" = "--self-test" ]; then
  for sample in \
    '[[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]]' \
    '[SCScreenshotManager captureImageInRect:rect completionHandler:handler]' \
    '[[SCStream alloc] initWithFilter:filter configuration:config delegate:nil]' \
    '[NSEvent addGlobalMonitorForEventsMatchingMask:mask handler:handler]' \
    '[data writeToFile:path atomically:YES]'
  do
    compact_sample=$(printf '%s' "$sample" | tr -d '[:space:]')
    if ! rejects "$compact_sample"; then
      echo "capture API guard self-test missed: $sample" >&2
      exit 1
    fi
  done
  allowed='SCScreenshotManager.captureImage(contentFilter:filter,configuration:configuration)SCContentFilter(desktopIndependentWindow:window)'
  if rejects "$allowed"; then
    echo "capture API guard self-test rejected the exact-window path" >&2
    exit 1
  fi
  echo "capture API guard self-test passed"
  exit 0
fi

search() {
  pattern=$1
  if command -v rg >/dev/null 2>&1; then
    rg -n \
      --glob '*.swift' \
      --glob '*.m' \
      --glob '*.mm' \
      --glob '*.h' \
      "$pattern" "$production"
  else
    grep -R -n -E \
      --include='*.swift' \
      --include='*.m' \
      --include='*.mm' \
      --include='*.h' \
      "$pattern" "$production"
  fi
}

compact=$(find "$production" -type f \( \
  -name '*.swift' -o \
  -name '*.m' -o \
  -name '*.mm' -o \
  -name '*.h' \
\) -exec sed 's://.*$::' {} \; | tr -d '[:space:]')
if printf '%s' "$compact" | grep -E "$forbidden_capture" >/dev/null; then
  echo "forbidden broad or legacy capture API found" >&2
  exit 1
fi

# Package assembly may legitimately use temporary files. Pixel-bearing capture and
# onboarding proof code may not: keep that stricter policy scoped to the only
# production paths that can receive captured images.
pixel_pipeline_compact=$(find \
  "$production/Capture" \
  "$production/Onboarding/CaptureProofService.swift" \
  -type f \( \
    -name '*.swift' -o \
    -name '*.m' -o \
    -name '*.mm' -o \
    -name '*.h' \
  \) -exec sed 's://.*$::' {} \; | tr -d '[:space:]')
if printf '%s' "$pixel_pipeline_compact" | grep -E "$forbidden_pixel_persistence" >/dev/null; then
  echo "forbidden captured-pixel persistence API found" >&2
  exit 1
fi

if ! search 'SCScreenshotManager\.captureImage' >/dev/null; then
  echo "exact-window screenshot API is missing" >&2
  exit 1
fi
if ! search 'desktopIndependentWindow:|initWithDesktopIndependentWindow:' >/dev/null; then
  echo "desktop-independent exact-window filter is missing" >&2
  exit 1
fi

echo "capture API guard passed"
