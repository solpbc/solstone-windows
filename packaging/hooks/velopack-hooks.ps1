# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Velopack lifecycle hooks - DOCUMENTATION ONLY.
#
# Velopack invokes the observer EXE directly with the lifecycle arguments; it does
# NOT call this script. The `VelopackApp::build()...run()` call at the top of
# src-tauri/src/main.rs handles them in-process, which is what makes the binary
# Velopack-aware (a non-aware exe was the spike's non-zero-hook gotcha).
#
# The real velopack 1.2.0 installer hooks `run()` acts on and then exits:
#   --veloapp-install      first install on this machine
#   --veloapp-updated      an update was applied
#   --veloapp-obsolete     this version is being superseded
#   --veloapp-uninstall    the app is being removed
#
# First launch after install is signaled by the VELOPACK_FIRSTRUN env var, which
# fires `on_first_run` in main.rs (registering the per-user autostart login item)
# and then CONTINUES to the tray - it is not a separate process exit.
#
# This file remains as living documentation of that contract and as a no-op log if
# ever invoked manually. The binary (src-tauri/src/main.rs) owns the dispatch;
# autostart registration lives there, never here.

param([string]$Phase)

$ErrorActionPreference = "Stop"

switch ($Phase) {
    "install"   { Write-Host "velopack: install (handled in-process by the EXE)" }
    "updated"   { Write-Host "velopack: updated (handled in-process by the EXE)" }
    "obsolete"  { Write-Host "velopack: obsolete (handled in-process by the EXE)" }
    "uninstall" { Write-Host "velopack: uninstall (handled in-process by the EXE)" }
    "firstrun"  { Write-Host "velopack: firstrun - autostart registers in the EXE (VELOPACK_FIRSTRUN)" }
    default     { Write-Host "velopack: documentation-only hook; no phase action" }
}
exit 0
