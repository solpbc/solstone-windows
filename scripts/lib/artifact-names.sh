# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Single source of the versioned installer name for the shell publish scripts so
# publish-r2 and publish-winget never drift.

setup_exe_name() {
  printf 'solstone-setup-%s.exe' "$1"
}

winget_installer_url() {
  printf 'https://github.com/%s/releases/download/v%s/%s' "$1" "$2" "$(setup_exe_name "$2")"
}

resolve_release_version() {
  ls "$1"/Solstone-*-full.nupkg 2>/dev/null | sed -E 's#.*/Solstone-(.+)-full\.nupkg#\1#; s/-win$//' | sort -V | tail -1
}
