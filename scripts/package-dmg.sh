#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
. "$script_dir/release-common.sh"

mode=
app="$root/dist/Open Chronicle.app"
output="$root/dist/Open Chronicle.dmg"

usage() {
    echo "usage: $0 (--unsigned|--unsigned-candidate|--sign-only|--sign) [--app PATH] [--output PATH]" >&2
    exit 2
}

fail() {
    echo "package-dmg: $*" >&2
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --unsigned|--unsigned-candidate|--sign-only|--sign)
            [ -z "$mode" ] || usage
            mode=$1
            shift
            ;;
        --app)
            [ "$#" -ge 2 ] || usage
            app=$2
            shift 2
            ;;
        --output)
            [ "$#" -ge 2 ] || usage
            output=$2
            shift 2
            ;;
        *) usage ;;
    esac
done

[ -n "$mode" ] || usage
[ -d "$app" ] || fail "app bundle not found: $app"
command -v hdiutil >/dev/null 2>&1 || fail "hdiutil is required"
command -v ditto >/dev/null 2>&1 || fail "ditto is required"

if [ "$mode" = "--sign" ] || [ "$mode" = "--sign-only" ] \
    || [ "$mode" = "--unsigned-candidate" ]; then
    provenance_mode=release-candidate
else
    provenance_mode=development
fi

release_validate_app_provenance_current "$root" "$app" "$provenance_mode"

if [ "$mode" = "--sign" ] || [ "$mode" = "--sign-only" ]; then
    [ -n "${MACOS_DEVELOPER_ID_APPLICATION:-}" ] \
        || fail "--sign requires MACOS_DEVELOPER_ID_APPLICATION"
    [ -n "${MACOS_TEAM_ID:-}" ] \
        || fail "--sign requires MACOS_TEAM_ID"
    release_validate_expected_signer \
        "$MACOS_DEVELOPER_ID_APPLICATION" "$MACOS_TEAM_ID" \
        || fail "configured signer identity or Team ID is invalid"
    "$script_dir/verify-bundle.sh" "$app" --signed
elif [ "$mode" = "--unsigned-candidate" ]; then
    "$script_dir/verify-bundle.sh" "$app" --unsigned-candidate
else
    "$script_dir/verify-bundle.sh" "$app" --unsigned
fi

output_dir=$(dirname -- "$output")
mkdir -p "$output_dir" "$root/build"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
output="$output_dir/$(basename -- "$output")"
work=$(mktemp -d "$root/build/u14-dmg-work.XXXXXX")
stage="$work/payload"
temporary_dmg="$work/Open Chronicle.dmg"
mkdir -p "$stage"

remove_work() {
    [ ! -e "$work" ] && return 0
    if command -v trash >/dev/null 2>&1; then
        trash "$work"
        return
    fi
    echo "package-dmg: preserved working directory because trash is unavailable: $work" >&2
    return 1
}

cleanup_work_on_exit() {
    primary_status=$?
    trap - EXIT
    cleanup_status=0
    if ! remove_work; then
        echo "package-dmg: failed to clean packaging work directory: $work" >&2
        cleanup_status=1
    fi
    if [ "$primary_status" -ne 0 ]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}

cleanup_work_on_signal() {
    signal_status=$1
    trap - EXIT HUP INT TERM
    if ! remove_work; then
        echo "package-dmg: cleanup also failed while handling signal: $work" >&2
    fi
    exit "$signal_status"
}

trap cleanup_work_on_exit EXIT
trap 'cleanup_work_on_signal 129' HUP
trap 'cleanup_work_on_signal 130' INT
trap 'cleanup_work_on_signal 143' TERM

ditto --noqtn "$app" "$stage/Open Chronicle.app"
ln -s /Applications "$stage/Applications"

hdiutil create \
    -volname "Open Chronicle" \
    -fs HFS+ \
    -format UDZO \
    -srcfolder "$stage" \
    -ov \
    "$temporary_dmg"

if [ "$mode" = "--sign" ] || [ "$mode" = "--sign-only" ]; then
    codesign --force --timestamp --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$temporary_dmg"
    release_assert_codesign_identity \
        "$temporary_dmg" "DMG" \
        "$MACOS_DEVELOPER_ID_APPLICATION" "$MACOS_TEAM_ID" \
        || fail "DMG signer identity verification failed"
fi

if [ "$mode" = "--sign" ]; then
    [ -n "${MACOS_NOTARY_KEYCHAIN_PROFILE:-}" ] \
        || fail "--sign requires MACOS_NOTARY_KEYCHAIN_PROFILE"
    if [ -n "${MACOS_NOTARY_KEYCHAIN:-}" ]; then
        xcrun notarytool submit "$temporary_dmg" \
            --keychain-profile "$MACOS_NOTARY_KEYCHAIN_PROFILE" \
            --keychain "$MACOS_NOTARY_KEYCHAIN" \
            --wait
    else
        xcrun notarytool submit "$temporary_dmg" \
            --keychain-profile "$MACOS_NOTARY_KEYCHAIN_PROFILE" \
            --wait
    fi
    xcrun stapler staple "$temporary_dmg"
    xcrun stapler validate "$temporary_dmg"
fi

release_validate_app_provenance_current "$root" "$app" "$provenance_mode"

mv -f "$temporary_dmg" "$output"

"$script_dir/write-checksum.sh" "$output"
checksum="$output.sha256"
case "$output" in
    *.dmg) provenance_output=${output%.dmg}.provenance.json ;;
    *) provenance_output=$output.provenance.json ;;
esac
ditto --noqtn "$app/Contents/Resources/release-provenance.json" "$provenance_output"
chmod 644 "$output" "$checksum" "$provenance_output"

echo "built $output (${mode#--})"
echo "built $checksum"
echo "built $provenance_output"
