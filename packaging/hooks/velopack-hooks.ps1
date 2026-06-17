# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Velopack lifecycle hooks. The observer must be Velopack-aware so these exit 0
# (a non-aware exe was the spike's non-zero-hook gotcha). The binary dispatches
# the --veloapp-* arguments before booting the tray runtime.
#
#   --veloapp-install    first install on this machine
#   --veloapp-update     an update was applied
#   --veloapp-obsolete   this version is being superseded
#   --veloapp-firstrun   first launch after install — register the per-user
#                        autostart login item here
#
# Skeleton: the real handlers (autostart registration, migration) land with the
# packaging work. This file documents the contract; the binary owns the dispatch.

$ErrorActionPreference = "Stop"
param([string]$Phase)

switch ($Phase) {
    "install"  { Write-Host "velopack: install (not yet implemented)" }
    "update"   { Write-Host "velopack: update (not yet implemented)" }
    "obsolete" { Write-Host "velopack: obsolete (not yet implemented)" }
    "firstrun" { Write-Host "velopack: firstrun — register autostart (not yet implemented)" }
    default    { Write-Host "velopack: unknown phase '$Phase'" }
}
exit 0
