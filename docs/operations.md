# Installed application operations

## Install and update

1. Download the qualified, published release DMG through a browser so macOS records
   quarantine. A GitHub draft prerelease candidate is not yet a public release.
2. Verify the published SHA-256 checksum from the release page and match it to the
   qualified candidate record.
3. Mount the DMG and drag `Open Chronicle.app` to `/Applications`.
4. Eject the DMG and launch the copy in `/Applications`, not the mounted image.
5. Complete onboarding and the Chronicle-owned safe capture proof.

MVP updates are manual DMG replacements. Quit Chronicle, replace the application in
`/Applications`, and relaunch it. Do not delete Application Support data during an
ordinary app replacement. Automatic update delivery is not implemented.

Before a draft candidate is manually promoted, test replacement from the previous
supported build and prove that compatible evidence, settings, agent receipts,
disclosure grants, and launch-at-login preference remain intact. Qualification and
promotion must reuse the exact candidate bytes; never replace an asset under an
existing tag.

## Runtime ownership

The app owns capture, OCR, aggregation, retention, and the shared Rust service. The
only bundled helper is `Contents/Helpers/chronicle-mcp`; supported agent registration
must point to that installed helper or an app-created MCPB copy, never to the source
checkout or `target/`. The app needs no Node, npm, Python, Rust, Xcode, or network
connection for its local runtime.

Launch at login uses `SMAppService.mainApp` for convenience. It is not a crash
supervisor. A fatal app crash stops recording until relaunch or the next login, and
the missing interval must remain an explicit factual gap.

## Diagnostics

For an artifact before installation:

```sh
rtk ./scripts/verify-bundle.sh 'dist/Open Chronicle.app'
rtk ./scripts/smoke-installed-app.sh 'dist/Open Chronicle.dmg'
```

For a signed release candidate:

```sh
rtk ./scripts/verify-release.sh 'dist/Open Chronicle.dmg'
```

The candidate must remain draft/prerelease until the separate qualification matrix
and approval record are complete. Release workflow success alone is not promotion
authority.

Preserve the exact command output, DMG checksum, app/helper `lipo -archs`, app/helper
`otool -L`, macOS build, client versions, and Chronicle health output. Do not attach
real screenshots, OCR, local databases, credentials, or other user evidence to an
issue or CI artifact.

When local operation fails, distinguish bundle failure from runtime state:

- Bundle verification failure: rebuild; do not bypass architecture, dependency,
  privacy-manifest, signing, notarization, or Gatekeeper checks.
- Screen Recording denial/revocation: repair permission in System Settings, then run
  the app-owned proof again. Never fall back to whole-display or shell capture.
- Agent connection failure: keep the grant default-deny, show the installed helper
  path and version-specific guided registration fallback, and avoid editing unrelated
  client configuration.
- Storage/projection failure: keep capture acknowledgement honest, use supported
  recovery/repair UI, and retain journal diagnostics. Never reinterpret a storage
  outage as inactivity.
- Crash: relaunch and confirm the gap plus recovery state; do not describe launch at
  login as supervision.

## Deletion boundary

Screenshot expiry does not delete OCR, factual events, or chunks. Delete Evidence
and Factory Reset are separate confirmed operations. Factory Reset removes only
matching Chronicle-owned agent registrations. FileVault is recommended; Chronicle
does not claim deletion from backups or protection from a compromised same-user
process.
