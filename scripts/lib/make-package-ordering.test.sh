#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ASSERTIONS=0
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/make-package-ordering.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  echo "make-package-ordering.test.sh: assertion failed: $1" >&2
  echo "make-package-ordering.test.sh: failure after $ASSERTIONS assertions" >&2
  exit 1
}

assert_eq() {
  if [ "$2" != "$3" ]; then
    echo "  expected: $2" >&2
    echo "  actual:   $3" >&2
    fail "$1"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

FAKE_BIN="$TMP_ROOT/selected"
POISON_BIN="$TMP_ROOT/poison"
WITNESS="$TMP_ROOT/witness"
POISON_WITNESS="$TMP_ROOT/poison-witness"
mkdir "$FAKE_BIN" "$POISON_BIN"

FAKE_CARGO="$FAKE_BIN/cargo"
FAKE_NPM="$FAKE_BIN/npm"
FAKE_PWSH="$FAKE_BIN/powershell"

cat > "$FAKE_CARGO" <<'EOF'
#!/usr/bin/env sh
set -eu
case "${1:-}" in
  run)
    echo version-gate >> "$PACKAGE_WITNESS"
    [ "${PACKAGE_FAIL_AT:-}" != version ] || exit 31
    echo 0.2.11
    ;;
  build)
    echo cargo-build >> "$PACKAGE_WITNESS"
    ;;
  *) exit 90 ;;
esac
EOF

cat > "$FAKE_NPM" <<'EOF'
#!/usr/bin/env sh
set -eu
case "$*" in
  *" ci --offline") echo npm-ci >> "$PACKAGE_WITNESS" ;;
  *" run build") echo npm-build >> "$PACKAGE_WITNESS" ;;
  *) exit 90 ;;
esac
EOF

cat > "$FAKE_PWSH" <<'EOF'
#!/usr/bin/env sh
set -eu
case "$*" in
  *"packaging/preflight-release-tools.ps1"*)
    echo preflight >> "$PACKAGE_WITNESS"
    [ "${PACKAGE_FAIL_AT:-}" != preflight ] || exit 30
    printf '{"schema":"solstone.release-tool-selection.v1","mode":"unsigned","tools":{"cargo":{"path":"%s"},"npm":{"path":"%s"},"powershell":{"path":"%s"}}}\n' "$FAKE_SELECTED_CARGO" "$FAKE_SELECTED_NPM" "$FAKE_SELECTED_POWERSHELL"
    ;;
  *"tools.cargo.path"*) printf '%s\n' "$FAKE_SELECTED_CARGO" ;;
  *"tools.npm.path"*) printf '%s\n' "$FAKE_SELECTED_NPM" ;;
  *"tools.powershell.path"*) printf '%s\n' "$FAKE_SELECTED_POWERSHELL" ;;
  *"packaging/lock-guard.ps1"*)
    echo lock-guard >> "$PACKAGE_WITNESS"
    [ "${PACKAGE_FAIL_AT:-}" != lock ] || exit 32
    ;;
  *"scripts/package.ps1"*) echo direct-package >> "$PACKAGE_WITNESS" ;;
  *) exit 90 ;;
esac
EOF
chmod +x "$FAKE_CARGO" "$FAKE_NPM" "$FAKE_PWSH"

for tool in cargo npm pwsh powershell; do
  cat > "$POISON_BIN/$tool" <<'EOF'
#!/usr/bin/env sh
echo poison >> "$POISON_WITNESS"
exit 97
EOF
  chmod +x "$POISON_BIN/$tool"
done

export PACKAGE_WITNESS="$WITNESS"
export POISON_WITNESS
export FAKE_SELECTED_CARGO="$FAKE_CARGO"
export FAKE_SELECTED_NPM="$FAKE_NPM"
export FAKE_SELECTED_POWERSHELL="$FAKE_PWSH"

hash_locks() {
  sha256sum "$REPO_ROOT/Cargo.lock" "$REPO_ROOT/ui/package-lock.json"
}

run_case() {
  PACKAGE_FAIL_AT=$1
  export PACKAGE_FAIL_AT
  : > "$WITNESS"
  : > "$POISON_WITNESS"
  before=$(hash_locks)
  if PATH="$POISON_BIN:$PATH" MAKEFLAGS= make -s -C "$REPO_ROOT" package PWSH="$FAKE_PWSH" CARGO="$POISON_BIN/cargo" >/dev/null 2>&1; then
    CASE_STATUS=0
  else
    CASE_STATUS=$?
  fi
  after=$(hash_locks)
  assert_eq "$PACKAGE_FAIL_AT lock bytes stable" "$before" "$after"
  assert_eq "$PACKAGE_FAIL_AT poisoned PATH unused" "" "$(cat "$POISON_WITNESS")"
}

run_case none
assert_eq "success status" "0" "$CASE_STATUS"
assert_eq "success order" "preflight
version-gate
lock-guard
npm-ci
npm-build
cargo-build
direct-package" "$(cat "$WITNESS")"

run_case preflight
[ "$CASE_STATUS" -ne 0 ] || fail "preflight failure must fail"
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq "preflight failure stops later work" "preflight" "$(cat "$WITNESS")"

run_case version
[ "$CASE_STATUS" -ne 0 ] || fail "version failure must fail"
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq "version failure stops lock and work" "preflight
version-gate" "$(cat "$WITNESS")"

run_case lock
[ "$CASE_STATUS" -ne 0 ] || fail "lock failure must fail"
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq "lock failure stops dependency/build/package work" "preflight
version-gate
lock-guard" "$(cat "$WITNESS")"

echo "make-package-ordering.test.sh: $ASSERTIONS assertions passed"
