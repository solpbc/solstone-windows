#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ASSERTIONS=0
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/make-prove-native-ordering.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  echo "make-prove-native-ordering.test.sh: assertion failed: $1" >&2
  echo "make-prove-native-ordering.test.sh: failure after $ASSERTIONS assertions" >&2
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

assert_true() {
  if ! eval "$2"; then
    fail "$1"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

WITNESS="$TMP_ROOT/witness"
TARGET_DIR="$TMP_ROOT/cargo-target"
MISSING_TARGET_DIR="$TMP_ROOT/missing-cargo-target"
FAKE_CARGO="$TMP_ROOT/fake-cargo"
FAKE_XTASK="$TMP_ROOT/fake-xtask"
FAKE_PWSH="$TMP_ROOT/proof-powershell.exe"
RELEASE_DIR="target/release-candidate/0.2.11"
MISSING_LOG="$TMP_ROOT/missing-release-dir.log"
MISSING_BINARY_LOG="$TMP_ROOT/missing-binary.log"
RECIPE_BODY="$TMP_ROOT/recipe-body"

cat > "$FAKE_XTASK" <<'EOF'
#!/usr/bin/env sh
set -eu
printf 'xtask|args=%s|offline=%s|powershell=%s|version-cargo=%s\n' \
  "$*" "${CARGO_NET_OFFLINE:-}" "${SOLSTONE_PROOF_POWERSHELL:-}" "${SOLSTONE_VERSION_GATE_CARGO:-}" >> "$PROVE_WITNESS"
EOF
chmod +x "$FAKE_XTASK"

cat > "$FAKE_CARGO" <<'EOF'
#!/usr/bin/env sh
set -eu
printf 'cargo|args=%s\n' "$*" >> "$PROVE_WITNESS"
if [ "${FAKE_CARGO_CREATE_XTASK:-1}" = "1" ]; then
  mkdir -p "$CARGO_TARGET_DIR/debug"
  cp "$FAKE_XTASK_TEMPLATE" "$CARGO_TARGET_DIR/debug/xtask"
  chmod +x "$CARGO_TARGET_DIR/debug/xtask"
fi
EOF
chmod +x "$FAKE_CARGO"

export PROVE_WITNESS="$WITNESS"
export FAKE_XTASK_TEMPLATE="$FAKE_XTASK"

: > "$WITNESS"
missing_status=0
if CARGO_TARGET_DIR="$TARGET_DIR" make -s -C "$REPO_ROOT" prove-rust-release-native \
    RELEASE_DIR= CARGO="$FAKE_CARGO" PWSH="$FAKE_PWSH" >"$MISSING_LOG" 2>&1; then
  missing_status=0
else
  missing_status=$?
fi
assert_true "missing RELEASE_DIR fails first with exact diagnostic and invokes nothing" \
  "[ '$missing_status' -ne 0 ] && grep -Fqx 'ERROR: RELEASE_DIR is required; pass target/release-candidate/<VERSION> and retry.' '$MISSING_LOG' && [ ! -s '$WITNESS' ]"

: > "$WITNESS"
CARGO_TARGET_DIR="$TARGET_DIR" make -s -C "$REPO_ROOT" prove-rust-release-native \
  RELEASE_DIR="$RELEASE_DIR" CARGO="$FAKE_CARGO" PWSH="$FAKE_PWSH" >/dev/null
assert_eq "fake build tool is invoked exactly once with exact argv" \
  "cargo|args=build --locked -q -p xtask" "$(grep '^cargo|' "$WITNESS")"
assert_eq "build tool is never invoked with run" "0" "$(grep -c '^cargo|args=run' "$WITNESS" || true)"
assert_true "custom target directory contains the executable xtask" "[ -x '$TARGET_DIR/debug/xtask' ]"
assert_true "fake xtask receives exact proof argv" \
  "grep -Fq 'xtask|args=rust-release-manifest prove-native --release-dir $RELEASE_DIR|' '$WITNESS'"
assert_true "fake xtask receives offline mode" "grep -Fq '|offline=true|' '$WITNESS'"
assert_true "fake xtask receives selected proof PowerShell" "grep -Fq '|powershell=$FAKE_PWSH|' '$WITNESS'"
assert_true "fake xtask receives selected version-gate cargo" "grep -Fq '|version-cargo=$FAKE_CARGO' '$WITNESS'"
assert_eq "build precedes direct xtask invocation" "cargo
xtask" "$(cut -d'|' -f1 "$WITNESS")"

awk '
  /^prove-rust-release-native:/ { in_target = 1 }
  in_target && !/^prove-rust-release-native:/ && /^[^[:space:]#][^:]*:/ { exit }
  in_target { print }
' "$REPO_ROOT/Makefile" > "$RECIPE_BODY"
assert_true "recipe source contains the locked xtask build" "grep -Fq 'build --locked -q -p xtask' '$RECIPE_BODY'"
assert_true "recipe source contains no cargo run xtask invocation" "! grep -Fq 'run --locked -q -p xtask' '$RECIPE_BODY'"

: > "$WITNESS"
missing_binary_status=0
if FAKE_CARGO_CREATE_XTASK=0 CARGO_TARGET_DIR="$MISSING_TARGET_DIR" \
    make -s -C "$REPO_ROOT" prove-rust-release-native RELEASE_DIR="$RELEASE_DIR" \
    CARGO="$FAKE_CARGO" PWSH="$FAKE_PWSH" >"$MISSING_BINARY_LOG" 2>&1; then
  missing_binary_status=0
else
  missing_binary_status=$?
fi
assert_true "missing built xtask fails with exact diagnostic" \
  "[ '$missing_binary_status' -ne 0 ] && grep -Fqx 'ERROR: built xtask executable not found at $MISSING_TARGET_DIR/debug/xtask or $MISSING_TARGET_DIR/debug/xtask.exe; restore CARGO_TARGET_DIR consistency and retry.' '$MISSING_BINARY_LOG'"

echo "make-prove-native-ordering.test.sh: $ASSERTIONS assertions passed"
