# Distribution testing

## Automated bundle contract

`scripts/verify-bundle.sh` enforces the local facts that can be established without
launching the app:

- bundle ID `com.screenata.openchronicle`, application package type, executable
  name, Screen Recording usage description, privacy manifest, macOS 14 minimum, and
  plist version/build values matching the embedded provenance;
- an exact bundle-layout allowlist: the app executable, one
  `Contents/Helpers/chronicle-mcp`, required plist/privacy/provenance files, and
  signature metadata only for a signed app;
- every file is inspected: unexpected executable or Mach-O content, frameworks,
  renamed runtimes, symlinks, source, static archives, headers, `.git`, and `target`
  payloads are rejected;
- every allowed Mach-O has exactly `arm64` and `x86_64` slices, an
  `LC_BUILD_VERSION` minimum of 14.0 in each slice, and only `/System/Library` and
  `/usr/lib` dynamic dependencies;
- provenance has a valid commit/fingerprint/mode/version/build shape; current-tree
  packaging additionally requires an exact current commit, fingerprint, and dirty
  state match;
- either an explicitly unsigned/ad-hoc development state or valid inside-out
  Developer ID signatures with Hardened Runtime and the exact configured leaf
  signer and Team ID across the app, main executable, helper, and DMG.

`scripts/smoke-installed-app.sh` adds a non-GUI DMG/copy/runtime proof. It verifies
the copied bundle and completes MCP initialization plus a 16-tool listing from the
copied helper. The preserved directory printed by the script contains the JSONL
response and stderr log.

## Local unsigned proof

```sh
rtk ./scripts/build-app.sh --configuration Release --unsigned
rtk ./scripts/package-dmg.sh --unsigned
rtk ./scripts/smoke-installed-app.sh 'dist/Open Chronicle.dmg'
rtk ./scripts/probe-release-packaging.sh
rtk ./scripts/probe-release-packaging.sh --static-only
```

Record the command output, `shasum -a 256`, `lipo -archs`, deployment targets, and
`otool -L` results with the tested commit. The probe proves refusal of dirty signed
input, tag/version mismatch, stale provenance, extra Mach-O/layout content, mutable
action references, dSYM publication, replacement flags, and a non-draft tag path.
It also rejects malformed or mismatched checksum sidecars. Static-only mode does
not require a built app. This is development evidence only.

## Clean-machine release matrix

Every cell starts as **not run**. Attach dated evidence rather than replacing an
untested cell with an assumption.

- macOS 14: Apple Silicon; Intel where supported hardware is available
- macOS 15: Apple Silicon; Intel where supported hardware is available
- macOS 26: Apple Silicon; Intel where supported hardware is available
- browser-downloaded quarantine and Gatekeeper
- no Xcode, Command Line Tools, Rust, Node, npm, or Python installed
- offline capture, report, timeline, search, export, and MCP after installation
- agents present and absent; supported Claude Desktop, Claude Code, Codex desktop,
  and Codex CLI version/registration combinations
- Screen Recording permission not determined, denied, granted, and revoked
- personal and time-bounded study modes; sleep/wake and expiry
- launch at login and honest post-crash gap behavior
- manual older-DMG-to-newer-DMG replacement with evidence, settings, receipts,
  grants, and login preference preserved
- safe capture through journal, chunk, report, MCP, relaunch, and no duplicate chunk
- eight-hour 30-second live soak, 24-hour high-change storage simulation, and
  86,400-attempt metadata corpus budgets

Use this evidence header for each run:

```text
Date/time UTC:
Commit/tag:
DMG SHA-256:
Hardware/architecture:
macOS build:
Quarantine source:
Developer tools present:
Network state:
Agent/version:
Result: pass | fail | blocked | not run
Elapsed install-to-recording:
Assistance/terminal use:
Evidence/log location:
Notes:
```

The first-time participant gate fails when download-to-proven-recording exceeds ten
minutes or requires terminal help. A local developer run cannot satisfy this gate.

## Signed release proof

`scripts/verify-release.sh` is intentionally signed-only. It requires matching
checksum and provenance sidecars plus successful DMG signature, stapler,
Gatekeeper, exact-content, and nested bundle verification. Its mount cleanup uses
bounded normal detach retries followed by a forced detach; a primary verification
failure is preserved while any cleanup failure is surfaced. It does not substitute
for the clean-machine matrix or the end-to-end capture flow. The checksum sidecar
must be exactly one `<64 lowercase hex><two spaces><DMG basename>` record; the
computed digest is compared directly before signature checks.
