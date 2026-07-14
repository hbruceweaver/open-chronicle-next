#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
. "$script_dir/release-common.sh"

configuration=Release
mode=
version=
build_number=
tag=

usage() {
    echo "usage: $0 [--configuration Debug|Release] (--unsigned|--prepare-signing) [--version X.Y.Z] [--build-number N] [--tag vX.Y.Z]" >&2
    exit 2
}

fail() {
    echo "build-app: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

replace_directory() {
    path=$1
    if [ ! -e "$path" ]; then
        return
    fi
    if ! command -v trash >/dev/null 2>&1; then
        fail "$path already exists; move it aside or install the trash command before rebuilding"
    fi
    trash "$path"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --configuration)
            [ "$#" -ge 2 ] || usage
            configuration=$2
            shift 2
            ;;
        --unsigned|--prepare-signing)
            [ -z "$mode" ] || usage
            mode=$1
            shift
            ;;
        --version)
            [ "$#" -ge 2 ] || usage
            version=$2
            shift 2
            ;;
        --build-number)
            [ "$#" -ge 2 ] || usage
            build_number=$2
            shift 2
            ;;
        --tag)
            [ "$#" -ge 2 ] || usage
            tag=$2
            shift 2
            ;;
        *) usage ;;
    esac
done

case "$configuration" in
    Debug|Release) ;;
    *) usage ;;
esac
[ -n "$mode" ] || usage

source_dirty=$(release_git_dirty "$root")
source_commit=$(git -C "$root" rev-parse HEAD)

if [ "$mode" = "--prepare-signing" ]; then
    [ "$configuration" = Release ] || fail "release-candidate build requires --configuration Release"
    [ -n "$tag" ] || fail "release-candidate build requires --tag vX.Y.Z"
    [ -n "$build_number" ] || fail "release-candidate build requires --build-number N"
    tag_version=$(release_semver_from_tag "$tag") \
        || fail "--tag must be stable SemVer in vX.Y.Z form"
    if [ -n "$version" ] && [ "$version" != "$tag_version" ]; then
        fail "--version $version does not match tag $tag"
    fi
    version=$tag_version
    release_validate_build_number "$build_number" \
        || fail "--build-number must be a positive integer"
    [ "$source_dirty" = false ] \
        || fail "release-candidate build requires a clean git tree, including no untracked files"
    tag_commit=$(git -C "$root" rev-parse --verify "refs/tags/$tag^{commit}" 2>/dev/null) \
        || fail "tag does not exist: $tag"
    [ "$tag_commit" = "$source_commit" ] \
        || fail "tag $tag does not point to current HEAD $source_commit"
    provenance_mode=release-candidate
else
    [ -z "$tag" ] || fail "--unsigned does not accept --tag; development artifacts cannot claim a release tag"
    if [ -z "$version" ]; then
        version=$(release_project_setting "$root" MARKETING_VERSION) \
            || fail "could not resolve one MARKETING_VERSION from the Xcode project"
    fi
    if [ -z "$build_number" ]; then
        build_number=$(release_project_setting "$root" CURRENT_PROJECT_VERSION) \
            || fail "could not resolve one CURRENT_PROJECT_VERSION from the Xcode project"
    fi
    release_validate_version "$version" \
        || fail "--version must be stable SemVer in X.Y.Z form"
    release_validate_build_number "$build_number" \
        || fail "--build-number must be a positive integer"
    provenance_mode=development
fi

require_command xcodebuild
require_command xcrun
require_command ditto

build_root="$root/build/u14-app"
products_dir="$build_root/Products/$configuration"
derived_data="$build_root/DerivedData"
source_app="$products_dir/Open Chronicle.app"
source_dsym="$products_dir/Open Chronicle.app.dSYM"
dist_dir="$root/dist"
dist_app="$dist_dir/Open Chronicle.app"
dist_dsym="$dist_dir/Open Chronicle.app.dSYM"

mkdir -p "$products_dir" "$derived_data" "$dist_dir"
source_fingerprint=$(release_source_fingerprint "$root")
source_commit_time=$(git -C "$root" show -s --format=%cI HEAD)

xcodebuild \
    -project "$root/macos/OpenChronicle.xcodeproj" \
    -scheme OpenChronicle \
    -configuration "$configuration" \
    -destination 'generic/platform=macOS' \
    -derivedDataPath "$derived_data" \
    ARCHS='arm64 x86_64' \
    ONLY_ACTIVE_ARCH=NO \
    MACOSX_DEPLOYMENT_TARGET=14.0 \
    MARKETING_VERSION="$version" \
    CURRENT_PROJECT_VERSION="$build_number" \
    CONFIGURATION_BUILD_DIR="$products_dir" \
    CODE_SIGNING_ALLOWED=NO \
    CODE_SIGNING_REQUIRED=NO \
    build

[ -d "$source_app" ] || fail "Xcode did not produce $source_app"
post_build_fingerprint=$(release_source_fingerprint "$root")
[ "$post_build_fingerprint" = "$source_fingerprint" ] \
    || fail "source tree changed during the build; refusing stale provenance"

replace_directory "$dist_app"
ditto --noqtn "$source_app" "$dist_app"

provenance="$dist_app/Contents/Resources/release-provenance.json"
provenance_tmp=$(mktemp "$root/build/release-provenance.XXXXXX")
printf '%s\n' \
    '{' \
    '  "schema_version": 1,' \
    "  \"artifact_mode\": \"$provenance_mode\"," \
    "  \"source_commit\": \"$source_commit\"," \
    "  \"source_commit_time\": \"$source_commit_time\"," \
    "  \"source_fingerprint\": \"$source_fingerprint\"," \
    "  \"source_dirty\": $source_dirty," \
    "  \"git_tag\": \"$tag\"," \
    "  \"marketing_version\": \"$version\"," \
    "  \"build_number\": \"$build_number\"," \
    "  \"configuration\": \"$configuration\"" \
    '}' > "$provenance_tmp"
plutil -convert json -o /dev/null "$provenance_tmp" \
    || fail "generated provenance is invalid JSON"
chmod 644 "$provenance_tmp"
mv -f "$provenance_tmp" "$provenance"

if [ -d "$source_dsym" ]; then
    replace_directory "$dist_dsym"
    ditto --noqtn "$source_dsym" "$dist_dsym"
fi

release_validate_app_provenance_current "$root" "$dist_app" "$provenance_mode"

if [ "$mode" = "--prepare-signing" ]; then
    "$script_dir/verify-bundle.sh" "$dist_app" --unsigned-candidate
else
    "$script_dir/verify-bundle.sh" "$dist_app" --unsigned
fi

echo "built $dist_app ($configuration, ${mode#--}, provenance=$provenance_mode, dirty=$source_dirty, version=$version, build=$build_number)"
if [ -d "$dist_dsym" ]; then
    echo "built debug symbols at $dist_dsym; do not upload them through a public workflow artifact"
fi
