#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SCANNER=$SCRIPT_DIR/doc-stale-scan.sh
ASSERTIONS=0
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/doc-stale-scan-test.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  echo "doc-stale-scan.test.sh: assertion failed: $1" >&2
  echo "doc-stale-scan.test.sh: failure after $ASSERTIONS assertions" >&2
  exit 1
}

assert_contains() {
  label=$1
  haystack=$2
  needle=$3
  case "$haystack" in
    *"$needle"*) ;;
    *) fail "$label (missing: $needle)" ;;
  esac
  ASSERTIONS=$((ASSERTIONS + 1))
}

GOOD=$TMP_ROOT/good
mkdir "$GOOD"
printf '%s\n\n%s\n\n%s\n' \
  'R2 is the authoritative update feed. The GitHub Releases mirror is optional and non-authoritative.' \
  'No GitHub mirror is required, and it cannot gate a release.' \
  'Never hand-chain cargo build --locked before vpk pack; use make package and the aggregate provenance publisher.' \
  > "$GOOD/good.md"
good_output=$(sh "$SCANNER" --root "$GOOD" good.md 2>&1) || fail "qualified examples must pass"
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "positive scanned-file count" "$good_output" "scanned 1 eligible files"
assert_contains "good result" "$good_output" "no violations"

AUTHORITY=$TMP_ROOT/authority
mkdir "$AUTHORITY"
printf '%s\n' 'GitHub Releases serves the authoritative update feed for Windows.' > "$AUTHORITY/authority.md"
if authority_output=$(sh "$SCANNER" --root "$AUTHORITY" authority.md 2>&1); then
  fail "github authority mutation must be rejected"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "github authority rule" "$authority_output" "authority.md:1:github-authority:"
assert_contains "github authority remediation" "$authority_output" "name R2 as authoritative"

AUTHORITY_ADJACENT=$TMP_ROOT/authority-adjacent
mkdir "$AUTHORITY_ADJACENT"
printf '%s\n' \
  '- GitHub Releases hosts the update feed for Windows.' \
  '- A separate developer preview mirror is optional and non-authoritative.' \
  > "$AUTHORITY_ADJACENT/authority-adjacent.md"
if authority_adjacent_output=$(sh "$SCANNER" --root "$AUTHORITY_ADJACENT" authority-adjacent.md 2>&1); then
  fail "adjacent authority qualifier must not mask a violating list item"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "adjacent github authority rule" "$authority_adjacent_output" "authority-adjacent.md:1:github-authority:"

MIRROR=$TMP_ROOT/mirror
mkdir "$MIRROR"
printf '%s\n' 'The GitHub mirror must succeed and gates every release.' > "$MIRROR/mirror.md"
if mirror_output=$(sh "$SCANNER" --root "$MIRROR" mirror.md 2>&1); then
  fail "required mirror mutation must be rejected"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "required mirror rule" "$mirror_output" "mirror.md:1:required-mirror:"
assert_contains "required mirror remediation" "$mirror_output" "cannot gate release"

MIRROR_ADJACENT=$TMP_ROOT/mirror-adjacent
mkdir "$MIRROR_ADJACENT"
printf '%s\n' \
  '| Surface | Policy |' \
  '| --- | --- |' \
  '| GitHub mirror | Must succeed and gates release |' \
  '| Documentation mirror | Optional and cannot gate release |' \
  > "$MIRROR_ADJACENT/mirror-adjacent.md"
if mirror_adjacent_output=$(sh "$SCANNER" --root "$MIRROR_ADJACENT" mirror-adjacent.md 2>&1); then
  fail "adjacent mirror qualifier must not mask a violating table row"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "adjacent required mirror rule" "$mirror_adjacent_output" "mirror-adjacent.md:3:required-mirror:"

CHAIN=$TMP_ROOT/chain
mkdir "$CHAIN"
printf '%s\n' '```sh' 'cargo build --locked --release' 'vpk pack --packId Solstone' '```' > "$CHAIN/chain.md"
if chain_output=$(sh "$SCANNER" --root "$CHAIN" chain.md 2>&1); then
  fail "build-to-pack hand-chain mutation must be rejected"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "build-to-pack hand-chain rule" "$chain_output" "chain.md:1:hand-chain:"
assert_contains "build-to-pack remediation" "$chain_output" "make package/finalizer"

CHAIN_ADJACENT=$TMP_ROOT/chain-adjacent
mkdir "$CHAIN_ADJACENT"
printf '%s\n' \
  '```sh' \
  'cargo build --locked --release' \
  'vpk pack --packId Solstone' \
  '```' \
  '- Never hand-chain a separate diagnostic build with publication.' \
  > "$CHAIN_ADJACENT/chain-adjacent.md"
if chain_adjacent_output=$(sh "$SCANNER" --root "$CHAIN_ADJACENT" chain-adjacent.md 2>&1); then
  fail "adjacent prohibition must not mask a violating fenced command block"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "adjacent hand-chain rule" "$chain_adjacent_output" "chain-adjacent.md:1:hand-chain:"

UPLOAD=$TMP_ROOT/upload
mkdir "$UPLOAD"
printf '%s\n' '```sh' 'vpk pack --packId Solstone' 'gh release upload v0.2.11 artifact.zip' '```' > "$UPLOAD/upload.md"
if upload_output=$(sh "$SCANNER" --root "$UPLOAD" upload.md 2>&1); then
  fail "pack-to-upload hand-chain mutation must be rejected"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "pack-to-upload hand-chain rule" "$upload_output" "upload.md:1:hand-chain:"

EMPTY=$TMP_ROOT/empty
mkdir "$EMPTY"
if empty_output=$(sh "$SCANNER" --root "$EMPTY" 2>&1); then
  fail "zero-file scan must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "zero-file guard" "$empty_output" "scanned zero eligible files"

repo_output=$(sh "$SCANNER" 2>&1) || fail "corrected repository docs must pass"
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "repository scan count" "$repo_output" "eligible files; no violations"

echo "doc-stale-scan.test.sh: $ASSERTIONS assertions passed"
