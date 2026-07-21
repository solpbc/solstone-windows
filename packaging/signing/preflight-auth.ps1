# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Signing credential preflight. Runs before `vpk pack --signTemplate ...` so a
# misprovisioned signing environment fails fast with a clean, secret-free message
# instead of letting the signer fail opaquely mid-pack. (The KeyLocker KSP
# surfaces a missing credential or lost keypair access as the cryptic signtool
# error 0x8009002d / NTE_BAD_ALGID, which says nothing useful.) Release-only
# signing; see docs/release-runbook.md.
#
# This script NEVER prints a secret value. It checks names and end-to-end
# reachability only.
#
# Checks:
#   1. The non-secret signing env vars are present and non-empty:
#      SM_HOST, SM_CLIENT_CERT_FILE, SM_KEYPAIR_ALIAS.
#   2. SM_CLIENT_CERT_FILE points at a file that exists.
#   3. The selected `smctl` path is an existing file.
#   4. `smctl healthcheck` reports a signing-ready connection. This is the
#      authoritative gate: it authenticates end-to-end, so it validates the API
#      key and client-cert password wherever they live (process environment or the
#      OS credential store) without this script ever reading them.

param([Parameter(Mandatory=$true)][string]$SmctlPath)

$ErrorActionPreference = "Stop"

$required = @("SM_HOST", "SM_CLIENT_CERT_FILE", "SM_KEYPAIR_ALIAS")
$missing = $required | Where-Object {
    [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($_))
}
if ($missing) {
    throw "signing preflight: missing/empty signing env var(s): $($missing -join ', '). " +
          "Provision the signing environment on the build box (credentials live in the operator vault; never commit them)."
}

$certFile = $env:SM_CLIENT_CERT_FILE
if (-not (Test-Path -LiteralPath $certFile)) {
    throw "signing preflight: the client-auth certificate at SM_CLIENT_CERT_FILE does not exist. " +
          "Deploy the KeyLocker client-auth certificate on the build box."
}

if (-not (Test-Path -LiteralPath $SmctlPath -PathType Leaf)) {
    throw "signing preflight: selected smctl executable not found at $SmctlPath. " +
          "Run 'make preflight-release-tools' and repair the pinned release toolchain."
}

Write-Host "preflight-auth: SM_HOST / SM_CLIENT_CERT_FILE / SM_KEYPAIR_ALIAS present, cert file present, selected smctl present."

# End-to-end credential gate. smctl self-masks the API key and password in its
# output, so this is safe to echo.
$health = (& $SmctlPath healthcheck 2>&1 | Out-String)
Write-Host $health
$connected = $health -match "(?im)^\s*Status\s*:\s*Connected" `
             -and $health -match "(?im)^\s*Can sign\s*:\s*Yes"
if ($LASTEXITCODE -ne 0 -or -not $connected) {
    throw "signing preflight: 'smctl healthcheck' did not report a signing-ready connection " +
          "(want Status: Connected + Can sign: Yes; see output above). " +
          "Fix the signing credentials / keypair access before packaging."
}

Write-Host "preflight-auth: smctl healthcheck OK - signing-ready."
