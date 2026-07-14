#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
. "$script_dir/release-common.sh"

app="$root/dist/Open Chronicle.app"
if [ "$#" -gt 0 ]; then
    [ "$#" -eq 2 ] && [ "$1" = --app ] || {
        echo "usage: $0 [--app PATH]" >&2
        exit 2
    }
    app=$2
fi

fail() {
    echo "sign-app: $*" >&2
    exit 1
}

[ -d "$app" ] || fail "app bundle not found: $app"
[ -n "${MACOS_DEVELOPER_ID_APPLICATION:-}" ] \
    || fail "MACOS_DEVELOPER_ID_APPLICATION is required"
[ -n "${MACOS_TEAM_ID:-}" ] || fail "MACOS_TEAM_ID is required"
release_validate_expected_signer \
    "$MACOS_DEVELOPER_ID_APPLICATION" "$MACOS_TEAM_ID" \
    || fail "configured signer identity or Team ID is invalid"

release_validate_app_provenance_current "$root" "$app" release-candidate
"$script_dir/verify-bundle.sh" "$app" --unsigned-candidate

helper="$app/Contents/Helpers/chronicle-mcp"
entitlements="$root/macos/OpenChronicle/Resources/OpenChronicle.entitlements"
if [ -n "${MACOS_SIGNING_KEYCHAIN:-}" ]; then
    security find-identity -v -p codesigning "$MACOS_SIGNING_KEYCHAIN" \
        | grep -F "\"$MACOS_DEVELOPER_ID_APPLICATION\"" >/dev/null \
        || fail "expected identity is unavailable in the temporary keychain"
    codesign --force --timestamp --options runtime \
        --keychain "$MACOS_SIGNING_KEYCHAIN" \
        --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$helper"
    codesign --force --timestamp --options runtime --entitlements "$entitlements" \
        --keychain "$MACOS_SIGNING_KEYCHAIN" \
        --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$app"
else
    security find-identity -v -p codesigning \
        | grep -F "\"$MACOS_DEVELOPER_ID_APPLICATION\"" >/dev/null \
        || fail "expected identity is unavailable"
    codesign --force --timestamp --options runtime \
        --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$helper"
    codesign --force --timestamp --options runtime --entitlements "$entitlements" \
        --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$app"
fi

"$script_dir/verify-bundle.sh" "$app" --signed
echo "signed app without rebuilding: $app"
