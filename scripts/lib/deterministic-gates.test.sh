#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ASSERTIONS=0
TMP_ROOT=""

cleanup() {
  if [ -n "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi
}
trap cleanup EXIT HUP INT TERM

fail() {
  echo "deterministic-gates.test.sh: assertion failed: $1" >&2
  echo "deterministic-gates.test.sh: failure after $ASSERTIONS assertions" >&2
  exit 1
}

assert_eq() {
  label=$1
  expected=$2
  actual=$3
  if [ "$actual" != "$expected" ]; then
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    fail "$label"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
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

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/deterministic-gates.XXXXXX")

FAKE_RUSTC="$TMP_ROOT/fake-rustc"
cat > "$FAKE_RUSTC" <<'EOF'
#!/usr/bin/env sh
echo "rustc ${FAKE_RELEASE} (probe 1970-01-01)"
echo "binary: rustc"
echo "release: ${FAKE_RELEASE}"
EOF
chmod +x "$FAKE_RUSTC"

if toolchain_output=$(RUSTC="$FAKE_RUSTC" FAKE_RELEASE=9.9.9 sh "$REPO_ROOT/scripts/preflight-toolchain.sh" 2>&1); then
  fail "toolchain skew must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq \
  "toolchain skew error" \
  "ERROR: Rust toolchain mismatch: expected 1.96.0, actual 9.9.9. Run 'make rust-toolchain'." \
  "$toolchain_output"

toolchain_output=$(RUSTC="$FAKE_RUSTC" FAKE_RELEASE=1.96.0 sh "$REPO_ROOT/scripts/preflight-toolchain.sh" 2>&1)
assert_eq "matching toolchain is silent" "" "$toolchain_output"

UNAVAILABLE_REPO="$TMP_ROOT/unavailable-repo"
mkdir -p "$UNAVAILABLE_REPO/scripts"
cp "$REPO_ROOT/scripts/preflight-toolchain.sh" "$UNAVAILABLE_REPO/scripts/preflight-toolchain.sh"
: > "$UNAVAILABLE_REPO/rust-toolchain.toml"
if toolchain_output=$(RUSTC="$TMP_ROOT/missing-rustc" sh "$UNAVAILABLE_REPO/scripts/preflight-toolchain.sh" 2>&1); then
  fail "unavailable expected and actual toolchains must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq \
  "unavailable toolchain error" \
  "ERROR: Rust toolchain mismatch: expected unavailable, actual unavailable. Run 'make rust-toolchain'." \
  "$toolchain_output"

FAKE_CARGO="$TMP_ROOT/fake-cargo"
cat > "$FAKE_CARGO" <<'EOF'
#!/usr/bin/env sh
if [ "${FAKE_DENY_MODE:-}" = "missing" ]; then
  exit 127
fi
echo "cargo-deny ${FAKE_DENY_VERSION}"
EOF
chmod +x "$FAKE_CARGO"

if deny_output=$(CARGO="$FAKE_CARGO" FAKE_DENY_MODE=missing sh "$REPO_ROOT/scripts/preflight-cargo-deny.sh" 2>&1); then
  fail "missing cargo-deny must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "missing cargo-deny version" "$deny_output" "actual unavailable"
assert_contains \
  "missing cargo-deny repair" \
  "$deny_output" \
  "cargo install cargo-deny --version 0.20.2 --locked"

if deny_output=$(CARGO="$FAKE_CARGO" FAKE_DENY_VERSION=9.9.9 sh "$REPO_ROOT/scripts/preflight-cargo-deny.sh" 2>&1); then
  fail "skewed cargo-deny must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "skewed cargo-deny version" "$deny_output" "expected 0.20.2, actual 9.9.9"
assert_contains \
  "skewed cargo-deny repair" \
  "$deny_output" \
  "cargo install cargo-deny --version 0.20.2 --locked"

deny_output=$(CARGO="$FAKE_CARGO" FAKE_DENY_VERSION=0.20.2 sh "$REPO_ROOT/scripts/preflight-cargo-deny.sh" 2>&1)
assert_eq "matching cargo-deny is silent" "" "$deny_output"

GIT_REPO="$TMP_ROOT/git-repo"
mkdir "$GIT_REPO"
git -C "$GIT_REPO" init -q
git -C "$GIT_REPO" config user.name "solstone gate test"
git -C "$GIT_REPO" config user.email "gate-test@example.invalid"
printf '%s\n' "baseline" > "$GIT_REPO/tracked.txt"
git -C "$GIT_REPO" add tracked.txt
git -C "$GIT_REPO" commit -qm "baseline"

printf '%s\n' "probe" > "$GIT_REPO/scratch_untracked_probe.txt"
if guard_output=$(cd "$GIT_REPO" && sh "$REPO_ROOT/scripts/check-win-sync-tree.sh" 2>&1); then
  fail "untracked file must refuse Windows sync"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "untracked refusal names file" "$guard_output" "scratch_untracked_probe.txt"
assert_contains "untracked refusal explains omission" "$guard_output" "would be omitted"

git -C "$GIT_REPO" add scratch_untracked_probe.txt
guard_output=$(cd "$GIT_REPO" && sh "$REPO_ROOT/scripts/check-win-sync-tree.sh" 2>&1)
assert_eq "staged addition passes guard" "" "$guard_output"
git -C "$GIT_REPO" commit -qm "add probe"

printf '%s\n' "modified" >> "$GIT_REPO/tracked.txt"
guard_output=$(cd "$GIT_REPO" && sh "$REPO_ROOT/scripts/check-win-sync-tree.sh" 2>&1)
assert_eq "tracked modification passes guard" "" "$guard_output"

echo "deterministic-gates.test.sh: $ASSERTIONS assertions passed"
