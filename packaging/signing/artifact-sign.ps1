# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# The one Microsoft Artifact Signing entry point for a release. The release
# finalizer runs it in three modes:
#
#   replace   Velopack's --signTemplate, once per file Velopack signs. Every
#             internal executable ships with this signature alone.
#   remove    strips the Artifact signature Velopack put on the setup, so the
#             setup can take DigiCert as its primary signature.
#   append    adds one nested Artifact signature after the setup's primary
#             DigiCert signature.
#
# The finalizer verifies every result with SignTool afterwards.
#
# Inputs:
#   -Mode                            replace, append or remove
#   -Path                            the executable to change in place
#   SOLSTONE_NESTED_SIGNING_TOKEN    a short-lived access token for the signing
#                                    profile, supplied per release; never stored.
#                                    replace and append need it; remove does not.
#
# The signer is jsign on a portable Java runtime at a fixed path, each pinned
# by SHA-256 so an unexpected signer never touches a release.

param(
    [Parameter(Mandatory = $true)][ValidateSet("replace", "append", "remove")][string]$Mode,
    [Parameter(Mandatory = $true)][string]$Path
)

$ErrorActionPreference = "Stop"

$SignerRoot = "C:\Tools\jsign-7.6"
$Java = Join-Path $SignerRoot "jre\bin\java.exe"
$Jsign = Join-Path $SignerRoot "jsign-7.6.jar"
$Pins = [ordered]@{
    $Java  = "f8dc082a7dedbfd2924c3562405a5a1fd12efd2641808c92e2de819c76a58b7e"
    $Jsign = "6cf58aa080e7ff82656f320cff8d4a3bff30408df8766806dcba3740dd839fa5"
}

foreach ($pinned in $Pins.Keys) {
    if (-not (Test-Path -LiteralPath $pinned -PathType Leaf)) {
        throw "artifact signing: $pinned is not installed; provision the pinned signer on the build box and retry."
    }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $pinned).Hash.ToLowerInvariant()
    if ($actual -ne $Pins[$pinned]) {
        throw "artifact signing: $pinned differs from its pinned SHA-256; restore the pinned signer and retry."
    }
}
if ($Mode -ne "remove" -and [string]::IsNullOrEmpty($env:SOLSTONE_NESTED_SIGNING_TOKEN)) {
    throw "artifact signing: SOLSTONE_NESTED_SIGNING_TOKEN is not set; supply a fresh signing-profile token for this release and retry."
}
if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "artifact signing: $Path not found; $Mode needs the executable in place."
}

$before = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
if ($Mode -eq "remove") {
    # jsign 7.6: `remove` strips every signature and takes no options.
    & $Java -jar $Jsign remove $Path
} else {
    $signArgs = @(
        "--storetype", "TRUSTEDSIGNING",
        "--keystore", "wcus.codesigning.azure.net",
        "--storepass", "env:SOLSTONE_NESTED_SIGNING_TOKEN",
        "--alias", "solpbc-signing/solpbc-public",
        "--tsaurl", "http://timestamp.acs.microsoft.com/",
        "--tsmode", "RFC3161",
        "--alg", "SHA-256"
    )
    if ($Mode -eq "replace") {
        $signArgs += "--replace"
    }
    & $Java -jar $Jsign @signArgs $Path
}
if ($LASTEXITCODE -ne 0) {
    $outcome = switch ($Mode) {
        "replace" { "$Path was not given its Artifact signature" }
        "append" { "$Path was not given its nested signature" }
        "remove" { "$Path still carries its signature" }
    }
    throw "artifact signing: jsign exited $LASTEXITCODE; $outcome."
}
$after = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
if ($after -eq $before) {
    throw "artifact signing: $Path is unchanged after $Mode; check the jsign output above and rerun the release."
}
