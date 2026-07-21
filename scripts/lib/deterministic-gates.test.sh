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

assert_not_contains() {
  label=$1
  haystack=$2
  needle=$3
  case "$haystack" in
    *"$needle"*) fail "$label (unexpected: $needle)" ;;
    *) ;;
  esac
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_file_exists() {
  label=$1
  path=$2
  if [ ! -f "$path" ]; then
    fail "$label (missing: $path)"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_file_absent() {
  label=$1
  path=$2
  if [ -e "$path" ]; then
    fail "$label (unexpected: $path)"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_line_order() {
  label=$1
  path=$2
  shift 2
  prior=0
  for pattern in "$@"; do
    line=$(grep -n -F "$pattern" "$path" | sed -n '1s/:.*//p')
    if [ -z "$line" ] || [ "$line" -le "$prior" ]; then
      fail "$label (out of order or missing: $pattern)"
    fi
    prior=$line
  done
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_file_count() {
  label=$1
  expected=$2
  directory=$3
  pattern=$4
  actual=$(find "$directory" -type f -name "$pattern" | wc -l | tr -d ' ')
  assert_eq "$label" "$expected" "$actual"
}

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/deterministic-gates.XXXXXX")

FAKE_RUSTC="$TMP_ROOT/fake-rustc"
cat > "$FAKE_RUSTC" <<'EOF'
#!/usr/bin/env sh
echo "rustc ${FAKE_RELEASE} (probe 1970-01-01)"
echo "binary: rustc"
echo "host: x86_64-unknown-linux-gnu"
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

SPACED_RUSTC_DIR="$TMP_ROOT/compiler path with spaces"
SPACED_RUSTC="$SPACED_RUSTC_DIR/fake rustc"
mkdir -p "$SPACED_RUSTC_DIR"
cp "$FAKE_RUSTC" "$SPACED_RUSTC"
chmod +x "$SPACED_RUSTC"
toolchain_output=$(RUSTC="$SPACED_RUSTC" FAKE_RELEASE=1.96.0 sh "$REPO_ROOT/scripts/preflight-toolchain.sh" 2>&1)
assert_eq "matching toolchain path with spaces is silent" "" "$toolchain_output"
if toolchain_output=$(RUSTC="$SPACED_RUSTC" FAKE_RELEASE=9.9.9 sh "$REPO_ROOT/scripts/preflight-toolchain.sh" 2>&1); then
  fail "skewed toolchain path with spaces must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_eq \
  "skewed toolchain path with spaces error" \
  "ERROR: Rust toolchain mismatch: expected 1.96.0, actual 9.9.9. Run 'make rust-toolchain'." \
  "$toolchain_output"

NATIVE_PREFLIGHT=$(cat "$REPO_ROOT/scripts/preflight-toolchain.cmd")
assert_contains \
  "native preflight names MSVC host" \
  "$NATIVE_PREFLIGHT" \
  "x86_64-pc-windows-msvc"
assert_contains \
  "native preflight parses host field" \
  "$NATIVE_PREFLIGHT" \
  '/c:"host:"'
assert_contains \
  "native preflight honors explicit RUSTC" \
  "$NATIVE_PREFLIGHT" \
  '%RUSTC%'
for required_substring in \
  "Rust toolchain mismatch" \
  "expected" \
  "actual" \
  "make rust-toolchain"
do
  assert_contains \
    "native preflight retains $required_substring diagnostic" \
    "$NATIVE_PREFLIGHT" \
    "$required_substring"
done

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
  "Run 'make provision-cargo-deny'."

if deny_output=$(CARGO="$FAKE_CARGO" FAKE_DENY_VERSION=9.9.9 sh "$REPO_ROOT/scripts/preflight-cargo-deny.sh" 2>&1); then
  fail "skewed cargo-deny must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "skewed cargo-deny version" "$deny_output" "expected 0.20.2, actual 9.9.9"
assert_contains \
  "skewed cargo-deny repair" \
  "$deny_output" \
  "Run 'make provision-cargo-deny'."

deny_output=$(CARGO="$FAKE_CARGO" FAKE_DENY_VERSION=0.20.2 sh "$REPO_ROOT/scripts/preflight-cargo-deny.sh" 2>&1)
assert_eq "matching cargo-deny is silent" "" "$deny_output"

DENY_PREFLIGHT=$(cat "$REPO_ROOT/scripts/preflight-cargo-deny.sh")
assert_contains \
  "cargo-deny preflight names isolated advisory root" \
  "$DENY_PREFLIGHT" \
  "isolated_db_relative=target/release-advisory-db"

provision_dry_run=$(MAKEFLAGS= make -C "$REPO_ROOT" -n provision-cargo-deny 2>&1)
assert_contains \
  "named cargo-deny provisioning verb" \
  "$provision_dry_run" \
  "cargo install cargo-deny --version 0.20.2 --locked"
advisory_config_dry_run=$(MAKEFLAGS= make -C "$REPO_ROOT" -n check-release-advisory-config 2>&1)
assert_contains \
  "advisory config check maps isolated database" \
  "$advisory_config_dry_run" \
  "target/release-advisory-db"
assert_contains \
  "advisory config check uses xtask materializer" \
  "$advisory_config_dry_run" \
  "rust-release-manifest advisory-config"
assert_contains \
  "advisory config check invokes real pin offline" \
  "$advisory_config_dry_run" \
  'deny --locked --offline --config'
assert_contains \
  "advisory config check missing-cache remediation" \
  "$advisory_config_dry_run" \
  "run 'make audit' or refresh the RustSec cache, then retry"
assert_contains \
  "advisory config check removes its transient lock" \
  "$advisory_config_dry_run" \
  'db_lock="$isolated/db.lock"'
ui_update_dry_run=$(MAKEFLAGS= make -C "$REPO_ROOT" -n ui-deps-update 2>&1)
assert_contains "named UI dependency update verb" "$ui_update_dry_run" "npm --prefix ui install"
for target in build test ui-test package ci audit; do
  dry_run=$(MAKEFLAGS= make -C "$REPO_ROOT" -n "$target" WIN_REMOTE_HOST=fake@example.invalid 2>&1 || true)
  assert_not_contains \
    "$target never provisions cargo-deny" \
    "$dry_run" \
    "cargo install cargo-deny"
done

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

default_pull=$(MAKEFLAGS= make -C "$REPO_ROOT" -n pull-releases WIN_REMOTE_HOST=fake@host 2>&1)
assert_contains "pull-releases default carries scp control options" "$default_pull" \
  "scp -o ControlMaster=auto -o ControlPath=/tmp/sw-%r@%h:%p -o ControlPersist=60s -r fake@host:swbuild/Releases Releases"
custom_pull=$(MAKEFLAGS= make -C "$REPO_ROOT" -n pull-releases WIN_REMOTE_HOST=fake@host WIN_SCP=/custom/scp 2>&1)
assert_contains "pull-releases honors WIN_SCP override" "$custom_pull" \
  "/custom/scp -r fake@host:swbuild/Releases Releases"
assert_not_contains "custom WIN_SCP replaces the default scp binary" "$custom_pull" "ControlMaster"

WIN_CI_SOURCE=$(cat "$REPO_ROOT/scripts/win-ci.cmd")
assert_contains "box gate requires expected commit" "$WIN_CI_SOURCE" "if not defined EXPECTED_RELEASE_COMMIT"
assert_contains "box gate emits Cargo.lock digest" "$WIN_CI_SOURCE" "echo WIN_CI_CARGO_LOCK_SHA256="
assert_contains "box gate emits UI lock digest" "$WIN_CI_SOURCE" "echo WIN_CI_UI_LOCK_SHA256="
assert_line_order \
  "box source binding precedes byte-changing work" \
  "$REPO_ROOT/scripts/win-ci.cmd" \
  "git rev-parse HEAD" \
  "git status --porcelain=v1 --untracked-files=all --ignore-submodules=none" \
  "Get-FileHash -LiteralPath 'Cargo.lock'" \
  "Get-FileHash -LiteralPath 'ui/package-lock.json'" \
  "echo === cargo build --locked"

FAKE_GIT="$TMP_ROOT/fake-git"
cat > "$FAKE_GIT" <<'EOF'
#!/usr/bin/env sh
set -eu

{
  if [ -n "${FAKE_RUN_ID:-}" ]; then
    printf '%s-' "$FAKE_RUN_ID"
  fi
  printf 'git'
  for arg in "$@"; do
    printf '|%s' "$arg"
  done
  printf '\n'
} >> "$FAKE_WITNESS"

fail_for() {
  if [ "${FAKE_GIT_FAIL_PHASE:-}" = "$1" ]; then
    exit "${FAKE_GIT_FAIL_STATUS:-23}"
  fi
}

command_name=${1:-}
shift || :
case "$command_name" in
  ls-files)
    case "${1:-}" in
      --unmerged)
        fail_for guard
        if [ -n "${FAKE_GIT_UNMERGED:-}" ]; then
          printf '%s\n' "$FAKE_GIT_UNMERGED"
        fi
        ;;
      --others)
        fail_for guard
        if [ -n "${FAKE_GIT_UNTRACKED:-}" ]; then
          printf '%s\n' "$FAKE_GIT_UNTRACKED"
        fi
        ;;
      *) exit 90 ;;
    esac
    ;;
  stash)
    [ "${1:-}" = "create" ] || exit 90
    fail_for resolve-sha
    if [ "${FAKE_GIT_STASH_EMPTY:-0}" != "1" ]; then
      printf '%s\n' "$FAKE_GIT_SHA"
    fi
    ;;
  status)
    fail_for release-status
    if [ "${FAKE_GIT_RELEASE_DIRTY:-0}" = "1" ]; then
      printf ' M Cargo.lock\0'
    fi
    ;;
  show)
    fail_for resolve-binding
    case "${1:-}" in
      "$FAKE_GIT_SHA:Cargo.lock") printf '%s\n' "$FAKE_CARGO_LOCK_CONTENT" ;;
      "$FAKE_GIT_SHA:ui/package-lock.json") printf '%s\n' "$FAKE_UI_LOCK_CONTENT" ;;
      *) exit 90 ;;
    esac
    ;;
  rev-parse)
    case "${1:-}" in
      HEAD)
        fail_for resolve-fallback
        printf '%s\n' "$FAKE_GIT_SHA"
        ;;
      --git-common-dir)
        printf '%s\n' "$FAKE_GIT_COMMON_DIR"
        ;;
      *) exit 90 ;;
    esac
    ;;
  update-ref)
    if [ "${1:-}" = "-d" ]; then
      fail_for delete-temp-ref
      if [ "${FAKE_GIT_FAIL_CLEANUP:-0}" = "1" ]; then
        exit "${FAKE_GIT_CLEANUP_STATUS:-67}"
      fi
      expected=${3:-}
      if [ ! -f "$FAKE_GIT_STATE_DIR/swsync" ] ||
        [ "$(cat "$FAKE_GIT_STATE_DIR/swsync")" != "$expected" ]; then
        exit 68
      fi
      rm -f "$FAKE_GIT_STATE_DIR/swsync"
    else
      fail_for create-temp-ref
      [ "${1:-}" = "refs/heads/__swsync" ] || exit 90
      [ "${3+x}" = x ] || exit 90
      [ -z "$3" ] || exit 90
      if [ -f "$FAKE_GIT_STATE_DIR/swsync" ]; then
        exit 69
      fi
      printf '%s\n' "$2" > "$FAKE_GIT_STATE_DIR/swsync"
    fi
    ;;
  bundle)
    subcommand=${1:-}
    shift || :
    case "$subcommand" in
      create)
        fail_for create-bundle
        printf 'fake bundle for %s\n' "$FAKE_GIT_SHA" > "$1"
        ;;
      verify)
        fail_for verify-bundle
        [ -f "$1" ] || exit 70
        ;;
      list-heads)
        fail_for verify-heads
        if [ "${FAKE_GIT_HEAD_MISMATCH:-0}" = "1" ]; then
          printf '%s %s\n' "$FAKE_OTHER_SHA" refs/heads/not-swsync
        else
          printf '%s %s\n' "$FAKE_GIT_SHA" refs/heads/__swsync
        fi
        ;;
      *) exit 90 ;;
    esac
    ;;
  *) exit 90 ;;
esac
EOF
chmod +x "$FAKE_GIT"

FAKE_SCP="$TMP_ROOT/fake-scp"
cat > "$FAKE_SCP" <<'EOF'
#!/usr/bin/env sh
set -eu

{
  if [ -n "${FAKE_RUN_ID:-}" ]; then
    printf '%s-' "$FAKE_RUN_ID"
  fi
  printf 'scp'
  for arg in "$@"; do
    printf '|%s' "$arg"
  done
  printf '\n'
} >> "$FAKE_WITNESS"

source_path=
destination=
for arg in "$@"; do
  source_path=$destination
  destination=$arg
done
if [ "${FAKE_SCP_FAIL:-0}" = "1" ]; then
  exit "${FAKE_SCP_STATUS:-41}"
fi
case "$destination" in
  *:swbuild.bundle)
    if [ -n "${FAKE_SCP_SOURCE_FILE:-}" ]; then
      printf '%s\n' "$source_path" > "$FAKE_SCP_SOURCE_FILE"
    fi
    if [ -n "${FAKE_SCP_COPY_TO:-}" ]; then
      cp "$source_path" "$FAKE_SCP_COPY_TO"
    fi
    ;;
  *:win-host-ci-source-binding.json)
    if [ "${FAKE_SCP_BINDING_FAIL:-0}" = "1" ]; then
      exit "${FAKE_SCP_STATUS:-42}"
    fi
    ;;
esac
EOF
chmod +x "$FAKE_SCP"

FAKE_SSH="$TMP_ROOT/fake-ssh"
cat > "$FAKE_SSH" <<'EOF'
#!/usr/bin/env sh
set -eu

{
  if [ -n "${FAKE_RUN_ID:-}" ]; then
    printf '%s-' "$FAKE_RUN_ID"
  fi
  printf 'ssh'
  for arg in "$@"; do
    printf '|%s' "$arg"
  done
  printf '\n'
} >> "$FAKE_WITNESS"

if [ -n "${FAKE_LOCK_WITNESS:-}" ]; then
  printf '%s-start\n' "$FAKE_RUN_ID" >> "$FAKE_LOCK_WITNESS"
  printf '%s-ssh-start\n' "$FAKE_RUN_ID" >> "$FAKE_WITNESS"
  sleep 0.2
  printf '%s-end\n' "$FAKE_RUN_ID" >> "$FAKE_LOCK_WITNESS"
  printf '%s-ssh-end\n' "$FAKE_RUN_ID" >> "$FAKE_WITNESS"
fi

case "${FAKE_SSH_MODE:-success}" in
  success)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  nonzero-success)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    exit "${FAKE_SSH_STATUS:-52}"
    ;;
  zero-head)
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  two-head)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  mismatch-head)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_OTHER_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  zero-cargo)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  two-cargo)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  mismatch-cargo)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_OTHER_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  zero-ui)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  two-ui)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  mismatch-ui)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_OTHER_UI_LOCK_SHA256"
    printf '%s\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  missing-ok)
    printf 'WIN_CI_HEAD=%s\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\n' "$FAKE_UI_LOCK_SHA256"
    ;;
  crlf)
    printf 'WIN_CI_HEAD=%s\r\n' "$FAKE_GIT_SHA"
    printf 'WIN_CI_CARGO_LOCK_SHA256=%s\r\n' "$FAKE_CARGO_LOCK_SHA256"
    printf 'WIN_CI_UI_LOCK_SHA256=%s\r\n' "$FAKE_UI_LOCK_SHA256"
    printf '%s\r\n' '=== WIN_CI_OK: fake native gate passed ==='
    ;;
  *) exit 90 ;;
esac
EOF
chmod +x "$FAKE_SSH"

FAKE_GIT_SHA=1111111111111111111111111111111111111111
FAKE_OTHER_SHA=2222222222222222222222222222222222222222
FAKE_CARGO_LOCK_CONTENT="fake Cargo.lock snapshot"
FAKE_UI_LOCK_CONTENT="fake ui/package-lock.json snapshot"
FAKE_CARGO_LOCK_SHA256=$(printf '%s\n' "$FAKE_CARGO_LOCK_CONTENT" | sha256sum | awk '{ print $1 }')
FAKE_UI_LOCK_SHA256=$(printf '%s\n' "$FAKE_UI_LOCK_CONTENT" | sha256sum | awk '{ print $1 }')
FAKE_OTHER_CARGO_LOCK_SHA256=3333333333333333333333333333333333333333333333333333333333333333
FAKE_OTHER_UI_LOCK_SHA256=4444444444444444444444444444444444444444444444444444444444444444
export FAKE_GIT_SHA FAKE_OTHER_SHA FAKE_CARGO_LOCK_CONTENT FAKE_UI_LOCK_CONTENT
export FAKE_CARGO_LOCK_SHA256 FAKE_UI_LOCK_SHA256
export FAKE_OTHER_CARGO_LOCK_SHA256 FAKE_OTHER_UI_LOCK_SHA256

reset_fake_transfer() {
  fake_case=$1
  FAKE_CASE_DIR="$TMP_ROOT/$fake_case"
  rm -rf "$FAKE_CASE_DIR"
  mkdir -p "$FAKE_CASE_DIR/state/common" "$FAKE_CASE_DIR/sha"
  FAKE_WITNESS="$FAKE_CASE_DIR/witness"
  FAKE_GIT_STATE_DIR="$FAKE_CASE_DIR/state"
  FAKE_GIT_COMMON_DIR="$FAKE_CASE_DIR/state/common"
  FAKE_BINDING_FILE="$FAKE_CASE_DIR/sha/win-host-ci-source-binding.json"
  : > "$FAKE_WITNESS"
  export FAKE_WITNESS FAKE_GIT_STATE_DIR FAKE_GIT_COMMON_DIR
  unset FAKE_GIT_FAIL_PHASE FAKE_GIT_FAIL_STATUS FAKE_GIT_STASH_EMPTY
  unset FAKE_GIT_UNMERGED FAKE_GIT_UNTRACKED FAKE_GIT_FAIL_CLEANUP
  unset FAKE_GIT_CLEANUP_STATUS FAKE_GIT_HEAD_MISMATCH FAKE_SCP_FAIL
  unset FAKE_SCP_STATUS FAKE_SCP_COPY_TO FAKE_SCP_SOURCE_FILE
  unset FAKE_SCP_BINDING_FAIL
  unset FAKE_SSH_MODE FAKE_SSH_STATUS FAKE_GIT_RELEASE_DIRTY
  unset FAKE_EXPECTED_RELEASE_COMMIT FAKE_LOCK_WITNESS FAKE_RUN_ID
}

run_fake_sync() {
  if SYNC_OUTPUT=$(
    WIN_REMOTE_HOST=fake@example.invalid \
      WIN_CI_BINDING_FILE="$FAKE_BINDING_FILE" \
      EXPECTED_RELEASE_COMMIT="${FAKE_EXPECTED_RELEASE_COMMIT:-}" \
      GIT="$FAKE_GIT" \
      SCP="$FAKE_SCP" \
      sh "$REPO_ROOT/scripts/sync-win-host.sh" 2>&1
  ); then
    SYNC_STATUS=0
  else
    SYNC_STATUS=$?
  fi
}

run_fake_orchestrator() {
  if ORCHESTRATOR_OUTPUT=$(
    WIN_REMOTE_HOST=fake@example.invalid \
      WIN_CI_BINDING_FILE="$FAKE_BINDING_FILE" \
      EXPECTED_RELEASE_COMMIT="${FAKE_EXPECTED_RELEASE_COMMIT:-}" \
      GIT="$FAKE_GIT" \
      SCP="$FAKE_SCP" \
      SSH="$FAKE_SSH" \
      sh "$REPO_ROOT/scripts/win-host-ci.sh" 2>&1
  ); then
    ORCHESTRATOR_STATUS=0
  else
    ORCHESTRATOR_STATUS=$?
  fi
}

binding_field() {
  binding_path=$1
  binding_name=$2
  sed -n "s/^  \"$binding_name\": \"\([0-9a-f]*\)\"[,]\{0,1\}$/\\1/p" "$binding_path"
}

reset_fake_transfer "dirty-success"
run_fake_sync
assert_eq "dirty transfer succeeds" "0" "$SYNC_STATUS"
assert_contains \
  "dirty transfer reports exact snapshot" \
  "$SYNC_OUTPUT" \
  "SYNC_WIN_HOST_OK commit=$FAKE_GIT_SHA cargo_lock_sha256=$FAKE_CARGO_LOCK_SHA256 ui_package_lock_sha256=$FAKE_UI_LOCK_SHA256"
assert_eq "dirty transfer binds exact commit" "$FAKE_GIT_SHA" "$(binding_field "$FAKE_BINDING_FILE" commit)"
assert_eq "dirty transfer binds exact Cargo.lock" "$FAKE_CARGO_LOCK_SHA256" "$(binding_field "$FAKE_BINDING_FILE" cargo_lock_sha256)"
assert_eq "dirty transfer binds exact UI lock" "$FAKE_UI_LOCK_SHA256" "$(binding_field "$FAKE_BINDING_FILE" ui_package_lock_sha256)"
assert_line_order \
  "dirty transfer phase order" \
  "$FAKE_WITNESS" \
  "git|ls-files|--unmerged" \
  "git|ls-files|--others|--exclude-standard" \
  "git|stash|create" \
  "git|show|$FAKE_GIT_SHA:Cargo.lock" \
  "git|show|$FAKE_GIT_SHA:ui/package-lock.json" \
  "git|update-ref|refs/heads/__swsync|$FAKE_GIT_SHA|" \
  "git|bundle|create|" \
  "git|bundle|verify|" \
  "git|bundle|list-heads|" \
  "git|update-ref|-d|refs/heads/__swsync|$FAKE_GIT_SHA" \
  "fake@example.invalid:swbuild.bundle" \
  "fake@example.invalid:win-host-ci-source-binding.json"

reset_fake_transfer "clean-success"
FAKE_GIT_STASH_EMPTY=1
export FAKE_GIT_STASH_EMPTY
run_fake_sync
assert_eq "clean transfer succeeds" "0" "$SYNC_STATUS"
assert_line_order \
  "clean transfer falls back to HEAD" \
  "$FAKE_WITNESS" \
  "git|stash|create" \
  "git|rev-parse|HEAD" \
  "git|update-ref|refs/heads/__swsync|$FAKE_GIT_SHA|"

reset_fake_transfer "release-clean-success"
FAKE_EXPECTED_RELEASE_COMMIT=$FAKE_GIT_SHA
export FAKE_EXPECTED_RELEASE_COMMIT
run_fake_sync
assert_eq "clean release transfer succeeds" "0" "$SYNC_STATUS"
assert_not_contains "release transfer never creates a synthetic snapshot" "$(cat "$FAKE_WITNESS")" "git|stash|create"
assert_line_order \
  "release binding precedes transfer" \
  "$FAKE_WITNESS" \
  "git|status|--porcelain=v1|-z|--untracked-files=all|--ignore-submodules=none" \
  "git|rev-parse|HEAD" \
  "git|show|$FAKE_GIT_SHA:Cargo.lock" \
  "fake@example.invalid:swbuild.bundle"

reset_fake_transfer "release-dirty-refusal"
FAKE_EXPECTED_RELEASE_COMMIT=$FAKE_GIT_SHA
FAKE_GIT_RELEASE_DIRTY=1
export FAKE_EXPECTED_RELEASE_COMMIT FAKE_GIT_RELEASE_DIRTY
run_fake_sync
if [ "$SYNC_STATUS" -eq 0 ]; then
  fail "release mode must refuse a dirty synthetic snapshot"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "release dirty refusal is actionable" \
  "$SYNC_OUTPUT" \
  "release mode refuses a synthetic or dirty snapshot"
assert_not_contains "release dirty refusal never creates a stash" "$(cat "$FAKE_WITNESS")" "git|stash|create"
assert_not_contains "release dirty refusal makes no SCP call" "$(cat "$FAKE_WITNESS")" "scp|"
assert_not_contains "release dirty refusal makes no SSH call" "$(cat "$FAKE_WITNESS")" "ssh|"

reset_fake_transfer "initialize-failure"
if SYNC_OUTPUT=$(
  WIN_REMOTE_HOST= \
    WIN_CI_BINDING_FILE="$FAKE_BINDING_FILE" \
    GIT="$FAKE_GIT" \
    SCP="$FAKE_SCP" \
    sh "$REPO_ROOT/scripts/sync-win-host.sh" 2>&1
); then
  SYNC_STATUS=0
else
  SYNC_STATUS=$?
fi
if [ "$SYNC_STATUS" -eq 0 ]; then
  fail "missing remote host must fail initialize"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains "initialize failure names phase" "$SYNC_OUTPUT" "ERROR: sync-win-host: initialize failed"
assert_not_contains "initialize failure never invokes SCP" "$(cat "$FAKE_WITNESS")" "scp|"

for failure_spec in \
  "guard:guard" \
  "resolve-sha:resolve-sha" \
  "resolve-binding:resolve-binding" \
  "create-temp-ref:create-temp-ref" \
  "create-bundle:create-bundle" \
  "verify-bundle:verify-bundle" \
  "delete-temp-ref:delete-temp-ref"
do
  expected_phase=${failure_spec%%:*}
  fake_phase=${failure_spec#*:}
  reset_fake_transfer "failure-$fake_phase"
  FAKE_GIT_FAIL_PHASE=$fake_phase
  export FAKE_GIT_FAIL_PHASE
  run_fake_sync
  if [ "$SYNC_STATUS" -eq 0 ]; then
    fail "$expected_phase injection must fail"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
  assert_contains \
    "$expected_phase failure names phase" \
    "$SYNC_OUTPUT" \
    "ERROR: sync-win-host: $expected_phase failed"
  assert_not_contains \
    "$expected_phase failure never invokes SCP" \
    "$(cat "$FAKE_WITNESS")" \
    "scp|"
done

reset_fake_transfer "resolve-fallback-failure"
FAKE_GIT_STASH_EMPTY=1
FAKE_GIT_FAIL_PHASE=resolve-fallback
export FAKE_GIT_STASH_EMPTY FAKE_GIT_FAIL_PHASE
run_fake_sync
if [ "$SYNC_STATUS" -eq 0 ]; then
  fail "clean HEAD fallback failure must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "clean HEAD fallback names resolve phase" \
  "$SYNC_OUTPUT" \
  "ERROR: sync-win-host: resolve-sha failed"
assert_not_contains \
  "clean HEAD fallback failure never invokes SCP" \
  "$(cat "$FAKE_WITNESS")" \
  "scp|"

reset_fake_transfer "bundle-head-mismatch"
FAKE_GIT_HEAD_MISMATCH=1
export FAKE_GIT_HEAD_MISMATCH
run_fake_sync
if [ "$SYNC_STATUS" -eq 0 ]; then
  fail "bundle head mismatch must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "bundle head mismatch names verify phase" \
  "$SYNC_OUTPUT" \
  "ERROR: sync-win-host: verify-bundle failed"
assert_not_contains \
  "bundle head mismatch never invokes SCP" \
  "$(cat "$FAKE_WITNESS")" \
  "scp|"

reset_fake_transfer "scp-failure"
FAKE_SCP_FAIL=1
export FAKE_SCP_FAIL
run_fake_sync
assert_eq "SCP failure preserves command status" "41" "$SYNC_STATUS"
assert_contains "SCP failure names phase" "$SYNC_OUTPUT" "ERROR: sync-win-host: scp failed"
assert_eq \
  "SCP failure witnesses exactly one attempt" \
  "1" \
  "$(grep -c '^scp|' "$FAKE_WITNESS")"
assert_file_exists "SCP failure preserves atomic local binding" "$FAKE_BINDING_FILE"

reset_fake_transfer "binding-scp-failure"
FAKE_SCP_BINDING_FAIL=1
export FAKE_SCP_BINDING_FAIL
run_fake_sync
assert_eq "binding SCP failure preserves command status" "42" "$SYNC_STATUS"
assert_contains \
  "binding SCP failure names phase" \
  "$SYNC_OUTPUT" \
  "ERROR: sync-win-host: scp-binding failed"
assert_eq \
  "binding SCP failure happens on the second SCP" \
  "2" \
  "$(grep -c '^scp|' "$FAKE_WITNESS")"
assert_file_exists "binding SCP failure preserves atomic local binding" "$FAKE_BINDING_FILE"

reset_fake_transfer "cleanup-failure"
FAKE_GIT_FAIL_PHASE=create-bundle
FAKE_GIT_FAIL_STATUS=37
FAKE_GIT_FAIL_CLEANUP=1
FAKE_GIT_CLEANUP_STATUS=67
export FAKE_GIT_FAIL_PHASE FAKE_GIT_FAIL_STATUS
export FAKE_GIT_FAIL_CLEANUP FAKE_GIT_CLEANUP_STATUS
run_fake_sync
assert_eq "cleanup failure preserves original status" "37" "$SYNC_STATUS"
assert_contains \
  "cleanup failure warns without masking" \
  "$SYNC_OUTPUT" \
  "WARNING: sync-win-host: cleanup failed for refs/heads/__swsync; preserving create-bundle exit 37"

reset_fake_transfer "stale-bundle"
stale_bundle=$FAKE_CASE_DIR/stale.bundle
printf '%s\n' "stale bundle bytes" > "$stale_bundle"
FAKE_GIT_FAIL_PHASE=create-bundle
export FAKE_GIT_FAIL_PHASE
run_fake_sync
assert_not_contains \
  "failed fresh bundle creation never ships stale bundle" \
  "$(cat "$FAKE_WITNESS")" \
  "scp|"
assert_eq \
  "stale bundle bytes remain untouched" \
  "stale bundle bytes" \
  "$(cat "$stale_bundle")"

reset_fake_transfer "fresh-bundle"
FAKE_SCP_SOURCE_FILE=$FAKE_CASE_DIR/scp-source
export FAKE_SCP_SOURCE_FILE
run_fake_sync
assert_eq "fresh bundle transfer succeeds" "0" "$SYNC_STATUS"
fresh_source=$(cat "$FAKE_SCP_SOURCE_FILE")
assert_not_contains "fresh transfer does not ship stale path" "$fresh_source" "stale.bundle"
assert_contains "fresh transfer uses mktemp bundle name" "$fresh_source" "target/win-host-ci.bundle."
assert_file_absent "successful transfer removes local temp bundle" "$fresh_source"
assert_contains \
  "successful transfer preserves fixed remote bundle name" \
  "$(cat "$FAKE_WITNESS")" \
  "fake@example.invalid:swbuild.bundle"

reset_fake_transfer "unique-bundle-1"
FAKE_SCP_SOURCE_FILE=$FAKE_CASE_DIR/source-1
export FAKE_SCP_SOURCE_FILE
run_fake_sync
source_one=$(cat "$FAKE_SCP_SOURCE_FILE")
reset_fake_transfer "unique-bundle-2"
FAKE_SCP_SOURCE_FILE=$FAKE_CASE_DIR/source-2
export FAKE_SCP_SOURCE_FILE
run_fake_sync
source_two=$(cat "$FAKE_SCP_SOURCE_FILE")
if [ "$source_one" = "$source_two" ]; then
  fail "separate attempts must use unique local bundle names"
fi
ASSERTIONS=$((ASSERTIONS + 1))

reset_fake_transfer "sentinel-ref"
printf '%s\n' "$FAKE_OTHER_SHA" > "$FAKE_GIT_STATE_DIR/swsync"
run_fake_sync
if [ "$SYNC_STATUS" -eq 0 ]; then
  fail "pre-existing __swsync ref must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "sentinel ref failure names create phase" \
  "$SYNC_OUTPUT" \
  "ERROR: sync-win-host: create-temp-ref failed"
assert_eq \
  "sentinel ref remains byte-exact" \
  "$FAKE_OTHER_SHA" \
  "$(cat "$FAKE_GIT_STATE_DIR/swsync")"
assert_not_contains \
  "sentinel ref failure never invokes SCP" \
  "$(cat "$FAKE_WITNESS")" \
  "scp|"

new_real_transfer_repo() {
  real_name=$1
  REAL_CASE_DIR="$TMP_ROOT/real-$real_name"
  REAL_REPO="$REAL_CASE_DIR/repo"
  REAL_BUNDLE="$REAL_CASE_DIR/custody.bundle"
  REAL_BINDING_FILE="$REAL_CASE_DIR/sha/win-host-ci-source-binding.json"
  REAL_WITNESS="$REAL_CASE_DIR/witness"
  mkdir -p "$REAL_REPO/scripts" "$REAL_REPO/ui" "$REAL_CASE_DIR/sha"
  cp "$REPO_ROOT/scripts/sync-win-host.sh" "$REAL_REPO/scripts/sync-win-host.sh"
  cp "$REPO_ROOT/scripts/check-win-sync-tree.sh" "$REAL_REPO/scripts/check-win-sync-tree.sh"
  printf '%s\n' "target/" > "$REAL_REPO/.gitignore"
  printf '%s\n' "baseline" > "$REAL_REPO/tracked.txt"
  printf '%s\n' "delete me" > "$REAL_REPO/delete-me.txt"
  printf '%s\n' "real Cargo.lock bytes" > "$REAL_REPO/Cargo.lock"
  printf '%s\n' "real ui/package-lock.json bytes" > "$REAL_REPO/ui/package-lock.json"
  git -C "$REAL_REPO" init -q
  git -C "$REAL_REPO" config user.name "solstone gate test"
  git -C "$REAL_REPO" config user.email "gate-test@example.invalid"
  git -C "$REAL_REPO" add .
  git -C "$REAL_REPO" commit -qm "baseline"
  REAL_BASE_BRANCH=$(git -C "$REAL_REPO" branch --show-current)
  : > "$REAL_WITNESS"
  REAL_EXPECTED_RELEASE_COMMIT=
}

run_real_sync() {
  FAKE_WITNESS=$REAL_WITNESS
  FAKE_SCP_COPY_TO=$REAL_BUNDLE
  export FAKE_WITNESS FAKE_SCP_COPY_TO
  unset FAKE_SCP_FAIL FAKE_SCP_SOURCE_FILE FAKE_RUN_ID
  if REAL_SYNC_OUTPUT=$(
    WIN_REMOTE_HOST=fake@example.invalid \
      WIN_CI_BINDING_FILE="$REAL_BINDING_FILE" \
      EXPECTED_RELEASE_COMMIT="$REAL_EXPECTED_RELEASE_COMMIT" \
      GIT=git \
      SCP="$FAKE_SCP" \
      sh "$REAL_REPO/scripts/sync-win-host.sh" 2>&1
  ); then
    REAL_SYNC_STATUS=0
  else
    REAL_SYNC_STATUS=$?
  fi
}

checkout_real_bundle() {
  REAL_CHECKOUT="$REAL_CASE_DIR/checkout"
  mkdir "$REAL_CHECKOUT"
  git -C "$REAL_CHECKOUT" init -q
  git -C "$REAL_CHECKOUT" fetch -q "$REAL_BUNDLE" \
    refs/heads/__swsync:refs/heads/__swsync
  git -C "$REAL_CHECKOUT" checkout -q __swsync
}

tree_file_list() {
  tree_root=$1
  (
    cd "$tree_root"
    find . -path './.git' -prune -o -type f -print | LC_ALL=C sort
  )
}

assert_real_tree_matches_checkout() {
  label=$1
  source_files=$(tree_file_list "$REAL_REPO")
  checkout_files=$(tree_file_list "$REAL_CHECKOUT")
  assert_eq "$label file set" "$source_files" "$checkout_files"
  for relative_file in $source_files; do
    if ! cmp "$REAL_REPO/$relative_file" "$REAL_CHECKOUT/$relative_file"; then
      fail "$label byte mismatch: $relative_file"
    fi
  done
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_real_transfer_snapshot() {
  label=$1
  assert_eq "$label transfer succeeds" "0" "$REAL_SYNC_STATUS"
  assert_file_exists "$label bundle reaches test custody" "$REAL_BUNDLE"
  assert_file_exists "$label binding exists" "$REAL_BINDING_FILE"
  checkout_real_bundle
  assert_eq \
    "$label checked-out HEAD equals intended SHA" \
    "$(binding_field "$REAL_BINDING_FILE" commit)" \
    "$(git -C "$REAL_CHECKOUT" rev-parse HEAD)"
  assert_eq \
    "$label Cargo.lock digest binds exact snapshot bytes" \
    "$(binding_field "$REAL_BINDING_FILE" cargo_lock_sha256)" \
    "$(sha256sum "$REAL_CHECKOUT/Cargo.lock" | awk '{ print $1 }')"
  assert_eq \
    "$label UI lock digest binds exact snapshot bytes" \
    "$(binding_field "$REAL_BINDING_FILE" ui_package_lock_sha256)" \
    "$(sha256sum "$REAL_CHECKOUT/ui/package-lock.json" | awk '{ print $1 }')"
  assert_real_tree_matches_checkout "$label"
}

new_real_transfer_repo "clean"
run_real_sync
assert_real_transfer_snapshot "clean exact-tree"

new_real_transfer_repo "unstaged-mod"
printf '%s\n' "unstaged final bytes" > "$REAL_REPO/tracked.txt"
run_real_sync
assert_real_transfer_snapshot "unstaged modification exact-tree"

new_real_transfer_repo "release-dirty"
REAL_EXPECTED_RELEASE_COMMIT=$(git -C "$REAL_REPO" rev-parse HEAD)
printf '%s\n' "release dirty bytes" > "$REAL_REPO/tracked.txt"
run_real_sync
if [ "$REAL_SYNC_STATUS" -eq 0 ]; then
  fail "real release mode must refuse a synthetic dirty snapshot"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "real release dirty refusal is actionable" \
  "$REAL_SYNC_OUTPUT" \
  "release mode refuses a synthetic or dirty snapshot"
assert_not_contains "real release dirty refusal never calls SCP" "$(cat "$REAL_WITNESS")" "scp|"
assert_file_absent "real release dirty refusal writes no binding" "$REAL_BINDING_FILE"

new_real_transfer_repo "staged-add"
printf '%s\n' "staged addition bytes" > "$REAL_REPO/added.txt"
git -C "$REAL_REPO" add added.txt
run_real_sync
assert_real_transfer_snapshot "staged addition exact-tree"

new_real_transfer_repo "staged-delete"
git -C "$REAL_REPO" rm -q delete-me.txt
run_real_sync
assert_real_transfer_snapshot "staged deletion exact-tree"

new_real_transfer_repo "staged-unstaged"
printf '%s\n' "staged bytes" > "$REAL_REPO/tracked.txt"
git -C "$REAL_REPO" add tracked.txt
printf '%s\n' "staged plus unstaged final bytes" > "$REAL_REPO/tracked.txt"
run_real_sync
assert_real_transfer_snapshot "staged plus unstaged exact-tree"

new_real_transfer_repo "untracked"
printf '%s\n' "must not be omitted" > "$REAL_REPO/untracked-path.txt"
run_real_sync
if [ "$REAL_SYNC_STATUS" -eq 0 ]; then
  fail "untracked non-ignored file must fail transfer"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "untracked transfer failure names path" \
  "$REAL_SYNC_OUTPUT" \
  "untracked-path.txt"
assert_contains \
  "untracked transfer failure names guard phase" \
  "$REAL_SYNC_OUTPUT" \
  "ERROR: sync-win-host: guard failed"
assert_not_contains \
  "untracked transfer failure never invokes SCP" \
  "$(cat "$REAL_WITNESS")" \
  "scp|"
assert_file_count \
  "untracked transfer creates no bundle" \
  "0" \
  "$REAL_REPO/target" \
  "win-host-ci.bundle.*"
if git -C "$REAL_REPO" show-ref --verify --quiet refs/heads/__swsync; then
  fail "untracked transfer must not create __swsync"
fi
ASSERTIONS=$((ASSERTIONS + 1))

new_real_transfer_repo "unmerged"
git -C "$REAL_REPO" checkout -qb conflict-side
printf '%s\n' "side bytes" > "$REAL_REPO/tracked.txt"
git -C "$REAL_REPO" commit -qam "side conflict"
git -C "$REAL_REPO" checkout -q "$REAL_BASE_BRANCH"
printf '%s\n' "base bytes" > "$REAL_REPO/tracked.txt"
git -C "$REAL_REPO" commit -qam "base conflict"
if git -C "$REAL_REPO" merge conflict-side >"$REAL_CASE_DIR/merge-output" 2>&1; then
  fail "fixture merge must produce an unmerged index"
fi
ASSERTIONS=$((ASSERTIONS + 1))
run_real_sync
if [ "$REAL_SYNC_STATUS" -eq 0 ]; then
  fail "unmerged index must fail transfer"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "unmerged transfer failure names path" \
  "$REAL_SYNC_OUTPUT" \
  "tracked.txt"
assert_contains \
  "unmerged transfer failure explains index state" \
  "$REAL_SYNC_OUTPUT" \
  "index contains unmerged entries"
assert_contains \
  "unmerged transfer failure gives repair action" \
  "$REAL_SYNC_OUTPUT" \
  "Resolve or abort the merge"
assert_not_contains \
  "unmerged transfer failure never invokes SCP" \
  "$(cat "$REAL_WITNESS")" \
  "scp|"
assert_file_count \
  "unmerged transfer creates no bundle" \
  "0" \
  "$REAL_REPO/target" \
  "win-host-ci.bundle.*"
if git -C "$REAL_REPO" show-ref --verify --quiet refs/heads/__swsync; then
  fail "unmerged transfer must not create __swsync"
fi
ASSERTIONS=$((ASSERTIONS + 1))

new_real_transfer_repo "ignored"
printf '%s\n' "ignored.tmp" >> "$REAL_REPO/.gitignore"
git -C "$REAL_REPO" add .gitignore
git -C "$REAL_REPO" commit -qm "ignore local probe"
printf '%s\n' "ignored bytes" > "$REAL_REPO/ignored.tmp"
run_real_sync
assert_eq "ignored file permits transfer" "0" "$REAL_SYNC_STATUS"
checkout_real_bundle
assert_eq \
  "ignored transfer checked-out HEAD equals intended SHA" \
  "$(binding_field "$REAL_BINDING_FILE" commit)" \
  "$(git -C "$REAL_CHECKOUT" rev-parse HEAD)"
assert_file_absent "ignored file is absent from bundle" "$REAL_CHECKOUT/ignored.tmp"

new_real_transfer_repo "sentinel"
sentinel_before=$(git -C "$REAL_REPO" rev-parse HEAD)
git -C "$REAL_REPO" update-ref refs/heads/__swsync "$sentinel_before"
run_real_sync
if [ "$REAL_SYNC_STATUS" -eq 0 ]; then
  fail "real pre-existing __swsync ref must fail transfer"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "real sentinel failure names create phase" \
  "$REAL_SYNC_OUTPUT" \
  "ERROR: sync-win-host: create-temp-ref failed"
assert_eq \
  "real sentinel ref remains unchanged" \
  "$sentinel_before" \
  "$(git -C "$REAL_REPO" rev-parse refs/heads/__swsync)"
assert_not_contains \
  "real sentinel failure never invokes SCP" \
  "$(cat "$REAL_WITNESS")" \
  "scp|"

reset_fake_transfer "orchestrator-success"
FAKE_SSH_MODE=success
export FAKE_SSH_MODE
run_fake_orchestrator
assert_eq "orchestrator success exits zero" "0" "$ORCHESTRATOR_STATUS"
assert_contains \
  "orchestrator reports verified three-field binding" \
  "$ORCHESTRATOR_OUTPUT" \
  "WIN_HOST_CI_VERIFIED commit=$FAKE_GIT_SHA cargo_lock_sha256=$FAKE_CARGO_LOCK_SHA256 ui_package_lock_sha256=$FAKE_UI_LOCK_SHA256"
assert_contains \
  "orchestrator invokes exact box command" \
  "$(cat "$FAKE_WITNESS")" \
  'cmd /d /c "set EXPECTED_RELEASE_COMMIT='
assert_contains \
  "orchestrator passes Cargo.lock binding" \
  "$(cat "$FAKE_WITNESS")" \
  "EXPECTED_CARGO_LOCK_SHA256=$FAKE_CARGO_LOCK_SHA256"
assert_contains \
  "orchestrator passes UI lock binding" \
  "$(cat "$FAKE_WITNESS")" \
  "EXPECTED_UI_PACKAGE_LOCK_SHA256=$FAKE_UI_LOCK_SHA256"

reset_fake_transfer "orchestrator-ssh-nonzero"
FAKE_SSH_MODE=nonzero-success
export FAKE_SSH_MODE
run_fake_orchestrator
assert_eq "nonzero SSH status is preserved" "52" "$ORCHESTRATOR_STATUS"
assert_contains \
  "nonzero SSH fails before trusting success-looking stdout" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: ssh failed (exit 52)"
assert_not_contains \
  "nonzero SSH is never reported verified" \
  "$ORCHESTRATOR_OUTPUT" \
  "WIN_HOST_CI_VERIFIED"

reset_fake_transfer "orchestrator-zero-head"
FAKE_SSH_MODE=zero-head
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "zero WIN_CI_HEAD lines must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "zero WIN_CI_HEAD count is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: expected exactly one WIN_CI_HEAD line, found 0"

reset_fake_transfer "orchestrator-two-heads"
FAKE_SSH_MODE=two-head
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "two WIN_CI_HEAD lines must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "two WIN_CI_HEAD count is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: expected exactly one WIN_CI_HEAD line, found 2"

reset_fake_transfer "orchestrator-head-mismatch"
FAKE_SSH_MODE=mismatch-head
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "remote HEAD mismatch must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "remote HEAD mismatch reports both SHAs" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: remote HEAD mismatch: expected $FAKE_GIT_SHA, actual $FAKE_OTHER_SHA"

reset_fake_transfer "orchestrator-zero-cargo-lock"
FAKE_SSH_MODE=zero-cargo
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "zero WIN_CI_CARGO_LOCK_SHA256 lines must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "zero Cargo.lock acknowledgement is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: expected exactly one WIN_CI_CARGO_LOCK_SHA256 line, found 0"

reset_fake_transfer "orchestrator-two-cargo-locks"
FAKE_SSH_MODE=two-cargo
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "two WIN_CI_CARGO_LOCK_SHA256 lines must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "duplicate Cargo.lock acknowledgement is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: expected exactly one WIN_CI_CARGO_LOCK_SHA256 line, found 2"

reset_fake_transfer "orchestrator-cargo-lock-mismatch"
FAKE_SSH_MODE=mismatch-cargo
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "remote Cargo.lock mismatch must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "remote Cargo.lock mismatch reports both digests" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: remote Cargo.lock SHA-256 mismatch: expected $FAKE_CARGO_LOCK_SHA256, actual $FAKE_OTHER_CARGO_LOCK_SHA256"

reset_fake_transfer "orchestrator-zero-ui-lock"
FAKE_SSH_MODE=zero-ui
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "zero WIN_CI_UI_LOCK_SHA256 lines must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "zero UI-lock acknowledgement is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: expected exactly one WIN_CI_UI_LOCK_SHA256 line, found 0"

reset_fake_transfer "orchestrator-two-ui-locks"
FAKE_SSH_MODE=two-ui
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "two WIN_CI_UI_LOCK_SHA256 lines must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "duplicate UI-lock acknowledgement is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: expected exactly one WIN_CI_UI_LOCK_SHA256 line, found 2"

reset_fake_transfer "orchestrator-ui-lock-mismatch"
FAKE_SSH_MODE=mismatch-ui
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "remote UI lock mismatch must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "remote UI lock mismatch reports both digests" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: remote ui/package-lock.json SHA-256 mismatch: expected $FAKE_UI_LOCK_SHA256, actual $FAKE_OTHER_UI_LOCK_SHA256"

reset_fake_transfer "orchestrator-missing-ok"
FAKE_SSH_MODE=missing-ok
export FAKE_SSH_MODE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "missing WIN_CI_OK acknowledgement must fail"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "missing WIN_CI_OK is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: expected exactly one WIN_CI_OK acknowledgement, found 0"

reset_fake_transfer "orchestrator-crlf"
FAKE_SSH_MODE=crlf
export FAKE_SSH_MODE
run_fake_orchestrator
assert_eq "CRLF native output succeeds" "0" "$ORCHESTRATOR_STATUS"
assert_contains \
  "CRLF native output verifies binding" \
  "$ORCHESTRATOR_OUTPUT" \
  "WIN_HOST_CI_VERIFIED commit=$FAKE_GIT_SHA cargo_lock_sha256=$FAKE_CARGO_LOCK_SHA256 ui_package_lock_sha256=$FAKE_UI_LOCK_SHA256"

reset_fake_transfer "orchestrator-sync-failure"
FAKE_GIT_FAIL_PHASE=create-bundle
export FAKE_GIT_FAIL_PHASE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "sync failure must fail orchestrator"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "orchestrator names sync failure" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: win-host-ci: sync failed"
assert_not_contains \
  "sync failure never invokes SSH" \
  "$(cat "$FAKE_WITNESS")" \
  "ssh|"

reset_fake_transfer "orchestrator-local-binding-failure"
FAKE_GIT_FAIL_PHASE=resolve-binding
export FAKE_GIT_FAIL_PHASE
run_fake_orchestrator
if [ "$ORCHESTRATOR_STATUS" -eq 0 ]; then
  fail "local source-binding failure must fail orchestrator"
fi
ASSERTIONS=$((ASSERTIONS + 1))
assert_contains \
  "local source-binding failure is diagnostic" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: sync-win-host: resolve-binding failed"
assert_not_contains \
  "local source-binding failure makes no SCP call" \
  "$(cat "$FAKE_WITNESS")" \
  "scp|"
assert_not_contains \
  "local source-binding failure makes no SSH call" \
  "$(cat "$FAKE_WITNESS")" \
  "ssh|"
assert_file_absent "local source-binding failure writes no binding" "$FAKE_BINDING_FILE"

reset_fake_transfer "orchestrator-binding-scp-failure"
FAKE_SCP_BINDING_FAIL=1
export FAKE_SCP_BINDING_FAIL
run_fake_orchestrator
assert_eq "orchestrator preserves binding SCP status" "42" "$ORCHESTRATOR_STATUS"
assert_contains \
  "orchestrator surfaces binding SCP failure" \
  "$ORCHESTRATOR_OUTPUT" \
  "ERROR: sync-win-host: scp-binding failed"
assert_not_contains \
  "binding SCP failure prevents SSH" \
  "$(cat "$FAKE_WITNESS")" \
  "ssh|"

reset_fake_transfer "orchestrator-lock"
FAKE_LOCK_WITNESS=$FAKE_CASE_DIR/lock-witness
: > "$FAKE_LOCK_WITNESS"
export FAKE_LOCK_WITNESS
lock_output_one=$FAKE_CASE_DIR/output-1
lock_output_two=$FAKE_CASE_DIR/output-2
WIN_REMOTE_HOST=fake@example.invalid \
  WIN_CI_BINDING_FILE="$FAKE_BINDING_FILE" \
  GIT="$FAKE_GIT" \
  SCP="$FAKE_SCP" \
  SSH="$FAKE_SSH" \
  FAKE_RUN_ID=1 \
  sh "$REPO_ROOT/scripts/win-host-ci.sh" >"$lock_output_one" 2>&1 &
lock_pid_one=$!
lock_started=0
lock_poll=0
while [ "$lock_poll" -lt 100 ]; do
  if grep -q '^1-start$' "$FAKE_LOCK_WITNESS"; then
    lock_started=1
    break
  fi
  lock_poll=$((lock_poll + 1))
  sleep 0.01
done
assert_eq "first orchestrator reaches locked SSH phase" "1" "$lock_started"
WIN_REMOTE_HOST=fake@example.invalid \
  WIN_CI_BINDING_FILE="$FAKE_BINDING_FILE" \
  GIT="$FAKE_GIT" \
  SCP="$FAKE_SCP" \
  SSH="$FAKE_SSH" \
  FAKE_RUN_ID=2 \
  sh "$REPO_ROOT/scripts/win-host-ci.sh" >"$lock_output_two" 2>&1 &
lock_pid_two=$!
if wait "$lock_pid_one"; then
  lock_status_one=0
else
  lock_status_one=$?
fi
if wait "$lock_pid_two"; then
  lock_status_two=0
else
  lock_status_two=$?
fi
assert_eq "first serialized orchestrator succeeds" "0" "$lock_status_one"
assert_eq "second serialized orchestrator succeeds" "0" "$lock_status_two"
assert_eq \
  "orchestrator lock prevents overlapping SSH phases" \
  "1-start
1-end
2-start
2-end" \
  "$(cat "$FAKE_LOCK_WITNESS")"
assert_line_order \
  "lock covers sync through SSH verification" \
  "$FAKE_WITNESS" \
  "1-git|ls-files|--unmerged" \
  "1-scp|-o|ControlMaster=auto" \
  "1-ssh-start" \
  "1-ssh-end" \
  "2-git|ls-files|--unmerged" \
  "2-scp|-o|ControlMaster=auto" \
  "2-ssh-start" \
  "2-ssh-end"

echo "deterministic-gates.test.sh: $ASSERTIONS assertions passed"
