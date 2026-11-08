# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Appends one nested Microsoft Artifact Signing signature to the signed setup.
# The primary signature stays the approved release certificate; this adds a
# second, timestamped signature after it. The release finalizer runs this only
# when packaging/signing-policy.json names a nested signer, and verifies the
# result with SignTool afterwards.
#
# Inputs:
#   -Setup                           the already-signed setup executable
#   SOLSTONE_NESTED_SIGNING_TOKEN    a short-lived access token for the signing
#                                    profile, supplied per release; never stored
#
# The signer is jsign on a portable Java runtime at a fixed path, each pinned
# by SHA-256 so an unexpected signer never touches a release.

param([Parameter(Mandatory = $true)][string]$Setup)

$ErrorActionPreference = "Stop"

$SignerRoot = "C:\Tools\jsign-7.6"
$Java = Join-Path $SignerRoot "jre\bin\java.exe"
$Jsign = Join-Path $SignerRoot "jsign-7.6.jar"
$Pins = [ordered]@{
    $Java  = "f8dc082a7dedbfd2924c3562405a5a1fd12efd2641808c92e2de819c76a58b7e"
    $Jsign = "6cf58aa080e7ff82656f320cff8d4a3bff30408df8766806dcba3740dd839fa5"
}

foreach ($path in $Pins.Keys) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "nested signing: $path is not installed; provision the pinned signer on the build box and retry."
    }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    if ($actual -ne $Pins[$path]) {
        throw "nested signing: $path differs from its pinned SHA-256; restore the pinned signer and retry."
    }
}
if ([string]::IsNullOrEmpty($env:SOLSTONE_NESTED_SIGNING_TOKEN)) {
    throw "nested signing: SOLSTONE_NESTED_SIGNING_TOKEN is not set; supply a fresh signing-profile token for this release and retry."
}
if (-not (Test-Path -LiteralPath $Setup -PathType Leaf)) {
    throw "nested signing: setup executable not found."
}

$before = (Get-FileHash -Algorithm SHA256 -LiteralPath $Setup).Hash
& $Java -jar $Jsign `
    --storetype TRUSTEDSIGNING `
    --keystore wcus.codesigning.azure.net `
    --storepass env:SOLSTONE_NESTED_SIGNING_TOKEN `
    --alias solpbc-signing/solpbc-public `
    --tsaurl http://timestamp.acs.microsoft.com/ `
    --tsmode RFC3161 `
    --alg SHA-256 `
    $Setup
if ($LASTEXITCODE -ne 0) {
    throw "nested signing: jsign exited $LASTEXITCODE; the setup was not given its nested signature."
}
$after = (Get-FileHash -Algorithm SHA256 -LiteralPath $Setup).Hash
if ($after -eq $before) {
    throw "nested signing: the setup is unchanged after signing."
}
