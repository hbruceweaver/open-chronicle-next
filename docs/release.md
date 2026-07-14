# Release process

Open Chronicle has two distribution modes with deliberately different claims.

- The unsigned mode produces a universal development app and drag-install DMG. A
  dirty source tree is allowed, but that fact is recorded in the bundle provenance
  and the result cannot claim a release tag. It proves bundle construction and
  local helper execution, but it is not a release and does not prove Gatekeeper,
  quarantine, notarization, or clean-machine behavior.
- The signed mode requires a Developer ID Application identity and a `notarytool`
  keychain profile. It signs nested code inside-out, signs and notarizes the DMG,
  staples the ticket, and runs strict release verification. It also requires a
  clean tree and a stable SemVer tag that resolves exactly to `HEAD`, whether the
  tag is annotated or lightweight. Missing credentials or mismatched provenance
  are hard failures, never an implicit unsigned fallback.

## Local unsigned artifact

Prerequisites: Xcode with the macOS 14 SDK, the Rust toolchain pinned in
`rust-toolchain.toml`, both Apple Rust targets, and enough free disk for two Rust
target trees plus Xcode products.

```sh
rtk ./scripts/build-app.sh --configuration Release --unsigned
rtk ./scripts/package-dmg.sh --unsigned
rtk ./scripts/verify-bundle.sh 'dist/Open Chronicle.app' --unsigned
rtk ./scripts/smoke-installed-app.sh 'dist/Open Chronicle.dmg'
```

Outputs:

- `dist/Open Chronicle.app`
- `dist/Open Chronicle.app.dSYM` when Xcode emits one
- `dist/Open Chronicle.dmg`
- `dist/Open Chronicle.dmg.sha256`
- `dist/Open Chronicle.provenance.json`

The app embeds the same provenance as
`Contents/Resources/release-provenance.json`. It binds the artifact to its commit,
source-tree fingerprint, dirty state, tag, version, and build number. Packaging
recomputes those facts and refuses a stale or mismatched app. dSYMs may be sent only
to an approved access-controlled external symbol service or placed manually in
restricted storage. Never attach dSYMs to a GitHub release or upload them as a
GitHub Actions artifact.

The smoke script mounts the DMG without opening Finder, copies the app into a
disposable Applications-shaped directory under `build/`, re-verifies the copied
bundle, and initializes/lists tools from the copied `chronicle-mcp`. It intentionally
does not launch the GUI, alter `/Applications`, request Screen Recording permission,
or claim a quarantined or network-disabled result.

## Credentialed releases

Credentialed releases are workflow-only for MVP. Do not assemble a signed release
by copying the low-level signing commands into a local shell: a linear command list
cannot guarantee that certificate files, API keys, and temporary keychains are
destroyed on every failure or signal path.

Use the protected GitHub `release` environment described below. Its executable
workflow owns the trap-backed cleanup and enforces this sequence: prepare the exact
unsigned candidate with no credentials present; sign the helper, app, and DMG in a
short-lived signing-keychain phase; notarize through a separate short-lived API-key
phase; staple; atomically regenerate the checksum; and verify the final bytes. The
individual signing scripts are workflow implementation details, not a supported
local release interface.

The tag supplies `MARKETING_VERSION` (`v1.2.3` becomes `1.2.3`). The explicitly
provided positive integer supplies `CURRENT_PROJECT_VERSION`; GitHub Actions uses
its deterministic `GITHUB_RUN_NUMBER`. The build checks both values in the final
`Info.plist` through provenance verification before notarization.

`build-app.sh` deliberately has no combined build-and-sign mode. The workflow's
signing-only step signs `Contents/Helpers/chronicle-mcp` before the app and enables
Hardened Runtime on each Mach-O. The checksum emitted by `--sign-only` is provisional;
stapling changes the DMG, so only the atomically regenerated post-staple checksum
may be verified or uploaded. `verify-release.sh` validates the final post-staple
checksum, provenance sidecar, DMG
signature, notarization ticket, Gatekeeper assessment, exact DMG contents, nested
signatures, Hardened Runtime, architectures, deployment targets, and dynamic
library paths. Signed verification fails unless the main executable, helper, app,
and DMG all report the exact configured Developer ID leaf authority and
`TeamIdentifier`.

## GitHub release environment

Manual workflow dispatch runs only the unsigned proof and uploads an Actions
artifact. Configure `MACOS_DEVELOPER_ID_APPLICATION` and the ten-character
`MACOS_TEAM_ID` as repository variables. A `v*` tag runs the protected `release`
environment and requires these secrets:

- `MACOS_CERTIFICATE_P12`: base64-encoded Developer ID Application PKCS#12
- `MACOS_CERTIFICATE_PASSWORD`
- `MACOS_BUILD_KEYCHAIN_PASSWORD`
- `APPLE_API_KEY_P8`
- `APPLE_API_KEY_ID`
- `APPLE_API_ISSUER_ID`

The credentialed build job has read-only repository permission and completes the
unsigned Release build/package proof before creating credentials. It deletes the
P12 immediately after import, destroys the signing keychain after the signed DMG,
deletes the P8 after credential storage, and destroys the notary keychain after
submission, all before SBOM generation. Only candidate assets cross into a separate
publication job; that job has no signing/notary secrets and uses only the pinned
download action plus GitHub CLI and system tools.

The tag workflow refuses an existing release and any ambiguous release lookup. It
never replaces an asset and never uses `--clobber`. After the third-party SBOM step,
it rechecks the exact DMG checksum and runs `verify-release.sh` immediately before
candidate creation. The only automated outcome is a **draft prerelease candidate**
containing the notarized DMG, checksum, provenance, SPDX JSON SBOM, and generated
notes. The workflow never publishes a public final release. dSYMs never enter the
workflow artifact or GitHub release path.

All third-party actions are pinned to immutable commit SHAs, resolved from their
official repositories on 2026-07-14:

- [`actions/checkout` commit `93cb6efe18208431cddfb8368fd83d5badbf9bfd`](https://github.com/actions/checkout/commit/93cb6efe18208431cddfb8368fd83d5badbf9bfd)
- [`dtolnay/rust-toolchain` commit `fa04a1451ff1842e2626ccb99004d0195b455a88`](https://github.com/dtolnay/rust-toolchain/commit/fa04a1451ff1842e2626ccb99004d0195b455a88)
- [`Swatinem/rust-cache` commit `e18b497796c12c097a38f9edb9d0641fb99eee32`](https://github.com/Swatinem/rust-cache/commit/e18b497796c12c097a38f9edb9d0641fb99eee32)
- [`anchore/sbom-action` commit `e22c389904149dbc22b58101806040fa8d37a610`](https://github.com/anchore/sbom-action/commit/e22c389904149dbc22b58101806040fa8d37a610)
- [`actions/upload-artifact` commit `ea165f8d65b6e75b540449e92b4886f43607fa02`](https://github.com/actions/upload-artifact/commit/ea165f8d65b6e75b540449e92b4886f43607fa02)
- [`actions/download-artifact` commit `634f93cb2916e3fdff6788551b99b062d0335ce0`](https://github.com/actions/download-artifact/commit/634f93cb2916e3fdff6788551b99b062d0335ce0)

## Qualification and promotion

Treat the draft prerelease as a write-once candidate. Do not replace its files or
rebuild under the same tag. Download the draft asset through an authenticated
browser so qualification exercises GitHub's asset delivery path and macOS
quarantine. Complete the clean-machine matrix in `docs/testing.md` and prove that
the downloaded SHA-256 is byte-for-byte identical to the recorded candidate.
Record the tag, commit, build number, checksum, SBOM, matrix evidence, and approvals.

Only after those gates pass may an authorized maintainer promote the existing
candidate by changing release metadata from draft/prerelease to published/stable.
Promotion is a separate manual operation outside this workflow; it must reuse the
existing assets exactly and must not upload replacements. A failed candidate gets
a new commit, tag, build number, and draft candidate.

## Public-release gates outside automation

Do not describe an artifact as generally released until the qualification and
promotion process above is complete, the final icon/artwork and privacy/legal review
are approved, and a browser-downloaded quarantined copy passes the supported
hardware and OS matrix. Manual old-to-new DMG replacement is the MVP update
mechanism; automatic updates are not implemented.
