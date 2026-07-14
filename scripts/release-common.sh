#!/bin/sh

release_provenance_error() {
    echo "release-provenance: $*" >&2
    return 1
}

release_remove_generated_directory() {
    root=$1
    candidate=$2

    [ -n "$root" ] && [ -n "$candidate" ] \
        || release_provenance_error "generated-directory cleanup requires a root and candidate" \
        || return 1
    build_root=$(CDPATH= cd -- "$root/build" 2>/dev/null && pwd -P) \
        || release_provenance_error "could not resolve generated build root: $root/build" \
        || return 1
    candidate_name=$(basename -- "$candidate")

    [ "$candidate" = "$build_root/$candidate_name" ] \
        || release_provenance_error "refusing a non-canonical generated-directory path: $candidate" \
        || return 1
    case "$candidate_name" in
        u14-dmg-work.*|u14-release-probe.*|u14-cleanup-probe.*) ;;
        *)
            release_provenance_error "refusing to clean an unrecognized generated directory: $candidate" \
                || return 1
            ;;
    esac
    [ -e "$candidate" ] || [ -L "$candidate" ] || return 0
    [ -d "$candidate" ] && [ ! -L "$candidate" ] \
        || release_provenance_error "refusing to clean a non-directory or symlink: $candidate" \
        || return 1

    if command -v trash >/dev/null 2>&1; then
        trash "$candidate" || return 1
    else
        command -v find >/dev/null 2>&1 \
            || release_provenance_error "find is required when trash is unavailable" \
            || return 1
        find -P "$candidate" -depth -delete || return 1
    fi

    [ ! -e "$candidate" ] && [ ! -L "$candidate" ] \
        || release_provenance_error "generated directory still exists after cleanup: $candidate"
}

release_semver_from_tag() {
    tag=$1
    printf '%s\n' "$tag" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
        || return 1
    printf '%s\n' "${tag#v}"
}

release_validate_version() {
    printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
}

release_validate_build_number() {
    printf '%s\n' "$1" | grep -Eq '^[1-9][0-9]*$'
}

release_validate_expected_signer() {
    identity=$1
    team_id=$2
    printf '%s\n' "$team_id" | grep -Eq '^[A-Z0-9]{10}$' \
        || release_provenance_error "expected Team ID must be 10 uppercase letters or digits" \
        || return 1
    case "$identity" in
        "Developer ID Application: "*" ($team_id)") ;;
        *)
            release_provenance_error \
                "expected signer identity must be a Developer ID Application identity ending in ($team_id)" \
                || return 1
            ;;
    esac
}

release_assert_codesign_identity() {
    target=$1
    label=$2
    expected_identity=$3
    expected_team_id=$4
    release_validate_expected_signer "$expected_identity" "$expected_team_id" \
        || return 1
    signature=$(codesign -d --verbose=4 "$target" 2>&1) \
        || release_provenance_error "could not read $label signature: $target" \
        || return 1
    leaf_authority=$(printf '%s\n' "$signature" \
        | sed -n 's/^Authority=//p' \
        | sed -n '1p')
    team_identifier=$(printf '%s\n' "$signature" \
        | sed -n 's/^TeamIdentifier=//p')
    [ "$leaf_authority" = "$expected_identity" ] \
        || release_provenance_error \
            "$label signer mismatch: expected $expected_identity, got ${leaf_authority:-missing}" \
        || return 1
    [ "$team_identifier" = "$expected_team_id" ] \
        || release_provenance_error \
            "$label TeamIdentifier mismatch: expected $expected_team_id, got ${team_identifier:-missing}" \
        || return 1
}

release_project_setting() {
    root=$1
    key=$2
    values=$(sed -n "s/.*$key = \([^;]*\);.*/\1/p" \
        "$root/macos/OpenChronicle.xcodeproj/project.pbxproj" \
        | tr -d '"' \
        | LC_ALL=C sort -u)
    count=$(printf '%s\n' "$values" | awk 'NF { count += 1 } END { print count + 0 }')
    [ "$count" -eq 1 ] || return 1
    printf '%s\n' "$values"
}

release_git_dirty() {
    root=$1
    if [ -n "$(git -C "$root" status --porcelain --untracked-files=normal)" ]; then
        printf '%s\n' true
    else
        printf '%s\n' false
    fi
}

release_source_fingerprint() {
    root=$1
    (
        cd "$root"
        git ls-files --cached --others --exclude-standard \
            | LC_ALL=C sort \
            | while IFS= read -r path; do
                if [ -L "$path" ]; then
                    mode=120000
                    hash=$(git hash-object -- "$path")
                elif [ -f "$path" ]; then
                    if [ -x "$path" ]; then
                        mode=100755
                    else
                        mode=100644
                    fi
                    hash=$(git hash-object -- "$path")
                else
                    mode=deleted
                    hash=-
                fi
                printf '%s %s\t%s\n' "$mode" "$hash" "$path"
            done
    ) | shasum -a 256 | awk '{ print $1 }'
}

release_provenance_value() {
    provenance=$1
    key=$2
    plutil -extract "$key" raw -o - "$provenance" 2>/dev/null \
        || release_provenance_error "$key is missing from $provenance"
}

release_read_app_provenance() {
    app=$1
    expected_mode=$2
    RELEASE_PROVENANCE_FILE="$app/Contents/Resources/release-provenance.json"
    [ -f "$RELEASE_PROVENANCE_FILE" ] \
        || release_provenance_error "bundle provenance is missing: $RELEASE_PROVENANCE_FILE" \
        || return 1
    plutil -convert json -o /dev/null "$RELEASE_PROVENANCE_FILE" \
        || release_provenance_error "bundle provenance is not valid JSON" \
        || return 1

    RELEASE_PROVENANCE_SCHEMA=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" schema_version) \
        || return 1
    RELEASE_PROVENANCE_MODE=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" artifact_mode) \
        || return 1
    RELEASE_PROVENANCE_COMMIT=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" source_commit) \
        || return 1
    RELEASE_PROVENANCE_FINGERPRINT=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" source_fingerprint) \
        || return 1
    RELEASE_PROVENANCE_DIRTY=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" source_dirty) \
        || return 1
    RELEASE_PROVENANCE_TAG=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" git_tag) \
        || return 1
    RELEASE_PROVENANCE_VERSION=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" marketing_version) \
        || return 1
    RELEASE_PROVENANCE_BUILD=$(release_provenance_value "$RELEASE_PROVENANCE_FILE" build_number) \
        || return 1

    [ "$RELEASE_PROVENANCE_SCHEMA" = 1 ] \
        || release_provenance_error "unsupported provenance schema: $RELEASE_PROVENANCE_SCHEMA" \
        || return 1
    [ "$RELEASE_PROVENANCE_MODE" = "$expected_mode" ] \
        || release_provenance_error "expected $expected_mode provenance, got $RELEASE_PROVENANCE_MODE" \
        || return 1
    printf '%s\n' "$RELEASE_PROVENANCE_COMMIT" | grep -Eq '^[0-9a-f]{40}$' \
        || release_provenance_error "invalid source commit: $RELEASE_PROVENANCE_COMMIT" \
        || return 1
    printf '%s\n' "$RELEASE_PROVENANCE_FINGERPRINT" | grep -Eq '^[0-9a-f]{64}$' \
        || release_provenance_error "invalid source fingerprint: $RELEASE_PROVENANCE_FINGERPRINT" \
        || return 1
    case "$RELEASE_PROVENANCE_DIRTY" in
        true|false) ;;
        *) release_provenance_error "invalid source_dirty value: $RELEASE_PROVENANCE_DIRTY" || return 1 ;;
    esac
    release_validate_version "$RELEASE_PROVENANCE_VERSION" \
        || release_provenance_error "invalid marketing version: $RELEASE_PROVENANCE_VERSION" \
        || return 1
    release_validate_build_number "$RELEASE_PROVENANCE_BUILD" \
        || release_provenance_error "invalid build number: $RELEASE_PROVENANCE_BUILD" \
        || return 1

    info="$app/Contents/Info.plist"
    plist_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$info" 2>/dev/null) \
        || release_provenance_error "CFBundleShortVersionString is missing" \
        || return 1
    plist_build=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$info" 2>/dev/null) \
        || release_provenance_error "CFBundleVersion is missing" \
        || return 1
    [ "$plist_version" = "$RELEASE_PROVENANCE_VERSION" ] \
        || release_provenance_error "plist version $plist_version does not match provenance $RELEASE_PROVENANCE_VERSION" \
        || return 1
    [ "$plist_build" = "$RELEASE_PROVENANCE_BUILD" ] \
        || release_provenance_error "plist build $plist_build does not match provenance $RELEASE_PROVENANCE_BUILD" \
        || return 1

    if [ "$expected_mode" = release-candidate ]; then
        [ "$RELEASE_PROVENANCE_DIRTY" = false ] \
            || release_provenance_error "release-candidate provenance cannot be dirty" \
            || return 1
        tag_version=$(release_semver_from_tag "$RELEASE_PROVENANCE_TAG") \
            || release_provenance_error "release candidate has invalid tag: $RELEASE_PROVENANCE_TAG" \
            || return 1
        [ "$tag_version" = "$RELEASE_PROVENANCE_VERSION" ] \
            || release_provenance_error "tag $RELEASE_PROVENANCE_TAG does not match version $RELEASE_PROVENANCE_VERSION" \
            || return 1
    else
        [ -z "$RELEASE_PROVENANCE_TAG" ] \
            || release_provenance_error "development provenance must not claim a git tag" \
            || return 1
    fi
}

release_validate_app_provenance_current() {
    root=$1
    app=$2
    expected_mode=$3
    release_read_app_provenance "$app" "$expected_mode" || return 1

    current_commit=$(git -C "$root" rev-parse HEAD)
    current_fingerprint=$(release_source_fingerprint "$root")
    current_dirty=$(release_git_dirty "$root")
    [ "$RELEASE_PROVENANCE_COMMIT" = "$current_commit" ] \
        || release_provenance_error "bundle commit $RELEASE_PROVENANCE_COMMIT does not match current HEAD $current_commit" \
        || return 1
    [ "$RELEASE_PROVENANCE_FINGERPRINT" = "$current_fingerprint" ] \
        || release_provenance_error "bundle source fingerprint is stale or mismatched" \
        || return 1
    [ "$RELEASE_PROVENANCE_DIRTY" = "$current_dirty" ] \
        || release_provenance_error "bundle dirty state does not match the current source tree" \
        || return 1

    if [ "$expected_mode" = release-candidate ]; then
        [ "$current_dirty" = false ] \
            || release_provenance_error "signed packaging requires a clean git tree" \
            || return 1
        tag_commit=$(git -C "$root" rev-parse --verify "refs/tags/$RELEASE_PROVENANCE_TAG^{commit}" 2>/dev/null) \
            || release_provenance_error "tag does not exist: $RELEASE_PROVENANCE_TAG" \
            || return 1
        [ "$tag_commit" = "$current_commit" ] \
            || release_provenance_error "tag $RELEASE_PROVENANCE_TAG does not point to current HEAD" \
            || return 1
    fi
}
