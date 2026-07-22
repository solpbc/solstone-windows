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

assert_true() {
  if ! eval "$2"; then
    fail "$1"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

WITNESS="$TMP_ROOT/witness"
FAKE_PWSH="$TMP_ROOT/fake-powershell"
cat > "$FAKE_PWSH" <<'EOF'
#!/usr/bin/env sh
set -eu
printf 'delegate|commit=%s|advisory=%s|git=%s|args=%s\n' \
  "${EXPECTED_RELEASE_COMMIT:-}" "${SOLSTONE_ADVISORY_TREE_SHA256:-}" "${GIT:-}" "$*" >> "$PACKAGE_WITNESS"
EOF
chmod +x "$FAKE_PWSH"
export PACKAGE_WITNESS="$WITNESS"

EXPECTED=0123456789abcdef0123456789abcdef01234567
ADVISORY=$(printf 'a%.0s' $(seq 1 64))

: > "$WITNESS"
if env -u EXPECTED_RELEASE_COMMIT -u SOLSTONE_ADVISORY_TREE_SHA256 -u SOLSTONE_SIGN \
    make -s -C "$REPO_ROOT" package PWSH="$FAKE_PWSH" >/dev/null 2>&1; then
  fail "missing EXPECTED_RELEASE_COMMIT must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq "missing commit invokes nothing" "" "$(cat "$WITNESS")"

: > "$WITNESS"
EXPECTED_RELEASE_COMMIT="$EXPECTED" \
  SOLSTONE_ADVISORY_TREE_SHA256="$ADVISORY" \
  make -s -C "$REPO_ROOT" package PWSH="$FAKE_PWSH" >/dev/null
assert_eq "unsigned make delegates once" "1" "$(wc -l < "$WITNESS" | tr -d ' ')"
assert_eq "unsigned delegation" \
  "delegate|commit=$EXPECTED|advisory=$ADVISORY|git=git|args=-NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1" \
  "$(cat "$WITNESS")"

: > "$WITNESS"
EXPECTED_RELEASE_COMMIT="$EXPECTED" SOLSTONE_ADVISORY_TREE_SHA256="$ADVISORY" SOLSTONE_SIGN=1 \
  make -s -C "$REPO_ROOT" package PWSH="$FAKE_PWSH" >/dev/null
assert_eq "signed make delegates once" "1" "$(wc -l < "$WITNESS" | tr -d ' ')"
assert_eq "signed delegation translates flag" \
  "delegate|commit=$EXPECTED|advisory=$ADVISORY|git=git|args=-NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Sign" \
  "$(cat "$WITNESS")"

: > "$WITNESS"
if EXPECTED_RELEASE_COMMIT="$EXPECTED" \
    make -s -C "$REPO_ROOT" package PWSH="$FAKE_PWSH" >/dev/null 2>&1; then
  fail "missing SOLSTONE_ADVISORY_TREE_SHA256 must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq "missing advisory digest invokes nothing" "" "$(cat "$WITNESS")"

for invalid_sign in 0 false ' '; do
  : > "$WITNESS"
  if EXPECTED_RELEASE_COMMIT="$EXPECTED" SOLSTONE_ADVISORY_TREE_SHA256="$ADVISORY" \
      SOLSTONE_SIGN="$invalid_sign" \
      make -s -C "$REPO_ROOT" package PWSH="$FAKE_PWSH" >/dev/null 2>&1; then
    fail "invalid SOLSTONE_SIGN value must fail"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
  assert_eq "invalid SOLSTONE_SIGN invokes nothing" "" "$(cat "$WITNESS")"
done

PACKAGE_SOURCE="$REPO_ROOT/scripts/package.ps1"
preflight_line=$(grep -n '\$SelectionLines = @(' "$PACKAGE_SOURCE" | cut -d: -f1)
version_line=$(grep -n '\$VersionOutput = @(' "$PACKAGE_SOURCE" | cut -d: -f1)
lock_line=$(grep -n 'packaging\\lock-guard.ps1' "$PACKAGE_SOURCE" | cut -d: -f1)
commit_validation_line=$(grep -n 'EXPECTED_RELEASE_COMMIT is required' "$PACKAGE_SOURCE" | cut -d: -f1)
advisory_validation_line=$(grep -n 'SOLSTONE_ADVISORY_TREE_SHA256 is required' "$PACKAGE_SOURCE" | cut -d: -f1)
npm_cache_line=$(grep -n 'packaging\\npm-cache-preflight.ps1' "$PACKAGE_SOURCE" | cut -d: -f1)
finalize_line=$(grep -n '\$SelectionJson | & \$CargoPath @FinalizeArgs' "$PACKAGE_SOURCE" | cut -d: -f1)
assert_true "package.ps1 keeps preflight-version-lock-npm-cache-finalize order" \
  "[ '$preflight_line' -lt '$version_line' ] && [ '$version_line' -lt '$lock_line' ] && [ '$lock_line' -lt '$npm_cache_line' ] && [ '$npm_cache_line' -lt '$finalize_line' ]"
assert_true "package.ps1 validates commit and advisory digest before npm cache probe" \
  "[ '$lock_line' -lt '$commit_validation_line' ] && [ '$commit_validation_line' -lt '$advisory_validation_line' ] && [ '$advisory_validation_line' -lt '$npm_cache_line' ]"
assert_eq "package.ps1 has one finalizer invocation" "1" \
  "$(grep -c '\$SelectionJson | & \$CargoPath @FinalizeArgs' "$PACKAGE_SOURCE")"
assert_true "package.ps1 never attests a pre-existing app exe" \
  "! grep -q 'solstone-windows-app.exe' '$PACKAGE_SOURCE'"
assert_true "package.ps1 retains no legacy Velopack invocation" \
  "! grep -qi 'vpk pack' '$PACKAGE_SOURCE'"

echo "make-package-ordering.test.sh: $ASSERTIONS assertions passed (PowerShell internals are source-witnessed here; executable .ps1/.cmd coverage is box-only without pwsh)"
