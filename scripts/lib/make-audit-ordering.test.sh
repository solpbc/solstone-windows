#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ASSERTIONS=0
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/make-audit-ordering.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  echo "make-audit-ordering.test.sh: assertion failed: $1" >&2
  echo "make-audit-ordering.test.sh: failure after $ASSERTIONS assertions" >&2
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

WITNESS="$TMP_ROOT/witness"
STDOUT="$TMP_ROOT/stdout"
FAKE_RUSTC="$TMP_ROOT/rustc"
FAKE_CARGO="$TMP_ROOT/cargo"

cat >"$FAKE_RUSTC" <<'EOF'
#!/usr/bin/env sh
set -eu
printf '%s\n' toolchain >>"$AUDIT_WITNESS"
if [ "${1:-}" = -Vv ]; then
  printf 'rustc synthetic\nrelease: %s\n' "$AUDIT_RUST_VERSION"
  exit 0
fi
exit 1
EOF

cat >"$FAKE_CARGO" <<'EOF'
#!/usr/bin/env sh
set -eu
if [ "${1:-}" = deny ] && [ "${2:-}" = --version ]; then
  printf '%s\n' deny-preflight >>"$AUDIT_WITNESS"
  printf 'cargo-deny 0.20.2\n'
  exit 0
fi
printf '%s\n' delegate >>"$AUDIT_WITNESS"
printf '{"schema":"synthetic.make-audit-test.v1"}\n'
EOF
chmod +x "$FAKE_RUSTC" "$FAKE_CARGO"

AUDIT_RUST_VERSION=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "$REPO_ROOT/rust-toolchain.toml" | sed -n '1p')
export AUDIT_RUST_VERSION AUDIT_WITNESS="$WITNESS"

LOCATOR=https://synthetic-user@mirror.example.invalid/advisory-db
RECEIPT="$TMP_ROOT/freshness.json"
PUBLIC_KEY="$TMP_ROOT/synthetic.pub"
BUNDLE="$TMP_ROOT/synthetic.bundle"
export SOLSTONE_ADVISORY_MIRROR_LOCATOR="$LOCATOR"
export SOLSTONE_ADVISORY_RECEIPT="$RECEIPT"
export SOLSTONE_ADVISORY_MIRROR_PUB="$PUBLIC_KEY"
export SOLSTONE_ADVISORY_BUNDLE="$BUNDLE"

for missing in SOLSTONE_ADVISORY_MIRROR_LOCATOR SOLSTONE_ADVISORY_RECEIPT SOLSTONE_ADVISORY_MIRROR_PUB SOLSTONE_ADVISORY_BUNDLE; do
  : >"$WITNESS"
  : >"$STDOUT"
  if env -u "$missing" make --no-print-directory -C "$REPO_ROOT" audit \
      RUSTC="$FAKE_RUSTC" CARGO="$FAKE_CARGO" >"$STDOUT" 2>/dev/null; then
    fail "missing $missing must fail"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
  assert_eq "missing input runs no tool" "" "$(cat "$WITNESS")"
  assert_eq "missing input emits no stdout" "" "$(cat "$STDOUT")"
done

for empty in SOLSTONE_ADVISORY_MIRROR_LOCATOR SOLSTONE_ADVISORY_RECEIPT SOLSTONE_ADVISORY_MIRROR_PUB SOLSTONE_ADVISORY_BUNDLE; do
  : >"$WITNESS"
  : >"$STDOUT"
  if env "$empty=" make --no-print-directory -C "$REPO_ROOT" audit \
      RUSTC="$FAKE_RUSTC" CARGO="$FAKE_CARGO" >"$STDOUT" 2>/dev/null; then
    fail "empty $empty must fail"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
  assert_eq "empty input runs no tool" "" "$(cat "$WITNESS")"
  assert_eq "empty input emits no stdout" "" "$(cat "$STDOUT")"
done

: >"$WITNESS"
make --no-print-directory -C "$REPO_ROOT" audit RUSTC="$FAKE_RUSTC" CARGO="$FAKE_CARGO" >"$STDOUT"
EXPECTED_ORDER=$(printf 'toolchain\ndeny-preflight\ndelegate')
assert_eq "audit tool order" "$EXPECTED_ORDER" "$(cat "$WITNESS")"
assert_eq "audit stdout is only the delegated witness" \
  '{"schema":"synthetic.make-audit-test.v1"}' "$(cat "$STDOUT")"

AUDIT_RECIPE=$(sed -n '/^audit:/,/^$/p' "$REPO_ROOT/Makefile")
assert_eq "audit has no prerequisites" "audit:" "$(printf '%s\n' "$AUDIT_RECIPE" | sed -n '1p')"
if printf '%s\n' "$AUDIT_RECIPE" | grep -q 'deny fetch'; then
  fail "audit recipe must not retain cargo-deny fetch"
fi
ASSERTIONS=$((ASSERTIONS + 1))
if printf '%s\n' "$AUDIT_RECIPE" | sed -n '/^[[:space:]]*[^#[:space:]]/p' | grep -v '^audit:$' | grep -qv '^[[:space:]]*@'; then
  fail "every audit recipe line must suppress make echo"
fi
ASSERTIONS=$((ASSERTIONS + 1))

echo "make-audit-ordering.test.sh: $ASSERTIONS assertions passed"
