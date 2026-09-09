#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Publish one exact finalized Windows candidate to the existing flat Velopack
# origin. The direct publish-r2 entry point remains locked; this is the
# candidate-verifying aggregate boundary used only after recorded clearance.

set -euo pipefail

umask 077
export LC_ALL=C

PRODUCT="solstone-windows"
BUCKET="${SOLSTONE_ORIGIN_BUCKET:-solstone-updates}"
PREFIX="$PRODUCT"
ORIGIN_URL="https://updates.solstone.app"
FEED="releases.win.json"
R2_ACCOUNT_ID="${SOLSTONE_R2_ACCOUNT_ID:-}"
R2_ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"

die() {
    printf 'windows release origin publisher: %s\n' "$1" >&2
    exit 1
}

usage() {
    echo "usage: publish-origin.sh --candidate-dir <directory> --finalization-receipt <json> --source-checkout <directory> --clearance <json> --receipt <json> [--dry-run]" >&2
    exit 2
}

candidate_directory=""
finalization_receipt=""
source_checkout=""
clearance=""
publication_receipt=""
dry_run=false
while (($# > 0)); do
    case "$1" in
        --candidate-dir)
            (($# >= 2)) || usage
            candidate_directory="$2"
            shift 2
            ;;
        --finalization-receipt)
            (($# >= 2)) || usage
            finalization_receipt="$2"
            shift 2
            ;;
        --source-checkout)
            (($# >= 2)) || usage
            source_checkout="$2"
            shift 2
            ;;
        --clearance)
            (($# >= 2)) || usage
            clearance="$2"
            shift 2
            ;;
        --receipt)
            (($# >= 2)) || usage
            publication_receipt="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=true
            shift
            ;;
        *) usage ;;
    esac
done
[[ -n "$candidate_directory" && -n "$finalization_receipt" && -n "$source_checkout" && -n "$clearance" && -n "$publication_receipt" ]] || usage

required_tools=(awk cargo cat cmp curl date dirname find git grep jq mkdir mktemp mv realpath rm sha1sum sha256sum sort tr unzip wc)
$dry_run || required_tools+=(aws)
for tool in "${required_tools[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || die "required release tool is unavailable: $tool"
done
if ! $dry_run; then
    [[ "$R2_ACCOUNT_ID" =~ ^[0-9a-f]{32}$ ]] || die "origin-auth-missing: SOLSTONE_R2_ACCOUNT_ID must be lowercase 32-hex"
    [[ -n "${AWS_ACCESS_KEY_ID:-}" ]] || die "origin-auth-missing: AWS_ACCESS_KEY_ID is required"
    [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]] || die "origin-auth-missing: AWS_SECRET_ACCESS_KEY is required"
    export AWS_EC2_METADATA_DISABLED=true
fi

script_directory="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
tooling_root="$(git -C "$script_directory" rev-parse --show-toplevel 2>/dev/null)" ||
    die "publisher is not inside a Git worktree"
tooling_commit="$(git -C "$tooling_root" rev-parse HEAD)" ||
    die "publisher tooling commit is unavailable"
[[ -z "$(git -C "$tooling_root" status --porcelain=v1 --untracked-files=all)" ]] ||
    die "tooling-unbound: publisher checkout must be clean"

[[ -d "$candidate_directory" && ! -L "$candidate_directory" ]] ||
    die "candidate-set-invalid: candidate directory must be a real directory"
candidate_directory="$(realpath "$candidate_directory")" ||
    die "candidate-set-invalid: candidate directory could not be resolved"

[[ -f "$finalization_receipt" && ! -L "$finalization_receipt" ]] ||
    die "candidate-set-invalid: finalization receipt must be one regular file"
finalization_receipt="$(realpath "$finalization_receipt")" ||
    die "candidate-set-invalid: finalization receipt could not be resolved"

[[ -f "$clearance" && ! -L "$clearance" ]] ||
    die "clearance-invalid: clearance must be one regular file"
clearance="$(realpath "$clearance")" || die "clearance-invalid: clearance could not be resolved"

[[ -d "$source_checkout" && ! -L "$source_checkout" ]] ||
    die "source-unbound: source checkout must be a real directory"
source_checkout="$(realpath "$source_checkout")" || die "source-unbound: source checkout could not be resolved"
source_root="$(git -C "$source_checkout" rev-parse --show-toplevel 2>/dev/null)" ||
    die "source-unbound: source checkout is not a Git worktree"
source_root="$(realpath "$source_root")"
[[ "$source_root" == "$source_checkout" ]] ||
    die "source-unbound: source checkout must name the worktree root"

mapfile -d '' -t manifests < <(
    find "$candidate_directory" -mindepth 1 -maxdepth 1 -type f \
        -name 'solstone-windows-*-pc-windows-msvc.rust-release-manifest.json' -print0
)
((${#manifests[@]} == 1)) ||
    die "candidate-set-invalid: candidate must contain exactly one companion manifest"
manifest="${manifests[0]}"

version="$(jq -er '.version' "$manifest")" || die "candidate-set-invalid: manifest version is unavailable"
source_commit="$(jq -er '.source_commit' "$manifest")" || die "candidate-set-invalid: manifest source commit is unavailable"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "candidate-set-invalid: version must be strict SemVer"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || die "candidate-set-invalid: source commit must be lowercase 40-hex"

manifest_name="solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json"
[[ "${manifest##*/}" == "$manifest_name" ]] || die "candidate-set-invalid: companion manifest basename is unexpected"
full="Solstone-$version-full.nupkg"
delta="Solstone-$version-delta.nupkg"
portable="Solstone-win-Portable.zip"
setup="solstone-setup-$version.exe"
assets="assets.win.json"
releases="releases.win.json"
release_index="RELEASES"
expected_names=("$assets" "$release_index" "$releases" "$delta" "$full" "$setup" "$portable" "$manifest_name")
mapfile -t expected_names < <(printf '%s\n' "${expected_names[@]}" | sort)

actual_names=()
while IFS= read -r -d '' path; do
    [[ -f "$path" && ! -L "$path" ]] || die "candidate-set-invalid: candidate entries must be regular files"
    actual_names+=("${path##*/}")
done < <(find "$candidate_directory" -mindepth 1 -maxdepth 1 -print0 | sort -z)
((${#actual_names[@]} == ${#expected_names[@]})) ||
    die "candidate-set-invalid: candidate file set is incomplete or unlisted"
for index in "${!expected_names[@]}"; do
    [[ "${actual_names[$index]}" == "${expected_names[$index]}" ]] ||
        die "candidate-set-invalid: candidate file set is incomplete or unlisted"
done

jq -e --arg version "$version" --arg source "$source_commit" '
    .schema_version == 1 and .product == "solstone-windows" and
    .version == $version and .source_commit == $source and .source_dirty == false and
    .target.triple == "x86_64-pc-windows-msvc" and
    .native_tools.signing_mode == "signed-verified" and
    (.artifacts | type) == "array" and (.artifacts | length) == 7
' "$manifest" >/dev/null || die "candidate-set-invalid: companion manifest does not describe one signed Windows candidate"

manifest_sha256="$(sha256sum "$manifest" | awk '{print $1}')"
jq -e --arg version "$version" --arg source "$source_commit" --arg manifest_name "$manifest_name" --arg manifest_sha "$manifest_sha256" '
    .schema == "solstone.rust-release-finalization.v2" and
    .product == "solstone-windows" and .version == $version and .target == "x86_64-pc-windows-msvc" and
    .source_commit == $source and .candidate.file_count == 8 and .signing_mode == "signed-verified" and
    .companion_manifest.filename == $manifest_name and .companion_manifest.sha256 == $manifest_sha
' "$finalization_receipt" >/dev/null || die "candidate-set-invalid: finalization receipt does not bind this signed candidate"

manifest_artifact_names="$(jq -er '.artifacts[].path' "$manifest" | sort)" ||
    die "candidate-set-invalid: manifest artifact names are unavailable"
expected_artifact_names="$(printf '%s\n' "$assets" "$release_index" "$releases" "$delta" "$full" "$setup" "$portable" | sort)"
[[ "$manifest_artifact_names" == "$expected_artifact_names" ]] ||
    die "candidate-set-invalid: manifest artifact set is incomplete or unlisted"

while IFS=$'\t' read -r name expected_sha expected_bytes; do
    [[ "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] || die "candidate-set-invalid: invalid artifact name"
    file="$candidate_directory/$name"
    observed_sha="$(sha256sum "$file" | awk '{print $1}')"
    observed_bytes="$(wc -c < "$file" | tr -d ' ')"
    [[ "$observed_sha" == "$expected_sha" && "$observed_bytes" == "$expected_bytes" ]] ||
        die "digest-mismatch: $name differs from the companion manifest"
done < <(jq -r '.artifacts[] | [.path, .sha256, (.bytes | tostring)] | @tsv' "$manifest")

stage_root="$(mktemp -d "${TMPDIR:-/tmp}/$PRODUCT-publish-origin.XXXXXX")"
cleanup() { rm -rf -- "$stage_root"; }
trap cleanup EXIT HUP INT TERM

expected_executable_sha="$(jq -er '.packaged_executable.sha256' "$finalization_receipt")" ||
    die "candidate-set-invalid: packaged executable digest is unavailable"
expected_executable_bytes="$(jq -er '.packaged_executable.bytes' "$finalization_receipt")" ||
    die "candidate-set-invalid: packaged executable size is unavailable"
unzip -p "$candidate_directory/$full" 'lib/app/solstone-windows-app.exe' > "$stage_root/nupkg-app.exe" ||
    die "candidate-set-invalid: full package does not contain the app executable"
unzip -p "$candidate_directory/$portable" 'current/solstone-windows-app.exe' > "$stage_root/portable-app.exe" ||
    die "candidate-set-invalid: portable package does not contain the app executable"
for executable in "$stage_root/nupkg-app.exe" "$stage_root/portable-app.exe"; do
    [[ "$(sha256sum "$executable" | awk '{print $1}')" == "$expected_executable_sha" &&
        "$(wc -c < "$executable" | tr -d ' ')" == "$expected_executable_bytes" ]] ||
        die "digest-mismatch: packaged executable differs from finalization receipt"
done

release_full_sha1="$(sha1sum "$candidate_directory/$full" | awk '{print toupper($1)}')"
release_full_bytes="$(wc -c < "$candidate_directory/$full" | tr -d ' ')"
grep -Fqx "$release_full_sha1 $full $release_full_bytes" "$candidate_directory/$release_index" ||
    die "candidate-set-invalid: RELEASES does not name the exact current full package"

jq -e --arg version "$version" --arg full "$full" --arg delta "$delta" \
    --arg full_sha "$(sha256sum "$candidate_directory/$full" | awk '{print toupper($1)}')" \
    --arg delta_sha "$(sha256sum "$candidate_directory/$delta" | awk '{print toupper($1)}')" \
    --argjson full_bytes "$(wc -c < "$candidate_directory/$full")" \
    --argjson delta_bytes "$(wc -c < "$candidate_directory/$delta")" '
    ([.Assets[] | select(.Version == $version and .Type == "Full" and .FileName == $full and .SHA256 == $full_sha and .Size == $full_bytes)] | length) == 1 and
    ([.Assets[] | select(.Version == $version and .Type == "Delta" and .FileName == $delta and .SHA256 == $delta_sha and .Size == $delta_bytes)] | length) == 1
' "$candidate_directory/$releases" >/dev/null ||
    die "candidate-set-invalid: releases.win.json does not bind the exact current packages"

jq -e --arg version "$version" --arg full "$full" --arg delta "$delta" --arg portable "$portable" --arg setup "$setup" '
    (map([.RelativeFileName, .Type])) == [
        [$delta, "Delta"], [$portable, "Portable"], [$setup, "Installer"], [$full, "Full"]
    ]
' "$candidate_directory/$assets" >/dev/null ||
    die "candidate-set-invalid: assets.win.json does not name the exact current artifacts"

[[ -z "$(git -C "$source_root" status --porcelain=v1 --untracked-files=all)" ]] ||
    die "source-unbound: source checkout must be clean"
[[ "$(git -C "$source_root" rev-parse HEAD)" == "$source_commit" ]] ||
    die "source-unbound: source checkout HEAD differs from the companion manifest"
source_version="$(cargo run --manifest-path "$source_root/Cargo.toml" --locked -q -p xtask -- version-gate --root "$source_root")" ||
    die "source-unbound: source version gate failed"
[[ "$source_version" == "$version" ]] || die "source-unbound: source version differs from the candidate"

jq -e --arg version "$version" --arg source "$source_commit" --arg manifest "$manifest_sha256" '
    .schema == "solstone.windows.origin-clearance.v1" and .decision == "publish" and
    .product == "solstone-windows" and .channel == "release" and
    .version == $version and .source_commit == $source and
    .companion_manifest_sha256 == $manifest and
    (.recorded_founder_clearance | type) == "string" and (.recorded_founder_clearance | length) > 0
' "$clearance" >/dev/null || die "clearance-invalid: recorded clearance does not authorize this exact candidate and release channel"

content_type_for() {
    case "$1" in
        *.json) echo "application/json" ;;
        *.zip) echo "application/zip" ;;
        RELEASES) echo "text/plain; charset=utf-8" ;;
        *) echo "application/octet-stream" ;;
    esac
}

remote_get() {
    local key="$1" destination="$2" log="$stage_root/r2-get.log"
    if aws s3api get-object --endpoint-url "$R2_ENDPOINT" --region auto \
        --bucket "$BUCKET" --key "$key" "$destination" >"$log" 2>&1; then
        return 0
    fi
    if grep -qF 'NoSuchKey' "$log"; then
        rm -f "$destination"
        return 1
    fi
    cat "$log" >&2
    die "origin-unreachable: could not read $key"
}

remote_put() {
    local key="$1" file="$2" cache_control="$3"
    aws s3api put-object --endpoint-url "$R2_ENDPOINT" --region auto \
        --bucket "$BUCKET" --key "$key" --body "$file" \
        --content-type "$(content_type_for "${key##*/}")" --cache-control "$cache_control" >/dev/null ||
        die "origin-unreachable: could not write $key"
}

# A successful If-None-Match:* PUT is the create-only boundary. Exit 3 means a
# concurrent writer won; callers must fetch and compare the committed bytes.
remote_put_create_only() {
    local key="$1" file="$2" cache_control="$3" log="$stage_root/r2-create.log"
    if aws s3api put-object --endpoint-url "$R2_ENDPOINT" --region auto \
        --bucket "$BUCKET" --key "$key" --body "$file" --if-none-match '*' \
        --content-type "$(content_type_for "${key##*/}")" --cache-control "$cache_control" >"$log" 2>&1; then
        return 0
    fi
    if grep -Eq 'PreconditionFailed|ConditionalRequestConflict|status code: (409|412)' "$log"; then
        return 3
    fi
    cat "$log" >&2
    die "origin-unreachable: could not create $key"
}

checkpoint() {
    [[ "${SOLSTONE_ORIGIN_FAIL_AFTER:-}" == "$1" ]] || return 0
    die "injected-failure $1"
}

version_is_not_older() {
    local candidate="$1" existing="$2"
    local -a candidate_parts existing_parts
    local index
    IFS='.' read -r -a candidate_parts <<<"$candidate"
    IFS='.' read -r -a existing_parts <<<"$existing"
    for index in 0 1 2; do
        ((10#${candidate_parts[$index]} > 10#${existing_parts[$index]})) && return 0
        ((10#${candidate_parts[$index]} < 10#${existing_parts[$index]})) && return 1
    done
    return 0
}

put_immutable() {
    local key="$1" file="$2" remote="$stage_root/remote-object" create_status
    if remote_get "$key" "$remote"; then
        cmp -s "$remote" "$file" || die "object-immutable: $key already exists with different bytes"
        printf '  present  %s\n' "$key"
    else
        if remote_put_create_only "$key" "$file" "public, max-age=31536000, immutable"; then
            create_status=0
        else
            create_status=$?
        fi
        if ((create_status == 3)); then
            remote_get "$key" "$remote" || die "origin-conflict: $key disappeared after a concurrent create"
            cmp -s "$remote" "$file" || die "object-immutable: concurrent create committed different bytes at $key"
            printf '  present  %s (concurrent equal create)\n' "$key"
            rm -f "$remote"
            return 0
        fi
        ((create_status == 0)) || die "origin-unreachable: create-only PUT failed for $key"
        remote_get "$key" "$remote" || die "origin-unreachable: $key was absent after upload"
        cmp -s "$remote" "$file" || die "digest-mismatch: $key differs after upload"
        printf '  put      %s\n' "$key"
    fi
    rm -f "$remote"
}

put_mutable() {
    local key="$1" file="$2" remote="$stage_root/remote-object"
    if remote_get "$key" "$remote" && cmp -s "$remote" "$file"; then
        printf '  present  %s\n' "$key"
    else
        remote_put "$key" "$file" "no-cache"
        remote_get "$key" "$remote" || die "origin-unreachable: $key was absent after upload"
        cmp -s "$remote" "$file" || die "digest-mismatch: $key differs after upload"
        printf '  put      %s\n' "$key"
    fi
    rm -f "$remote"
}

archive_names=("${expected_names[@]}" "rust-release-finalization.json")
flat_immutable=("$delta" "$full" "$setup")
flat_mutable=("$portable" "$manifest_name" "$assets" "$release_index")

printf 'validated %s %s from source %s with tooling %s\n' "$PRODUCT" "$version" "$source_commit" "$tooling_commit"
if $dry_run; then
    for name in "${archive_names[@]}"; do printf '  would publish %s/%s/v/%s/%s\n' "$ORIGIN_URL" "$PREFIX" "$version" "$name"; done
    for name in "${flat_immutable[@]}" "${flat_mutable[@]}" "$FEED"; do printf '  would publish %s/%s/%s\n' "$ORIGIN_URL" "$PREFIX" "$name"; done
    printf 'dry-run complete; no origin calls and no receipt written\n'
    exit 0
fi

current_feed="$stage_root/current-feed.json"
if remote_get "$PREFIX/$FEED" "$current_feed"; then
    current_versions_file="$stage_root/current-versions"
    jq -er '.Assets | if type == "array" and length > 0 then .[].Version else error("invalid feed") end' "$current_feed" > "$current_versions_file" ||
        die "latest-invalid: current releases.win.json is malformed"
    mapfile -t current_versions < "$current_versions_file"
    highest_current="0.0.0"
    for current_version in "${current_versions[@]}"; do
        [[ "$current_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
            die "latest-invalid: current releases.win.json contains a non-release version"
        if version_is_not_older "$current_version" "$highest_current"; then
            highest_current="$current_version"
        fi
    done
    version_is_not_older "$version" "$highest_current" ||
        die "latest-refused: candidate $version is older than live $highest_current"
fi
rm -f "$current_feed"

for name in "${archive_names[@]}"; do
    file="$candidate_directory/$name"
    [[ "$name" == "rust-release-finalization.json" ]] && file="$finalization_receipt"
    put_immutable "$PREFIX/v/$version/$name" "$file"
    checkpoint "archive:$name"
done
checkpoint "archive"

for name in "${archive_names[@]}"; do
    file="$candidate_directory/$name"
    [[ "$name" == "rust-release-finalization.json" ]] && file="$finalization_receipt"
    public_copy="$stage_root/public-archive-$name"
    curl --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 -fsS \
        "$ORIGIN_URL/$PREFIX/v/$version/$name" -o "$public_copy" ||
        die "origin-unreachable: public GET failed for v/$version/$name"
    cmp -s "$public_copy" "$file" ||
        die "digest-mismatch: public GET differs for v/$version/$name"
done
checkpoint "archive-public"

for name in "${flat_immutable[@]}"; do
    put_immutable "$PREFIX/$name" "$candidate_directory/$name"
    checkpoint "immutable:$name"
done
checkpoint "immutable"

for name in "${flat_immutable[@]}"; do
    public_copy="$stage_root/public-immutable-$name"
    curl --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 -fsS \
        "$ORIGIN_URL/$PREFIX/$name" -o "$public_copy" ||
        die "origin-unreachable: public GET failed for $name"
    cmp -s "$public_copy" "$candidate_directory/$name" ||
        die "digest-mismatch: public GET differs for $name"
done
checkpoint "immutable-public"

for name in "${flat_mutable[@]}"; do
    put_mutable "$PREFIX/$name" "$candidate_directory/$name"
    checkpoint "mutable:$name"
done
checkpoint "before-feed"
put_mutable "$PREFIX/$FEED" "$candidate_directory/$FEED"
checkpoint "feed"

for name in "${flat_immutable[@]}" "${flat_mutable[@]}" "$FEED"; do
    public_copy="$stage_root/public-$name"
    curl --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 -fsS \
        "$ORIGIN_URL/$PREFIX/$name" -o "$public_copy" ||
        die "origin-unreachable: public GET failed for $name"
    cmp -s "$public_copy" "$candidate_directory/$name" ||
        die "digest-mismatch: public GET differs for $name"
done

receipt_parent="$(dirname -- "$publication_receipt")"
mkdir -p "$receipt_parent"
receipt_temp="$receipt_parent/.origin-publication.$$.tmp"
published_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
jq -n --arg product "$PRODUCT" --arg version "$version" --arg source "$source_commit" \
    --arg tooling "$tooling_commit" --arg manifest "$manifest_sha256" --arg origin "$ORIGIN_URL/$PREFIX" \
    --arg published "$published_at" --arg clearance_sha "$(sha256sum "$clearance" | awk '{print $1}')" \
    '{schema:"solstone.windows.origin-publication.v1", product:$product, version:$version,
      source_commit:$source, tooling_commit:$tooling, companion_manifest_sha256:$manifest,
      origin:$origin, ordering:"immutable archive, flat versioned artifacts, mutable metadata, releases.win.json last",
      clearance_sha256:$clearance_sha, public_byte_verification:true, published_at:$published}' > "$receipt_temp" ||
    die "receipt-write-failed: could not render publication receipt"
mv "$receipt_temp" "$publication_receipt" || die "receipt-write-failed: could not promote publication receipt"
printf 'published and verified %s %s; receipt: %s\n' "$PRODUCT" "$version" "$publication_receipt"
