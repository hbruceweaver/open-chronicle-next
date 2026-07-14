#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
toolchain=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$root/rust-toolchain.toml")

if [ -z "$toolchain" ]; then
    echo "rust-toolchain.toml does not declare a channel" >&2
    exit 2
fi
if ! command -v rustup >/dev/null 2>&1; then
    echo "rustup is required to build the bundled Rust core" >&2
    exit 2
fi
if ! command -v xcrun >/dev/null 2>&1; then
    echo "Xcode command-line tools are required" >&2
    exit 2
fi

toolchain_root=$(rustup run "$toolchain" rustc --print sysroot)
cargo="$toolchain_root/bin/cargo"
rustc="$toolchain_root/bin/rustc"

if [ ! -x "$cargo" ] || [ ! -x "$rustc" ]; then
    echo "pinned Rust toolchain $toolchain is incomplete" >&2
    exit 2
fi

targets="aarch64-apple-darwin x86_64-apple-darwin"
for target in $targets; do
    rustup target add --toolchain "$toolchain" "$target" >/dev/null
    target_dir="$root/build/rust/$target"
    MACOSX_DEPLOYMENT_TARGET=14.0 \
        CARGO_TARGET_DIR="$target_dir" \
        RUSTC="$rustc" \
        "$cargo" build \
        --manifest-path "$root/Cargo.toml" \
        --package chronicle-ffi \
        --release \
        --locked \
        --target "$target"
done

universal_dir="$root/build/universal"
mkdir -p "$universal_dir"
temporary_archive=$(mktemp "$universal_dir/libchronicle_ffi.a.XXXXXX")
xcrun lipo -create \
    "$root/build/rust/aarch64-apple-darwin/aarch64-apple-darwin/release/libchronicle_ffi.a" \
    "$root/build/rust/x86_64-apple-darwin/x86_64-apple-darwin/release/libchronicle_ffi.a" \
    -output "$temporary_archive"
mv -f "$temporary_archive" "$universal_dir/libchronicle_ffi.a"

architectures=$(xcrun lipo -archs "$universal_dir/libchronicle_ffi.a")
case " $architectures " in *" arm64 "*) ;; *) echo "universal archive is missing arm64: $architectures" >&2; exit 1 ;; esac
case " $architectures " in *" x86_64 "*) ;; *) echo "universal archive is missing x86_64: $architectures" >&2; exit 1 ;; esac

echo "built $universal_dir/libchronicle_ffi.a ($architectures)"
