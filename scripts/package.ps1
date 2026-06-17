# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Velopack packaging: build the binary + webview, then `vpk pack` against
# packaging/ into Releases/ (full + delta nupkg + Setup.exe + feed JSON).
#
# Per-user install to %LocalAppData%, no UAC. Unsigned now: the $SignTemplate
# seam below is intentionally empty. When the signing cert lands, populate it
# with the Velopack `--signTemplate` form and add the credential pre-check; sign
# release artifacts only. No code restructure is needed to turn signing on.

$ErrorActionPreference = "Stop"

# Empty signing seam. When the cert is provisioned this becomes e.g.
#   $SignTemplate = '--signTemplate "smctl sign --fingerprint <fp> --input {{file}}"'
$SignTemplate = ""

# Windows-only step. On the build box this calls `vpk pack ... $SignTemplate`.
Write-Host "package.ps1: not yet implemented (Velopack pack runs on the Windows build box)."
Write-Host "  signing seam is empty (unsigned path); see packaging/signing/ for the future template."
exit 0
