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
cat > "$FAKE_BIN/wrangler" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
operation=$3
object=$4
key=${object#*/}
shift 4
file=""
while (($#)); do
  case "$1" in --file) file=$2; shift 2;; *) shift;; esac
done
printf '%s %s\n' "$operation" "$key" >> "$PUBLICATION_WITNESS"
target="$FAKE_R2/$key"
if [[ "$operation" == get ]]; then
  if [[ ! -f "$target" ]]; then echo 'The specified key does not exist' >&2; exit 1; fi
  mkdir -p "$(dirname "$file")"
  cp "$target" "$file"
else
  mkdir -p "$(dirname "$target")"
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
        "$PUBLISHER" --candidate-dir "$CANDIDATE" --finalization-receipt "$TMP_ROOT/finalization.json" \
        --source-checkout "$SOURCE" --clearance "$TMP_ROOT/clearance.json" --receipt "$TMP_ROOT/publication.json"
}

if PATH="$FAKE_BIN:$PATH" PUBLICATION_WITNESS="$WITNESS" FAKE_R2="$FAKE_R2" \
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

: > "$WITNESS"
run_publish >/dev/null
assert test -z "$(awk '$1 == "put" {print}' "$WITNESS")"

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
