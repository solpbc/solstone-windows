#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

untracked=$(git ls-files --others --exclude-standard)
if [ -n "$untracked" ]; then
  echo "ERROR: refusing Windows tree sync: untracked non-ignored files would be omitted from the git bundle:" >&2
  printf '%s\n' "$untracked" | sed 's/^/  /' >&2
  echo "Run 'git add <path>' to include them, or ignore/remove them before retrying." >&2
  exit 1
fi
