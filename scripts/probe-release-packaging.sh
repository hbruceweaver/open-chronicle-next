#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
app="$root/dist/Open Chronicle.app"
workflow="$root/.github/workflows/release.yml"
probe_mode=full

fail() {
    echo "probe-release-packaging: $*" >&2
    exit 1
}

if [ "$#" -gt 0 ]; then
    [ "$#" -eq 1 ] && [ "$1" = --static-only ] \
        || fail "usage: $0 [--static-only]"
    probe_mode=static
fi

if [ "$probe_mode" = full ]; then
    [ -d "$app" ] || fail "build the unsigned app before running this probe"
fi
command -v trash >/dev/null 2>&1 || fail "trash is required for probe cleanup"

work=$(mktemp -d "$root/build/u14-release-probe.XXXXXX")
dirty_marker="$root/.u14-release-dirty-probe-$$"

remove_probe_files() {
    cleanup_status=0
    if [ -e "$dirty_marker" ] && ! trash "$dirty_marker"; then cleanup_status=1; fi
    if [ -e "$work" ] && ! trash "$work"; then cleanup_status=1; fi
    [ "$cleanup_status" -eq 0 ]
}

cleanup_probe_on_exit() {
    primary_status=$?
    trap - EXIT
    cleanup_status=0
    remove_probe_files || cleanup_status=1
    if [ "$primary_status" -ne 0 ]; then exit "$primary_status"; fi
    exit "$cleanup_status"
}

cleanup_probe_on_signal() {
    signal_status=$1
    trap - EXIT HUP INT TERM
    remove_probe_files || echo "probe-release-packaging: signal cleanup failed" >&2
    exit "$signal_status"
}

trap cleanup_probe_on_exit EXIT
trap 'cleanup_probe_on_signal 129' HUP
trap 'cleanup_probe_on_signal 130' INT
trap 'cleanup_probe_on_signal 143' TERM

expect_failure() {
    label=$1
    expected=$2
    shift 2
    output="$work/$label.log"
    if "$@" > "$output" 2>&1; then
        fail "$label unexpectedly passed"
    fi
    grep -F "$expected" "$output" >/dev/null \
        || fail "$label failed without expected message: $expected"
    echo "probe passed: $label"
}

expect_failure tag-version-mismatch 'does not match tag' \
    env -u MACOS_DEVELOPER_ID_APPLICATION \
    "$script_dir/build-app.sh" --configuration Release --prepare-signing \
    --tag v1.2.3 --version 1.2.4 --build-number 1

expect_failure invalid-signer-team 'expected Team ID must be 10 uppercase letters or digits' \
    sh -c '. "$1"; release_validate_expected_signer "$2" "$3"' sh \
    "$script_dir/release-common.sh" \
    'Developer ID Application: Example, Inc. (INVALID)' INVALID

printf '%s\n' dirty-release-probe > "$dirty_marker"
expect_failure dirty-signed-refusal 'requires a clean git tree' \
    env -u MACOS_DEVELOPER_ID_APPLICATION \
    "$script_dir/build-app.sh" --configuration Release --prepare-signing \
    --tag v0.1.0 --build-number 1
trash "$dirty_marker"

checksum_dmg="$work/Checksum Fixture.dmg"
checksum_sidecar="$checksum_dmg.sha256"
checksum_provenance="$work/Checksum Fixture.provenance.json"
printf '%s\n' fixture > "$checksum_dmg"
printf '%s\n' '{}' > "$checksum_provenance"
"$script_dir/write-checksum.sh" "$checksum_dmg" >/dev/null
written_digest=$(shasum -a 256 "$checksum_dmg" | awk '{ print $1 }')
[ "$(sed -n '1p' "$checksum_sidecar")" = "$written_digest  Checksum Fixture.dmg" ] \
    || fail "atomic checksum writer emitted the wrong record"
printf '%s\n%s\n' \
    '0000000000000000000000000000000000000000000000000000000000000000  Checksum Fixture.dmg' \
    '0000000000000000000000000000000000000000000000000000000000000000  Checksum Fixture.dmg' \
    > "$checksum_sidecar"
expect_failure checksum-extra-record 'checksum sidecar must contain exactly one record' \
    env \
    MACOS_DEVELOPER_ID_APPLICATION='Developer ID Application: Example, Inc. (ABCDE12345)' \
    MACOS_TEAM_ID=ABCDE12345 \
    "$script_dir/verify-release.sh" "$checksum_dmg"
printf '%s\n' \
    '0000000000000000000000000000000000000000000000000000000000000000  Checksum Fixture.dmg' \
    > "$checksum_sidecar"
expect_failure checksum-digest-mismatch 'DMG SHA-256 does not match checksum sidecar' \
    env \
    MACOS_DEVELOPER_ID_APPLICATION='Developer ID Application: Example, Inc. (ABCDE12345)' \
    MACOS_TEAM_ID=ABCDE12345 \
    "$script_dir/verify-release.sh" "$checksum_dmg"

if [ "$probe_mode" = full ]; then
    stale_app="$work/Stale Open Chronicle.app"
    ditto --noqtn "$app" "$stale_app"
    plutil -replace source_fingerprint -string \
        0000000000000000000000000000000000000000000000000000000000000000 \
        "$stale_app/Contents/Resources/release-provenance.json"
    expect_failure stale-provenance 'source fingerprint is stale or mismatched' \
        "$script_dir/package-dmg.sh" --unsigned --app "$stale_app" \
        --output "$work/stale.dmg"

    extra_app="$work/Extra Mach-O.app"
    ditto --noqtn "$app" "$extra_app"
    ditto --noqtn "$extra_app/Contents/Helpers/chronicle-mcp" \
        "$extra_app/Contents/MacOS/renamed-node"
    chmod 755 "$extra_app/Contents/MacOS/renamed-node"
    expect_failure extra-layout 'unexpected bundle file: Contents/MacOS/renamed-node' \
        "$script_dir/verify-bundle.sh" "$extra_app" --unsigned
fi

uses=$(awk '
    $1 == "-" && $2 == "uses:" { print $3; next }
    $1 == "uses:" { print $2 }
' "$workflow")
[ -n "$uses" ] || fail "workflow action inventory is empty"
unpinned=$(printf '%s\n' "$uses" | grep -Ev '@[0-9a-f]{40}$' || true)
[ -z "$unpinned" ] || fail "workflow contains unpinned actions: $unpinned"
if grep -F -- '--clobber' "$workflow" >/dev/null; then
    fail "workflow must never replace existing release assets"
fi
if grep -F 'Open Chronicle.app.dSYM' "$workflow" >/dev/null; then
    fail "workflow must never upload dSYMs"
fi
grep -F -- '--draft' "$workflow" >/dev/null \
    || fail "workflow candidate must be draft"
grep -F -- '--prerelease' "$workflow" >/dev/null \
    || fail "workflow candidate must be a prerelease"
if grep -E -- '--draft=false|--prerelease=false|--latest|gh release (edit|upload)' \
    "$workflow" >/dev/null; then
    fail "tag workflow must never promote or mutate a candidate"
fi
release_create_count=$(grep -c 'gh release create "$GITHUB_REF_NAME"' "$workflow" || true)
[ "$release_create_count" -eq 1 ] \
    || fail "workflow must create exactly one draft candidate"
release_absence_checks=$(grep -c 'gh release view "$GITHUB_REF_NAME"' "$workflow" || true)
[ "$release_absence_checks" -eq 2 ] \
    || fail "workflow must fail closed on existing releases before build and publish"

publish_job=$(sed -n '/^  signed-publish:/,$p' "$workflow")
printf '%s\n' "$publish_job" | grep -F 'contents: write' >/dev/null \
    || fail "publish job requires minimal contents write permission"
if printf '%s\n' "$publish_job" | grep -F 'secrets.' >/dev/null; then
    fail "publish job must not reference signing or notarization secrets"
fi
publish_uses=$(printf '%s\n' "$publish_job" | awk '
    $1 == "-" && $2 == "uses:" { print $3; next }
    $1 == "uses:" { print $2 }
')
[ "$publish_uses" = 'actions/download-artifact@634f93cb2916e3fdff6788551b99b062d0335ce0' ] \
    || fail "publish job may use only the pinned download action"

cleanup_line=$(grep -n 'Destroy temporary signing and notarization credentials' "$workflow" \
    | cut -d: -f1)
sbom_lines=$(grep -n 'name: Generate SPDX SBOM' "$workflow" | cut -d: -f1)
signed_sbom_line=$(printf '%s\n' "$sbom_lines" | tail -n 1)
[ -n "$cleanup_line" ] && [ "$cleanup_line" -lt "$signed_sbom_line" ] \
    || fail "credential destruction must precede the signed SBOM action"
grep -F 'security lock-keychain "$keychain"' "$workflow" >/dev/null \
    || fail "workflow must lock the temporary keychain"
grep -F 'security delete-keychain "$keychain"' "$workflow" >/dev/null \
    || fail "workflow must delete the temporary keychain"

unsigned_build_line=$(grep -n 'Build unsigned release-candidate inputs' "$workflow" | cut -d: -f1)
unsigned_package_line=$(grep -n 'Package unsigned release-candidate inputs' "$workflow" | cut -d: -f1)
sign_phase_line=$(grep -n 'Materialize signer, sign app and DMG' "$workflow" | cut -d: -f1)
notary_phase_line=$(grep -n 'Materialize notary credential' "$workflow" | cut -d: -f1)
[ "$unsigned_build_line" -lt "$unsigned_package_line" ] \
    && [ "$unsigned_package_line" -lt "$sign_phase_line" ] \
    && [ "$sign_phase_line" -lt "$notary_phase_line" ] \
    || fail "unsigned build/package must precede signer and notary materialization"
if grep -E 'build-app\.sh.*[[:space:]]--sign([[:space:]]|$)' "$workflow" >/dev/null; then
    fail "workflow must use signing-only logic after the credential boundary"
fi
if grep -F -- '--sign' "$script_dir/build-app.sh" >/dev/null; then
    fail "build-app must not expose a combined build-and-sign mode"
fi
grep -F './scripts/sign-app.sh' "$workflow" >/dev/null \
    || fail "workflow must use the signing-only app path"
grep -F './scripts/package-dmg.sh --sign-only' "$workflow" >/dev/null \
    || fail "workflow must create the signed DMG without rebuilding or early notarization"
if grep -E 'xcodebuild|cargo|build-rust' "$script_dir/sign-app.sh" >/dev/null; then
    fail "signing-only script must not run build or dependency tooling"
fi
staple_line=$(grep -n "xcrun stapler staple 'dist/Open Chronicle.dmg'" "$workflow" | cut -d: -f1)
final_checksum_line=$(grep -n "write-checksum.sh 'dist/Open Chronicle.dmg'" "$workflow" | cut -d: -f1)
final_verify_line=$(grep -n "name: Verify signed release" "$workflow" | cut -d: -f1)
[ "$staple_line" -lt "$final_checksum_line" ] \
    && [ "$final_checksum_line" -lt "$final_verify_line" ] \
    || fail "final checksum regeneration must follow stapling and precede verification"

echo "probe passed: pinned actions and immutable draft candidate workflow"
echo "all release packaging probes passed"
