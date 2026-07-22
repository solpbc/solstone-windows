# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

function Assert-NativeProofHealthVersion {
    param(
        [Parameter(Mandatory = $true)]
        [AllowNull()]
        [AllowEmptyString()]
        [string]$Body,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedVersion
    )

    if ([string]::IsNullOrWhiteSpace($Body)) {
        throw "native-proof launched app /healthz returned an empty body; restore the canonical health response and retry"
    }

    try {
        $Health = $Body | ConvertFrom-Json
    } catch {
        throw "native-proof launched app /healthz returned malformed JSON; restore the canonical health response and retry"
    }

    $VersionProperty = if ($null -eq $Health) { $null } else { $Health.PSObject.Properties["version"] }
    if ($null -eq $VersionProperty -or $null -eq $VersionProperty.Value) {
        throw "native-proof launched app /healthz omitted version; restore the canonical health response and retry"
    }
    if ([string]$VersionProperty.Value -ne $ExpectedVersion) {
        throw "native-proof launched app version does not match ExpectedVersion; rebuild and reinstall the source-bound candidate"
    }
}
