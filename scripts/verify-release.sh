#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
. "$script_dir/release-common.sh"

usage() {
    echo "usage: $0 DMG_PATH" >&2
    exit 2
}

fail() {
    echo "verify-release: $*" >&2
    exit 1
}

[ "$#" -eq 1 ] || usage
dmg=$1
[ -f "$dmg" ] || fail "DMG not found: $dmg"
[ -n "${MACOS_DEVELOPER_ID_APPLICATION:-}" ] \
    || fail "signed verification requires MACOS_DEVELOPER_ID_APPLICATION"
[ -n "${MACOS_TEAM_ID:-}" ] \
    || fail "signed verification requires MACOS_TEAM_ID"
release_validate_expected_signer \
    "$MACOS_DEVELOPER_ID_APPLICATION" "$MACOS_TEAM_ID" \
    || fail "configured signer identity or Team ID is invalid"

dmg_dir=$(CDPATH= cd -- "$(dirname -- "$dmg")" && pwd)
dmg_name=$(basename -- "$dmg")
dmg="$dmg_dir/$dmg_name"
checksum="$dmg.sha256"
[ -f "$checksum" ] || fail "checksum not found: $checksum"
case "$dmg" in
    *.dmg) provenance=${dmg%.dmg}.provenance.json ;;
    *) provenance=$dmg.provenance.json ;;
esac
[ -f "$provenance" ] || fail "provenance sidecar not found: $provenance"

checksum_records=$(awk 'END { print NR + 0 }' "$checksum")
[ "$checksum_records" -eq 1 ] \
    || fail "checksum sidecar must contain exactly one record"
checksum_record=$(sed -n '1p' "$checksum")
expected_digest=$(printf '%s\n' "$checksum_record" | cut -c 1-64)
printf '%s\n' "$expected_digest" | grep -Eq '^[0-9a-f]{64}$' \
    || fail "checksum sidecar digest must be 64 lowercase hexadecimal characters"
[ "$checksum_record" = "$expected_digest  $dmg_name" ] \
    || fail "checksum sidecar must be exactly: <64 lowercase hex><two spaces>$dmg_name"
computed_digest=$(shasum -a 256 "$dmg" | awk '{ print $1 }')
[ "$computed_digest" = "$expected_digest" ] \
    || fail "DMG SHA-256 does not match checksum sidecar"
echo "$dmg_name: SHA-256 verified"

codesign --verify --verbose=2 "$dmg"
release_assert_codesign_identity \
    "$dmg" "DMG" "$MACOS_DEVELOPER_ID_APPLICATION" "$MACOS_TEAM_ID" \
    || fail "DMG signer identity verification failed"
xcrun stapler validate "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"

mountpoint=$(mktemp -d "${TMPDIR:-/tmp}/open-chronicle-release.XXXXXX")
mounted=false

detach_mount() {
    attempt=1
    while [ "$attempt" -le 3 ]; do
        if hdiutil detach "$mountpoint" >/dev/null 2>&1; then
            mounted=false
            return 0
        fi
        if [ "$attempt" -lt 3 ]; then
            sleep 1
        fi
        attempt=$((attempt + 1))
    done
    if hdiutil detach -force "$mountpoint" >/dev/null 2>&1; then
        mounted=false
        return 0
    fi
    return 1
}

perform_mount_cleanup() {
    cleanup_status=0
    if [ "$mounted" = true ]; then
        if ! detach_mount; then
            echo "verify-release: failed to detach $mountpoint after three retries and one forced attempt" >&2
            cleanup_status=1
        fi
    fi
    if [ -d "$mountpoint" ] && ! rmdir "$mountpoint" 2>/dev/null; then
        echo "verify-release: failed to remove mountpoint $mountpoint" >&2
        cleanup_status=1
    fi
    [ "$cleanup_status" -eq 0 ]
}

cleanup_mount_on_exit() {
    primary_status=$?
    trap - EXIT
    cleanup_status=0
    perform_mount_cleanup || cleanup_status=1
    if [ "$primary_status" -ne 0 ]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}

cleanup_mount_on_signal() {
    signal_status=$1
    trap - EXIT HUP INT TERM
    if ! perform_mount_cleanup; then
        echo "verify-release: cleanup also failed while handling signal" >&2
    fi
    exit "$signal_status"
}

trap cleanup_mount_on_exit EXIT
trap 'cleanup_mount_on_signal 129' HUP
trap 'cleanup_mount_on_signal 130' INT
trap 'cleanup_mount_on_signal 143' TERM

hdiutil attach "$dmg" -nobrowse -readonly -mountpoint "$mountpoint" >/dev/null
mounted=true

app="$mountpoint/Open Chronicle.app"
[ -d "$app" ] || fail "DMG does not contain Open Chronicle.app"
[ -L "$mountpoint/Applications" ] || fail "DMG does not contain the Applications link"
[ "$(readlink "$mountpoint/Applications")" = /Applications ] \
    || fail "DMG Applications link has the wrong destination"

entry_count=0
for entry in "$mountpoint"/* "$mountpoint"/.[!.]* "$mountpoint"/..?*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    entry_count=$((entry_count + 1))
    case "$entry" in
        "$app"|"$mountpoint/Applications") ;;
        *) fail "unexpected top-level DMG entry: $entry" ;;
    esac
done
[ "$entry_count" -eq 2 ] || fail "DMG must contain exactly the app and Applications link"

"$script_dir/verify-bundle.sh" "$app" --signed
cmp -s "$provenance" "$app/Contents/Resources/release-provenance.json" \
    || fail "provenance sidecar does not match the notarized app"
release_validate_app_provenance_current "$root" "$app" release-candidate

echo "verified signed, notarized, stapled release: $dmg"
