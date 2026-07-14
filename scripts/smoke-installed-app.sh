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
    echo "smoke-installed-app: $*" >&2
    exit 1
}

[ "$#" -eq 1 ] || usage
dmg=$1
[ -f "$dmg" ] || fail "DMG not found: $dmg"
case "$dmg" in
    *.dmg) provenance=${dmg%.dmg}.provenance.json ;;
    *) provenance=$dmg.provenance.json ;;
esac
[ -f "$provenance" ] || fail "provenance sidecar not found: $provenance"

mountpoint=$(mktemp -d "${TMPDIR:-/tmp}/open-chronicle-smoke-mount.XXXXXX")
install_root=$(mktemp -d "$root/build/u14-installed-smoke.XXXXXX")
installed_app="$install_root/Applications/Open Chronicle.app"
managed_root="$install_root/Application Support/Open Chronicle"
response="$install_root/mcp-response.jsonl"
stderr_log="$install_root/mcp-stderr.log"
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
            echo "smoke-installed-app: failed to detach $mountpoint after three retries and one forced attempt" >&2
            cleanup_status=1
        fi
    fi
    if [ -d "$mountpoint" ] && ! rmdir "$mountpoint" 2>/dev/null; then
        echo "smoke-installed-app: failed to remove mountpoint $mountpoint" >&2
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
        echo "smoke-installed-app: cleanup also failed while handling signal" >&2
    fi
    exit "$signal_status"
}

trap cleanup_mount_on_exit EXIT
trap 'cleanup_mount_on_signal 129' HUP
trap 'cleanup_mount_on_signal 130' INT
trap 'cleanup_mount_on_signal 143' TERM

hdiutil attach "$dmg" -nobrowse -readonly -mountpoint "$mountpoint" >/dev/null
mounted=true

[ -d "$mountpoint/Open Chronicle.app" ] || fail "DMG does not contain Open Chronicle.app"
[ -L "$mountpoint/Applications" ] || fail "DMG does not contain the Applications link"
[ "$(readlink "$mountpoint/Applications")" = /Applications ] \
    || fail "DMG Applications link has the wrong destination"

entry_count=0
for entry in "$mountpoint"/* "$mountpoint"/.[!.]* "$mountpoint"/..?*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    entry_count=$((entry_count + 1))
    case "$entry" in
        "$mountpoint/Open Chronicle.app"|"$mountpoint/Applications") ;;
        *) fail "unexpected top-level DMG entry: $entry" ;;
    esac
done
[ "$entry_count" -eq 2 ] || fail "DMG must contain exactly the app and Applications link"

mkdir -p "$(dirname -- "$installed_app")" "$managed_root"
ditto --noqtn "$mountpoint/Open Chronicle.app" "$installed_app"

"$script_dir/verify-bundle.sh" "$installed_app"
cmp -s "$provenance" "$installed_app/Contents/Resources/release-provenance.json" \
    || fail "provenance sidecar does not match the copied app"
release_validate_app_provenance_current "$root" "$installed_app" development

helper="$installed_app/Contents/Helpers/chronicle-mcp"
{
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"u14-installed-smoke","version":"1"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
} | "$helper" \
    --managed-root "$managed_root" \
    --client-id client-u14-installed-smoke \
    --grant-id grant-u14-installed-smoke \
    > "$response" 2> "$stderr_log"

[ ! -s "$stderr_log" ] || fail "installed MCP helper wrote stderr; see $stderr_log"
grep -F '"protocolVersion":"2025-06-18"' "$response" >/dev/null \
    || fail "installed MCP helper did not complete initialization"
grep -F '"name":"open-chronicle"' "$response" >/dev/null \
    || fail "installed MCP helper returned the wrong server identity"
tool_count=$(grep -o '"name":"chronicle_[^"]*"' "$response" | sort -u | wc -l | tr -d ' ')
[ "$tool_count" -eq 16 ] || fail "installed MCP helper exposed $tool_count tools, expected 16"

echo "verified copied bundle and installed MCP stdio runtime: $installed_app"
echo "preserved smoke evidence at $install_root"
echo "GUI launch, /Applications installation, quarantine, permissions, capture, and network-disabled behavior were not claimed"
