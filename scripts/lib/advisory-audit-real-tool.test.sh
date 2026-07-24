#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
CARGO_BIN=${CARGO:-$HOME/.cargo/bin/cargo}
GIT_BIN=${GIT:-/usr/bin/git}
MINISIGN_BIN=${MINISIGN:-$HOME/.local/bin/minisign}
CARGO_DENY_BIN=${CARGO_DENY:-$HOME/.cargo/bin/cargo-deny}
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/advisory-audit-real-tool.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

for tool in "$CARGO_BIN" "$GIT_BIN" "$MINISIGN_BIN" "$CARGO_DENY_BIN"; do
  if [ ! -x "$tool" ]; then
    echo "advisory-audit-real-tool.test.sh: required synthetic-test tool is unavailable" >&2
    exit 1
  fi
done

export SOLSTONE_TEST_GIT="$GIT_BIN"
export SOLSTONE_TEST_MINISIGN="$MINISIGN_BIN"
export SOLSTONE_TEST_CARGO_DENY="$CARGO_DENY_BIN"
export SOLSTONE_TEST_GIT_TRACE_SINK="$TMP_ROOT/git-trace"
export GIT_TRACE="$SOLSTONE_TEST_GIT_TRACE_SINK"

POISON="$TMP_ROOT/poison-network"
cat >"$POISON" <<'EOF'
#!/usr/bin/env sh
set -eu
printf '%s\n' invoked >>"$SOLSTONE_TEST_NETWORK_WITNESS"
exit 97
EOF
chmod +x "$POISON"
export SOLSTONE_TEST_NETWORK_WITNESS="$TMP_ROOT/network-witness"
export GIT_DIR="$TMP_ROOT/ambient-git-dir"
export GIT_WORK_TREE="$TMP_ROOT/ambient-work-tree"
export GIT_CONFIG_GLOBAL="$TMP_ROOT/ambient-gitconfig"
export GIT_SSH_COMMAND="$POISON"
export GIT_PROXY_COMMAND="$POISON"

cd "$REPO_ROOT"
"$CARGO_BIN" test --locked -p xtask --test advisory_audit -- \
  --ignored --exact real_tool_derived_name_matches_cargo_deny

if [ -e "$SOLSTONE_TEST_NETWORK_WITNESS" ]; then
  echo "advisory-audit-real-tool.test.sh: poisoned network helper was invoked" >&2
  exit 1
fi
if [ -s "$SOLSTONE_TEST_GIT_TRACE_SINK" ]; then
  echo "advisory-audit-real-tool.test.sh: removed Git trace sink received child output" >&2
  exit 1
fi
