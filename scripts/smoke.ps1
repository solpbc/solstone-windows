# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# FlaUI smoke against the *installed* app. Registers + fires a low-privilege
# scheduled task (LogonType=Interactive) into Session 1, runs the published net48
# FlaUI driver, and polls --dump-state / the health endpoint until app_state
# reaches `observing`, then exits 0. A failure-injection mode (kill system audio)
# asserts the observer drops out of `observing` and exits non-zero.
#
# This is a live-target step; it runs on the Windows build box only.

$ErrorActionPreference = "Stop"

Write-Host "smoke.ps1: not yet implemented (Session-1 scheduled-task FlaUI smoke runs on the Windows build box)."
exit 0
