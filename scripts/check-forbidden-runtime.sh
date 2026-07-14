#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
default_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

if [ "${1:-}" = "--self-test" ]; then
    temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/chronicle-runtime-guard.XXXXXX")
    mkdir -p "$temporary_root/crates/probe/src"
    printf '%s\n' 'fn probe() { std::process::Command::new("node"); }' \
        > "$temporary_root/crates/probe/src/lib.rs"

    if "$0" "$temporary_root" >/dev/null 2>&1; then
        echo "forbidden-runtime guard self-test failed: seeded runtime was accepted" >&2
        exit 1
    fi

    if command -v trash >/dev/null 2>&1; then
        trash "$temporary_root"
    else
        echo "forbidden-runtime guard self-test left temporary fixture at $temporary_root" >&2
    fi

    echo "forbidden-runtime guard self-test passed"
    exit 0
fi

root=${1:-$default_root}

if [ ! -d "$root" ]; then
    echo "forbidden-runtime guard root does not exist: $root" >&2
    exit 2
fi

runtime_pattern='CGWindowListCreateImage|/usr/sbin/screencapture|Command::new\([[:space:]]*"(node|npm|npx|pnpm|yarn|python|python3|swift)"|(^|[;&|[:space:]])(node|npm|npx|pnpm|yarn)[[:space:]]+(run|exec|install|start)|swift[[:space:]]+run|CARGO_MANIFEST_DIR.{0,160}\.(swift|js|ts|py)|/Users/[^/[:space:]"'\'']+/'

failed=0
for relative_path in crates macos app Sources spikes scripts; do
    candidate="$root/$relative_path"
    if [ ! -e "$candidate" ]; then
        continue
    fi

    if command -v rg >/dev/null 2>&1; then
        matches=$(rg --line-number --with-filename --pcre2 \
            --glob '*.rs' --glob '*.swift' --glob '*.sh' --glob '*.toml' \
            --glob '*.json' --glob '*.plist' --glob '*.yml' --glob '*.yaml' \
            --glob '!check-forbidden-runtime.sh' \
            --glob '!check-capture-apis.sh' \
            "$runtime_pattern" "$candidate" || true)
    else
        matches=$(find "$candidate" -type f \
            ! -path '*/.build/*' ! -path '*/target/*' ! -path '*/.git/*' \
            \( -name '*.rs' -o -name '*.swift' -o -name '*.sh' \
            -o -name '*.toml' -o -name '*.json' -o -name '*.plist' \
            -o -name '*.yml' -o -name '*.yaml' \) \
            ! -name 'check-forbidden-runtime.sh' \
            ! -name 'check-capture-apis.sh' \
            -exec grep -nEH "$runtime_pattern" {} + 2>/dev/null || true)
    fi

    if [ -n "$matches" ]; then
        echo "forbidden runtime or source-checkout dependency found:" >&2
        echo "$matches" >&2
        failed=1
    fi
done

fixtures="$root/fixtures"
if [ -d "$fixtures" ]; then
    if command -v rg >/dev/null 2>&1; then
        fixture_files=$(rg --files --hidden "$fixtures" || true)
    else
        fixture_files=$(find "$fixtures" -type f -print)
    fi
    if [ -n "$fixture_files" ]; then
        while IFS= read -r fixture; do
            case "$fixture" in
                */fixtures/synthetic/*) ;;
                *)
                    echo "fixture must live under fixtures/synthetic: $fixture" >&2
                    failed=1
                    ;;
            esac
        done <<EOF
$fixture_files
EOF
    fi

    fixture_pattern='(/Users/[^/[:space:]"'\'']+/|BEGIN[[:space:]]+PRIVATE|real[_ -]?evidence)'
    if command -v rg >/dev/null 2>&1; then
        matches=$(rg --line-number --with-filename --hidden \
            --glob '!synthetic/**' "$fixture_pattern" "$fixtures" || true)
    else
        matches=$(find "$fixtures" -type f ! -path '*/synthetic/*' \
            -exec grep -nEH "$fixture_pattern" {} + 2>/dev/null || true)
    fi

    if [ -n "$matches" ]; then
        echo "possible real-user evidence fixture found:" >&2
        echo "$matches" >&2
        failed=1
    fi
fi

if [ "$failed" -ne 0 ]; then
    exit 1
fi

echo "forbidden-runtime guard passed"
