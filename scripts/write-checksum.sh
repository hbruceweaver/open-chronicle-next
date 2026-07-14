#!/bin/sh
set -eu

usage() {
    echo "usage: $0 DMG_PATH" >&2
    exit 2
}

[ "$#" -eq 1 ] || usage
dmg=$1
[ -f "$dmg" ] || {
    echo "write-checksum: DMG not found: $dmg" >&2
    exit 1
}

dmg_dir=$(CDPATH= cd -- "$(dirname -- "$dmg")" && pwd)
dmg_name=$(basename -- "$dmg")
dmg="$dmg_dir/$dmg_name"
checksum="$dmg.sha256"
temporary=$(mktemp "$dmg_dir/.open-chronicle-checksum.XXXXXX")

cleanup_temporary() {
    primary_status=$?
    trap - EXIT
    if [ -e "$temporary" ]; then
        if command -v trash >/dev/null 2>&1; then
            trash "$temporary" || true
        else
            rm -f "$temporary"
        fi
    fi
    exit "$primary_status"
}
trap cleanup_temporary EXIT

digest=$(shasum -a 256 "$dmg" | awk '{ print $1 }')
printf '%s\n' "$digest" | grep -Eq '^[0-9a-f]{64}$' || {
    echo "write-checksum: shasum returned an invalid digest" >&2
    exit 1
}
printf '%s  %s\n' "$digest" "$dmg_name" > "$temporary"
chmod 644 "$temporary"
mv -f "$temporary" "$checksum"
trap - EXIT

echo "wrote $checksum"
