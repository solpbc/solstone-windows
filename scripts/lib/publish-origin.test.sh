#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH='' cd -- "$SCRIPT_DIR/../.." && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/windows-publish-origin-test.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
ASSERTIONS=0

fail() {
    echo "publish-origin.test.sh: assertion failed: $1" >&2
    echo "publish-origin.test.sh: failure after $ASSERTIONS assertions" >&2
    exit 1
}

assert() {
    "$@" || fail "$*"
    ASSERTIONS=$((ASSERTIONS + 1))
}

SOURCE="$TMP_ROOT/source"
CANDIDATE="$TMP_ROOT/candidate"
TOOL_REPO="$TMP_ROOT/tool-repo"
FAKE_BIN="$TMP_ROOT/bin"
FAKE_R2="$TMP_ROOT/r2"
WITNESS="$TMP_ROOT/witness"
TEST_ACCOUNT_ID=00000000000000000000000000000000
mkdir -p "$SOURCE" "$CANDIDATE" "$TOOL_REPO/scripts" "$FAKE_BIN" "$FAKE_R2"
: > "$WITNESS"

cp "$REPO_ROOT/scripts/publish-origin.sh" "$TOOL_REPO/scripts/publish-origin.sh"
git -C "$TOOL_REPO" init -q
git -C "$TOOL_REPO" add scripts/publish-origin.sh
git -C "$TOOL_REPO" -c user.name=test -c user.email=test@example.invalid commit -qm publisher
PUBLISHER="$TOOL_REPO/scripts/publish-origin.sh"

git -C "$SOURCE" init -q
printf '[workspace]\nmembers=[]\n' > "$SOURCE/Cargo.toml"
git -C "$SOURCE" add Cargo.toml
git -C "$SOURCE" -c user.name=test -c user.email=test@example.invalid commit -qm source
SOURCE_COMMIT="$(git -C "$SOURCE" rev-parse HEAD)"
VERSION=2.0.0
MANIFEST_NAME=solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json
FULL="Solstone-$VERSION-full.nupkg"
DELTA="Solstone-$VERSION-delta.nupkg"
SETUP="solstone-setup-$VERSION.exe"
PORTABLE=Solstone-win-Portable.zip
printf full > "$CANDIDATE/$FULL"
printf delta > "$CANDIDATE/$DELTA"
printf setup > "$CANDIDATE/$SETUP"
printf portable > "$CANDIDATE/$PORTABLE"
printf '[{"RelativeFileName":"%s","Type":"Delta"},{"RelativeFileName":"%s","Type":"Portable"},{"RelativeFileName":"%s","Type":"Installer"},{"RelativeFileName":"%s","Type":"Full"}]\n' "$DELTA" "$PORTABLE" "$SETUP" "$FULL" > "$CANDIDATE/assets.win.json"
FULL_SHA="$(sha256sum "$CANDIDATE/$FULL" | awk '{print $1}')"
DELTA_SHA="$(sha256sum "$CANDIDATE/$DELTA" | awk '{print $1}')"
FULL_SHA1="$(sha1sum "$CANDIDATE/$FULL" | awk '{print toupper($1)}')"
printf '%s %s %s\n' "$FULL_SHA1" "$FULL" "$(wc -c < "$CANDIDATE/$FULL")" > "$CANDIDATE/RELEASES"
jq -n --arg version "$VERSION" --arg full "$FULL" --arg delta "$DELTA" \
    --arg full_sha "${FULL_SHA^^}" --arg delta_sha "${DELTA_SHA^^}" \
    --argjson full_bytes "$(wc -c < "$CANDIDATE/$FULL")" --argjson delta_bytes "$(wc -c < "$CANDIDATE/$DELTA")" \
    '{Assets:[{PackageId:"Solstone",Version:$version,Type:"Full",FileName:$full,SHA256:$full_sha,Size:$full_bytes},{PackageId:"Solstone",Version:$version,Type:"Delta",FileName:$delta,SHA256:$delta_sha,Size:$delta_bytes}]}' > "$CANDIDATE/releases.win.json"

artifact_json="$(for name in assets.win.json RELEASES releases.win.json "$DELTA" "$FULL" "$SETUP" "$PORTABLE"; do jq -n --arg path "$name" --arg sha "$(sha256sum "$CANDIDATE/$name" | awk '{print $1}')" --argjson bytes "$(wc -c < "$CANDIDATE/$name")" '{path:$path,sha256:$sha,bytes:$bytes}'; done | jq -s .)"
jq -n --arg version "$VERSION" --arg source "$SOURCE_COMMIT" --argjson artifacts "$artifact_json" \
    '{schema_version:1,product:"solstone-windows",version:$version,source_commit:$source,source_dirty:false,
      target:{triple:"x86_64-pc-windows-msvc"},native_tools:{signing_mode:"signed-verified"},artifacts:$artifacts}' > "$CANDIDATE/$MANIFEST_NAME"
MANIFEST_SHA="$(sha256sum "$CANDIDATE/$MANIFEST_NAME" | awk '{print $1}')"
EXEC_SHA="$(printf candidate-executable | sha256sum | awk '{print $1}')"
jq -n --arg version "$VERSION" --arg source "$SOURCE_COMMIT" --arg manifest "$MANIFEST_SHA" --arg executable "$EXEC_SHA" \
    '{schema:"solstone.rust-release-finalization.v2",product:"solstone-windows",version:$version,target:"x86_64-pc-windows-msvc",
      source_commit:$source,candidate:{file_count:8},signing_mode:"signed-verified",
      companion_manifest:{filename:"solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json",sha256:$manifest},
      packaged_executable:{sha256:$executable,bytes:20}}' > "$TMP_ROOT/finalization.json"
jq -n --arg version "$VERSION" --arg source "$SOURCE_COMMIT" --arg manifest "$MANIFEST_SHA" \
    '{schema:"solstone.windows.origin-clearance.v1",decision:"publish",product:"solstone-windows",channel:"release",
      version:$version,source_commit:$source,companion_manifest_sha256:$manifest,recorded_founder_clearance:"test fixture only"}' > "$TMP_ROOT/clearance.json"

cat > "$FAKE_BIN/cargo" <<'EOF'
#!/usr/bin/env sh
echo 2.0.0
EOF
cat > "$FAKE_BIN/unzip" <<'EOF'
#!/usr/bin/env sh
printf candidate-executable
EOF
cat > "$FAKE_BIN/aws" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
operation=$2
shift 2
key=""
file=""
destination=""
if_none_match=""
while (($#)); do
  case "$1" in
    --key) key=$2; shift 2;;
    --body) file=$2; shift 2;;
    --if-none-match) if_none_match=$2; shift 2;;
    --endpoint-url|--region|--bucket|--content-type|--cache-control) shift 2;;
    --*) shift;;
    *) destination=$1; shift;;
  esac
done
[[ "$operation" == "get-object" ]] && witness_operation=get || witness_operation=put
printf '%s %s\n' "$witness_operation" "$key" >> "$PUBLICATION_WITNESS"
target="$FAKE_R2/$key"
if [[ "$operation" == get-object ]]; then
  if [[ ! -f "$target" ]]; then echo 'An error occurred (NoSuchKey)' >&2; exit 254; fi
  mkdir -p "$(dirname "$destination")"
  cp "$target" "$destination"
else
  mkdir -p "$(dirname "$target")"
  if [[ "${FAKE_RACE_KEY:-}" == "$key" && ! -e "$FAKE_R2/.race-fired" ]]; then
    cp "$FAKE_RACE_SOURCE" "$target"
    : > "$FAKE_R2/.race-fired"
  fi
  if [[ "$if_none_match" == '*' && -f "$target" ]]; then
    echo 'An error occurred (PreconditionFailed): status code: 412' >&2
    exit 255
  fi
  cp "$file" "$target"
fi
EOF
cat > "$FAKE_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
destination=""
while (($#)); do
  case "$1" in -o) destination=$2; shift 2;; http*) url=$1; shift;; *) shift;; esac
done
key=${url#https://updates.solstone.app/}
if [[ "${CORRUPT_PUBLIC_KEY:-}" == "$key" ]]; then printf corrupt > "$destination"; exit 0; fi
cp "$FAKE_R2/$key" "$destination"
EOF
chmod +x "$FAKE_BIN"/*

run_publish() {
    PATH="$FAKE_BIN:$PATH" PUBLICATION_WITNESS="$WITNESS" FAKE_R2="$FAKE_R2" \
        SOLSTONE_R2_ACCOUNT_ID="$TEST_ACCOUNT_ID" AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test \
        "$PUBLISHER" --candidate-dir "$CANDIDATE" --finalization-receipt "$TMP_ROOT/finalization.json" \
        --source-checkout "$SOURCE" --clearance "$TMP_ROOT/clearance.json" --receipt "$TMP_ROOT/publication.json"
}

if PATH="$FAKE_BIN:$PATH" PUBLICATION_WITNESS="$WITNESS" FAKE_R2="$FAKE_R2" \
    SOLSTONE_R2_ACCOUNT_ID="$TEST_ACCOUNT_ID" AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test \
    "$PUBLISHER" --candidate-dir "$CANDIDATE" --finalization-receipt "$TMP_ROOT/finalization.json" \
    --source-checkout "$SOURCE" --clearance "$TMP_ROOT/missing.json" --receipt "$TMP_ROOT/publication.json" >/dev/null 2>&1; then
    fail "missing clearance must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert test ! -s "$WITNESS"

printf dirty > "$TOOL_REPO/uncommitted"
if run_publish >"$TMP_ROOT/tooling-dirty.out" 2>&1; then fail "dirty publisher tooling must fail"; fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq 'tooling-unbound' "$TMP_ROOT/tooling-dirty.out"
assert test ! -s "$WITNESS"
rm "$TOOL_REPO/uncommitted"

run_publish >/dev/null
assert jq -e '.schema == "solstone.windows.origin-publication.v1" and .public_byte_verification == true' "$TMP_ROOT/publication.json" >/dev/null
last_put="$(awk '$1 == "put" {value=$2} END {print value}' "$WITNESS")"
assert test "$last_put" = "solstone-windows/releases.win.json"
assert test -f "$FAKE_R2/solstone-windows/v/$VERSION/rust-release-finalization.json"
assert cmp -s "$FAKE_R2/solstone-windows/$FULL" "$CANDIDATE/$FULL"

race_equal_key="solstone-windows/v/$VERSION/$SETUP"
rm "$FAKE_R2/$race_equal_key" "$FAKE_R2/.race-fired" 2>/dev/null || true
: > "$WITNESS"
FAKE_RACE_KEY="$race_equal_key" FAKE_RACE_SOURCE="$CANDIDATE/$SETUP" run_publish >"$TMP_ROOT/race-equal.out"
assert cmp -s "$FAKE_R2/$race_equal_key" "$CANDIDATE/$SETUP"
assert grep -Fq "present  $race_equal_key (concurrent equal create)" "$TMP_ROOT/race-equal.out"
assert test "$(grep -Fxc "get $race_equal_key" "$WITNESS")" = 2

race_conflict_key="solstone-windows/v/$VERSION/$DELTA"
rm "$FAKE_R2/$race_conflict_key" "$FAKE_R2/.race-fired"
printf concurrent-different > "$TMP_ROOT/concurrent-different"
: > "$WITNESS"
if FAKE_RACE_KEY="$race_conflict_key" FAKE_RACE_SOURCE="$TMP_ROOT/concurrent-different" run_publish >"$TMP_ROOT/race-conflict.out" 2>&1; then
    fail "concurrent different immutable create must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq "concurrent create committed different bytes at $race_conflict_key" "$TMP_ROOT/race-conflict.out"
assert test "$(cat "$FAKE_R2/$race_conflict_key")" = concurrent-different
cp "$CANDIDATE/$DELTA" "$FAKE_R2/$race_conflict_key"
rm "$FAKE_R2/.race-fired"

: > "$WITNESS"
run_publish >/dev/null
assert test -z "$(awk '$1 == "put" {print}' "$WITNESS")"

mkdir -p "$FAKE_R2/solstone-windows"
printf collision > "$FAKE_R2/solstone-windows/$FULL"
: > "$WITNESS"
if run_publish >"$TMP_ROOT/collision.out" 2>&1; then fail "immutable collision must fail"; fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq "object-immutable: solstone-windows/$FULL" "$TMP_ROOT/collision.out"
assert test -z "$(awk '$1 == "put" && $2 == "solstone-windows/releases.win.json" {print}' "$WITNESS")"

rm -rf "$FAKE_R2"
mkdir "$FAKE_R2"
: > "$WITNESS"
if SOLSTONE_ORIGIN_FAIL_AFTER=immutable run_publish >"$TMP_ROOT/interrupted.out" 2>&1; then fail "injected interruption must fail"; fi
ASSERTIONS=$((ASSERTIONS + 1))
assert test ! -f "$FAKE_R2/solstone-windows/releases.win.json"
run_publish >/dev/null
assert cmp -s "$FAKE_R2/solstone-windows/releases.win.json" "$CANDIDATE/releases.win.json"

rm "$TMP_ROOT/publication.json"
if CORRUPT_PUBLIC_KEY="solstone-windows/v/$VERSION/$FULL" run_publish >"$TMP_ROOT/public-mismatch.out" 2>&1; then
    fail "public archive byte mismatch must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq "public GET differs for v/$VERSION/$FULL" "$TMP_ROOT/public-mismatch.out"
assert test ! -f "$TMP_ROOT/publication.json"

cp "$CANDIDATE/$SETUP" "$TMP_ROOT/setup.original"
printf tampered > "$CANDIDATE/$SETUP"
: > "$WITNESS"
if run_publish >"$TMP_ROOT/tamper.out" 2>&1; then fail "candidate tamper must fail"; fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq 'digest-mismatch' "$TMP_ROOT/tamper.out"
assert test ! -s "$WITNESS"
mv "$TMP_ROOT/setup.original" "$CANDIDATE/$SETUP"

jq '.version="9.9.9"' "$TMP_ROOT/clearance.json" > "$TMP_ROOT/clearance-bad.json"
mv "$TMP_ROOT/clearance.json" "$TMP_ROOT/clearance-good.json"
mv "$TMP_ROOT/clearance-bad.json" "$TMP_ROOT/clearance.json"
: > "$WITNESS"
if run_publish >"$TMP_ROOT/clearance.out" 2>&1; then fail "mismatched clearance must fail"; fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq 'clearance-invalid' "$TMP_ROOT/clearance.out"
assert test ! -s "$WITNESS"

mv "$TMP_ROOT/clearance-good.json" "$TMP_ROOT/clearance.json"
jq '(.Assets[].Version) = "9.9.9"' "$CANDIDATE/releases.win.json" > "$FAKE_R2/solstone-windows/releases.win.json"
: > "$WITNESS"
if run_publish >"$TMP_ROOT/downgrade.out" 2>&1; then fail "live feed downgrade must fail"; fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq 'latest-refused: candidate 2.0.0 is older than live 9.9.9' "$TMP_ROOT/downgrade.out"
assert test -z "$(awk '$1 == "put" {print}' "$WITNESS")"

printf mismatch > "$SOURCE/clean-mismatch"
git -C "$SOURCE" add clean-mismatch
git -C "$SOURCE" -c user.name=test -c user.email=test@example.invalid commit -qm mismatch
: > "$WITNESS"
if run_publish >"$TMP_ROOT/source.out" 2>&1; then fail "different clean source HEAD must fail"; fi
ASSERTIONS=$((ASSERTIONS + 1))
assert grep -Fq 'source checkout HEAD differs' "$TMP_ROOT/source.out"
assert test ! -s "$WITNESS"

echo "publish-origin.test.sh: $ASSERTIONS assertions passed"
