#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$script_dir/release-common.sh"

mode=auto

usage() {
    echo "usage: $0 APP_PATH [--unsigned|--unsigned-candidate|--signed]" >&2
    exit 2
}

fail() {
    echo "verify-bundle: $*" >&2
    exit 1
}

[ "$#" -ge 1 ] || usage
app=$1
shift

if [ "$#" -gt 0 ]; then
    case "$1" in
        --unsigned) mode=unsigned ;;
        --unsigned-candidate) mode=unsigned-candidate ;;
        --signed) mode=signed ;;
        *) usage ;;
    esac
    shift
fi
[ "$#" -eq 0 ] || usage

[ -d "$app" ] || fail "app bundle not found: $app"

info="$app/Contents/Info.plist"
privacy="$app/Contents/Resources/PrivacyInfo.xcprivacy"
main="$app/Contents/MacOS/Open Chronicle"
helper="$app/Contents/Helpers/chronicle-mcp"

[ -f "$info" ] || fail "Info.plist is missing"
[ -f "$privacy" ] || fail "PrivacyInfo.xcprivacy is missing"
[ -x "$main" ] || fail "main executable is missing or not executable"
[ -x "$helper" ] || fail "MCP helper is missing or not executable"

signature_info=$(codesign -d --verbose=4 "$app" 2>&1 || true)
if [ "$mode" = auto ]; then
    if printf '%s\n' "$signature_info" | grep -F 'Authority=Developer ID Application' >/dev/null; then
        mode=signed
    else
        mode=unsigned
    fi
fi
if [ "$mode" = signed ]; then
    provenance_mode=release-candidate
    expected_identity=${MACOS_DEVELOPER_ID_APPLICATION:-}
    expected_team_id=${MACOS_TEAM_ID:-}
    [ -n "$expected_identity" ] \
        || fail "signed verification requires MACOS_DEVELOPER_ID_APPLICATION"
    [ -n "$expected_team_id" ] \
        || fail "signed verification requires MACOS_TEAM_ID"
    release_validate_expected_signer "$expected_identity" "$expected_team_id" \
        || fail "configured signer identity or Team ID is invalid"
elif [ "$mode" = unsigned-candidate ]; then
    provenance_mode=release-candidate
else
    provenance_mode=development
fi

plutil -lint "$info" >/dev/null
plutil -lint "$privacy" >/dev/null
release_read_app_provenance "$app" "$provenance_mode"

plist_value() {
    /usr/libexec/PlistBuddy -c "Print :$2" "$1" 2>/dev/null \
        || fail "$2 is missing from $1"
}

assert_plist_value() {
    actual=$(plist_value "$1" "$2")
    [ "$actual" = "$3" ] || fail "$2 must be $3, got $actual"
}

assert_plist_value "$info" CFBundlePackageType APPL
assert_plist_value "$info" CFBundleIdentifier com.screenata.openchronicle
assert_plist_value "$info" CFBundleExecutable "Open Chronicle"
assert_plist_value "$info" LSMinimumSystemVersion 14.0

usage_description=$(plist_value "$info" NSScreenCaptureUsageDescription)
[ -n "$usage_description" ] || fail "NSScreenCaptureUsageDescription is empty"
assert_plist_value "$privacy" NSPrivacyTracking false

[ -f "$app/Contents/PkgInfo" ] || fail "PkgInfo is missing"
[ -f "$app/Contents/Resources/release-provenance.json" ] || fail "release provenance is missing"
if [ "$mode" = signed ]; then
    [ -f "$app/Contents/_CodeSignature/CodeResources" ] \
        || fail "signed bundle is missing _CodeSignature/CodeResources"
fi

find "$app" -mindepth 1 -print | while IFS= read -r entry; do
    relative=${entry#"$app"/}
    [ ! -L "$entry" ] || fail "bundle symlinks are not allowed: $relative"
    if [ -d "$entry" ]; then
        case "$relative" in
            Contents|Contents/Helpers|Contents/MacOS|Contents/Resources) ;;
            Contents/_CodeSignature)
                [ "$mode" = signed ] || fail "signature directory is forbidden in unsigned mode"
                ;;
            *) fail "unexpected bundle directory: $relative" ;;
        esac
    elif [ -f "$entry" ]; then
        case "$relative" in
            Contents/Helpers/chronicle-mcp|Contents/Info.plist|Contents/MacOS/Open\ Chronicle|Contents/PkgInfo|Contents/Resources/PrivacyInfo.xcprivacy|Contents/Resources/release-provenance.json) ;;
            Contents/_CodeSignature/CodeResources)
                [ "$mode" = signed ] || fail "signature resources are forbidden in unsigned mode"
                ;;
            *) fail "unexpected bundle file: $relative" ;;
        esac
    else
        fail "unexpected bundle entry type: $relative"
    fi
done

assert_universal() {
    binary=$1
    label=$2
    archs=$(xcrun lipo -archs "$binary")
    case " $archs " in *" arm64 "*) ;; *) fail "$label is missing arm64: $archs" ;; esac
    case " $archs " in *" x86_64 "*) ;; *) fail "$label is missing x86_64: $archs" ;; esac
    count=$(printf '%s\n' "$archs" | awk '{ print NF }')
    [ "$count" -eq 2 ] || fail "$label has unexpected architectures: $archs"
    echo "verified $label architectures: $archs"
}

assert_minos() {
    binary=$1
    label=$2
    minos_values=$(otool -l "$binary" | awk '
        $1 == "cmd" && $2 == "LC_BUILD_VERSION" { build = 1; next }
        build && $1 == "minos" { print $2; build = 0 }
    ')
    minos_count=$(printf '%s\n' "$minos_values" | awk 'NF { count += 1 } END { print count + 0 }')
    [ "$minos_count" -eq 2 ] \
        || fail "$label must have one deployment target per architecture, got ${minos_values:-missing}"
    while IFS= read -r minos; do
        [ "$minos" = "14.0" ] \
            || fail "$label minimum macOS must be 14.0 in every slice, got $minos"
    done <<EOF
$minos_values
EOF
    echo "verified $label minimum macOS in both slices: 14.0"
}

assert_system_dependencies() {
    binary=$1
    label=$2
    dependencies=$(otool -L "$binary" | sed -E '/:$/d; s/^[[:space:]]*//; s/[[:space:]]+\(compatibility.*$//')
    [ -n "$dependencies" ] || fail "$label has no dynamic-library inventory"
    while IFS= read -r dependency; do
        case "$dependency" in
            /System/Library/*|/usr/lib/*) ;;
            *) fail "$label has unexpected dynamic dependency: $dependency" ;;
        esac
    done <<EOF
$dependencies
EOF
    echo "verified $label dynamic dependencies are system-only"
}

assert_universal "$main" "main executable"
assert_universal "$helper" "MCP helper"
assert_minos "$main" "main executable"
assert_minos "$helper" "MCP helper"
assert_system_dependencies "$main" "main executable"
assert_system_dependencies "$helper" "MCP helper"

find "$app" -type f -print | while IFS= read -r entry; do
    description=$(file -b "$entry")
    case "$description" in
        *Mach-O*)
            case "$entry" in
                "$main"|"$helper") ;;
                *) fail "unexpected Mach-O payload: ${entry#"$app"/}" ;;
            esac
            ;;
        *)
            [ ! -x "$entry" ] \
                || fail "unexpected executable non-Mach-O payload: ${entry#"$app"/}"
            ;;
    esac
done

forbidden_payload=$(find "$app" -type f \( \
    -name '*.a' -o -name '*.h' -o -name '*.rs' -o -name '*.swift' -o \
    -name '*.ts' -o -name '*.js' -o -name '*.py' -o \
    -name node -o -name npm -o -name python -o -name python3 -o \
    -name cargo -o -name rustc -o -name swift -o -name xcodebuild \
    \) -print -quit)
[ -z "$forbidden_payload" ] || fail "forbidden source/developer runtime payload: $forbidden_payload"

if [ "$mode" = signed ]; then
    printf '%s\n' "$signature_info" | grep -F '(runtime)' >/dev/null \
        || fail "app signature does not enable Hardened Runtime"
    codesign --verify --strict --verbose=2 "$main"
    codesign --verify --strict --verbose=2 "$helper"
    codesign --verify --strict --verbose=2 "$app"
    helper_signature=$(codesign -d --verbose=4 "$helper" 2>&1 || true)
    printf '%s\n' "$helper_signature" | grep -F '(runtime)' >/dev/null \
        || fail "MCP helper signature does not enable Hardened Runtime"
    release_assert_codesign_identity \
        "$main" "main executable" "$expected_identity" "$expected_team_id" \
        || fail "main executable signer identity verification failed"
    release_assert_codesign_identity \
        "$helper" "MCP helper" "$expected_identity" "$expected_team_id" \
        || fail "MCP helper signer identity verification failed"
    release_assert_codesign_identity \
        "$app" "application bundle" "$expected_identity" "$expected_team_id" \
        || fail "application bundle signer identity verification failed"
    echo "verified exact Developer ID signer, TeamIdentifier, and Hardened Runtime"
else
    if printf '%s\n' "$signature_info" | grep -F 'Authority=Developer ID Application' >/dev/null; then
        fail "unsigned verification received a Developer ID signed app"
    fi
    echo "verified unsigned/ad-hoc development bundle; no public-install claim"
fi

echo "verified bundle: $app"
