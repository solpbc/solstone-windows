# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

param([switch]$Sign)

$ErrorActionPreference = "Stop"

function Fail-Contract([string]$Message) {
    [Console]::Error.WriteLine("ERROR: release toolchain contract invalid: $Message")
    exit 1
}

function Require-String($Value, [string]$Path) {
    if ($null -eq $Value -or -not ($Value -is [string]) -or [string]::IsNullOrWhiteSpace($Value)) {
        Fail-Contract "$Path must be a non-empty string."
    }
}

function Assert-NoNull($Value, [string]$Path) {
    if ($null -eq $Value) {
        Fail-Contract "$Path must not be null."
    }
    if ($Value -is [string] -or $Value -is [ValueType]) { return }
    if ($Value -is [System.Collections.IEnumerable] -and -not ($Value -is [pscustomobject])) {
        $index = 0
        foreach ($item in $Value) {
            Assert-NoNull $item "$Path[$index]"
            $index++
        }
        return
    }
    foreach ($property in $Value.PSObject.Properties) {
        Assert-NoNull $property.Value "$Path.$($property.Name)"
    }
}

function Assert-ExactNames($Actual, [string[]]$Expected, [string]$Path) {
    $actualNames = @($Actual)
    $duplicates = @($actualNames | Group-Object | Where-Object { $_.Count -ne 1 })
    if ($duplicates.Count -ne 0) {
        Fail-Contract "$Path contains duplicate entries."
    }
    if (($actualNames -join "|") -ne ($Expected -join "|")) {
        Fail-Contract "$Path must be exactly: $($Expected -join ', ')."
    }
}

$Root = Split-Path -Parent $PSScriptRoot
$ContractPath = if ([string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_CONTRACT)) {
    Join-Path $PSScriptRoot "release-toolchain.json"
} else {
    $env:SOLSTONE_RELEASE_TOOLS_CONTRACT
}

if (-not (Test-Path -LiteralPath $ContractPath -PathType Leaf)) {
    Fail-Contract "contract file unavailable at $ContractPath."
}
try {
    $ContractJson = Get-Content -LiteralPath $ContractPath -Raw -Encoding UTF8
    if ([regex]::Matches($ContractJson, '"schema"\s*:').Count -ne 1) {
        Fail-Contract "schema must appear exactly once."
    }
    $Contract = $ContractJson | ConvertFrom-Json
} catch {
    Fail-Contract "invalid JSON at $ContractPath."
}

Assert-NoNull $Contract "contract"
if ($Contract.schema -ne "solstone.release-toolchain.v1") {
    Fail-Contract "schema must be solstone.release-toolchain.v1."
}

$UnsignedNames = @("rustc", "cargo", "cargo-deny", "dotnet", "vpk", "node", "npm", "msvc-cl", "windows-sdk", "powershell")
$SignedNames = @("smctl", "signtool")
$AllNames = @($UnsignedNames + $SignedNames)
Assert-ExactNames $Contract.PSObject.Properties.Name @("schema", "groups", "selection", "tools") "contract"
Assert-ExactNames $Contract.groups.unsigned $UnsignedNames "groups.unsigned"
Assert-ExactNames $Contract.groups.signedAdditional $SignedNames "groups.signedAdditional"
Assert-ExactNames $Contract.tools.PSObject.Properties.Name $AllNames "tools"

$ActionNames = @(
    "npm_ci",
    "npm_build",
    "cargo_release_build",
    "signing_auth_preflight",
    "vpk_pack",
    "smctl_sign",
    "signtool_verify",
    "cargo_deny_advisories",
    "native_smoke"
)
$MsvcEnvironmentNames = @(
    "PATH",
    "INCLUDE",
    "LIB",
    "LIBPATH",
    "VCINSTALLDIR",
    "VCToolsInstallDir",
    "VCToolsVersion",
    "UniversalCRTSdkDir",
    "UCRTVersion",
    "WindowsSdkDir",
    "WindowsSdkBinPath",
    "WindowsLibPath",
    "WindowsSDKVersion"
)
Assert-ExactNames $Contract.selection.PSObject.Properties.Name @("actions", "msvcEnvironment") "selection"
Assert-ExactNames $Contract.selection.actions.PSObject.Properties.Name $ActionNames "selection.actions"
Assert-ExactNames $Contract.selection.msvcEnvironment $MsvcEnvironmentNames "selection.msvcEnvironment"
foreach ($name in $ActionNames) {
    $action = $Contract.selection.actions.PSObject.Properties[$name].Value
    Assert-ExactNames $action.PSObject.Properties.Name @("tool", "argv") "selection.actions.$name"
    Require-String $action.tool "selection.actions.$name.tool"
    if (@($action.argv).Count -eq 0) { Fail-Contract "selection.actions.$name.argv must not be empty." }
    foreach ($argument in @($action.argv)) { Require-String $argument "selection.actions.$name.argv" }
}

foreach ($name in $AllNames) {
    $tool = $Contract.tools.PSObject.Properties[$name].Value
    Require-String $tool.observation "tools.$name.observation"
    Require-String $tool.repair "tools.$name.repair"
}
Require-String $Contract.tools.rustc.expected.release "tools.rustc.expected.release"
Require-String $Contract.tools.rustc.expected.host "tools.rustc.expected.host"
Require-String $Contract.tools.cargo.expected.version "tools.cargo.expected.version"
Require-String $Contract.tools.'cargo-deny'.expected.version "tools.cargo-deny.expected.version"
Require-String $Contract.tools.dotnet.expected.version "tools.dotnet.expected.version"
Require-String $Contract.tools.vpk.expected.packageId "tools.vpk.expected.packageId"
Require-String $Contract.tools.vpk.expected.version "tools.vpk.expected.version"
Require-String $Contract.tools.vpk.expected.command "tools.vpk.expected.command"
Require-String $Contract.tools.node.expected.version "tools.node.expected.version"
Require-String $Contract.tools.npm.expected.version "tools.npm.expected.version"
Require-String $Contract.tools.'msvc-cl'.expected.compilerVersion "tools.msvc-cl.expected.compilerVersion"
Require-String $Contract.tools.'msvc-cl'.expected.toolsetVersion "tools.msvc-cl.expected.toolsetVersion"
Require-String $Contract.tools.'msvc-cl'.expected.host "tools.msvc-cl.expected.host"
Require-String $Contract.tools.'msvc-cl'.expected.target "tools.msvc-cl.expected.target"
Require-String $Contract.tools.'windows-sdk'.expected.version "tools.windows-sdk.expected.version"
Require-String $Contract.tools.powershell.expected.majorMinor "tools.powershell.expected.majorMinor"
Require-String $Contract.tools.smctl.expected.version "tools.smctl.expected.version"
Require-String $Contract.tools.signtool.expected.path "tools.signtool.expected.path"
Require-String $Contract.tools.signtool.expected.productVersion "tools.signtool.expected.productVersion"
Require-String $Contract.tools.signtool.expected.originalFilename "tools.signtool.expected.originalFilename"

$SignEnabled = $Sign -or -not [string]::IsNullOrWhiteSpace($env:SOLSTONE_SIGN)
$Errors = New-Object System.Collections.Generic.List[string]
$Selections = [ordered]@{}
$MsvcEnvironment = $null

function Add-Mismatch([string]$Field, [string]$Expected, [string]$Actual, [string]$Repair) {
    if ([string]::IsNullOrWhiteSpace($Actual)) { $Actual = "unavailable" }
    $script:Errors.Add("ERROR: release tool mismatch: $Field expected $Expected, actual $Actual. $Repair")
}

function Resolve-NamedTool([string]$ToolName, [string]$CommandName, [string]$Repair) {
    $matches = @()
    if (-not [string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN)) {
        foreach ($extension in @(".exe", ".cmd")) {
            $candidate = Join-Path $env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN "$CommandName$extension"
            if (Test-Path -LiteralPath $candidate -PathType Leaf) { $matches += $candidate }
        }
    } else {
        $matches = @(Get-Command $CommandName -CommandType Application -ErrorAction SilentlyContinue | ForEach-Object { $_.Source })
    }
    $matches = @($matches | ForEach-Object { [IO.Path]::GetFullPath($_) } | Select-Object -Unique)
    if ($matches.Count -eq 0) {
        Add-Mismatch "$ToolName.path" "one executable" "unavailable" $Repair
        return $null
    }
    if ($matches.Count -ne 1) {
        Add-Mismatch "$ToolName.path" "one executable" "multiple: $($matches -join ', ')" $Repair
        return $null
    }
    return $matches[0]
}

function Invoke-Observed([string]$Path, [string[]]$Arguments) {
    try {
        $lines = @(& $Path @Arguments 2>&1)
        $status = $LASTEXITCODE
        return [pscustomobject]@{ status = $status; text = (($lines | Out-String).Trim()) }
    } catch {
        return [pscustomobject]@{ status = -1; text = $_.Exception.Message }
    }
}

function First-Version([string]$Text) {
    $match = [regex]::Match($Text, "[0-9]+(?:\.[0-9]+)+")
    if ($match.Success) { return $match.Value }
    return $null
}

function Find-NpmCompanions([string[]]$SearchDirectories) {
    $companions = @()
    foreach ($rawDirectory in $SearchDirectories) {
        if ([string]::IsNullOrWhiteSpace($rawDirectory)) { continue }
        try {
            $directory = [IO.Path]::GetFullPath($rawDirectory.Trim().Trim([char]34))
        } catch {
            continue
        }
        foreach ($name in @("npm.cmd", "npm")) {
            try {
                $candidate = Join-Path $directory $name
                if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { continue }
                $fullPath = [IO.Path]::GetFullPath($candidate)
                if (@($companions | Where-Object { $_.path -eq $fullPath }).Count -eq 0) {
                    $companions += [pscustomobject]@{ name = $name; path = $fullPath }
                }
            } catch {
                continue
            }
        }
    }
    return @($companions)
}

function Resolve-NpmTool {
    $entry = $Contract.tools.npm
    $expectedPathDescription = "one reachable npm.cmd co-located with selected node"
    if (-not $Selections.Contains("node")) {
        Add-Mismatch "npm.path" $expectedPathDescription "selected node unavailable" $entry.repair
        return
    }

    $nodeDirectory = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Selections["node"].path))
    $searchPath = if ([string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN)) {
        $env:PATH
    } elseif ([string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_FAKE_NPM_PATH)) {
        $env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN
    } else {
        $env:SOLSTONE_RELEASE_TOOLS_FAKE_NPM_PATH
    }
    $companions = @(Find-NpmCompanions @($searchPath -split ";"))
    $cmdPaths = @($companions | Where-Object { $_.name -eq "npm.cmd" } | ForEach-Object { $_.path } | Sort-Object)
    if ($cmdPaths.Count -eq 0) {
        Add-Mismatch "npm.path" $expectedPathDescription "unavailable" $entry.repair
        return
    }
    if ($cmdPaths.Count -ne 1) {
        Add-Mismatch "npm.path" $expectedPathDescription "multiple: $($cmdPaths -join ', ')" $entry.repair
        return
    }

    $npmPath = $cmdPaths[0]
    $expectedPath = [IO.Path]::GetFullPath((Join-Path $nodeDirectory "npm.cmd"))
    if ($npmPath -ne $expectedPath) {
        Add-Mismatch "npm.path" $expectedPath $npmPath $entry.repair
        return
    }

    $probe = Invoke-Observed $npmPath @("--version")
    if ($probe.status -eq -1) {
        Add-Mismatch "npm.invocation" "exit 0" "launch failed" $entry.repair
        return
    }
    if ($probe.status -ne 0) {
        Add-Mismatch "npm.invocation" "exit 0" "exit $($probe.status)" $entry.repair
        return
    }
    $actual = First-Version $probe.text
    if ($actual -ne $entry.expected.version) {
        Add-Mismatch "npm.version" $entry.expected.version $actual $entry.repair
        return
    }
    $Selections["npm"] = [ordered]@{ path = $npmPath; version = $actual }
}

function Invoke-ActivatedMsvcProbe([string]$VcvarsallPath, [string]$VcvarsVersionArg, [string]$ClPath) {
    try {
        $command = 'call "' + $VcvarsallPath + '" x64 ' + $VcvarsVersionArg +
            ' 2>&1 & set "VCX=!ERRORLEVEL!"' +
            ' & echo __SOLSTONE_RELEASE_PROBE_V1_VCVARS_EXIT__=!VCX!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_HOST__=!VSCMD_ARG_HOST_ARCH!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_TARGET__=!VSCMD_ARG_TGT_ARCH!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_TOOLSET__=!VCToolsVersion!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_PATH__=!PATH!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_INCLUDE__=!INCLUDE!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_LIB__=!LIB!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_LIBPATH__=!LIBPATH!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_VCINSTALLDIR__=!VCINSTALLDIR!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_VCToolsInstallDir__=!VCToolsInstallDir!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_VCToolsVersion__=!VCToolsVersion!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_UniversalCRTSdkDir__=!UniversalCRTSdkDir!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_UCRTVersion__=!UCRTVersion!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_WindowsSdkDir__=!WindowsSdkDir!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_WindowsSdkBinPath__=!WindowsSdkBinPath!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_WindowsLibPath__=!WindowsLibPath!' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_ENV_WindowsSDKVersion__=!WindowsSDKVersion!' +
            '& if "!VCX!"=="0" (' +
            'echo __SOLSTONE_RELEASE_PROBE_V1_COMPILER_BEGIN__' +
            '& call "' + $ClPath + '" /Bv 2>&1' +
            ' & set "CLX=!ERRORLEVEL!"' +
            ' & echo __SOLSTONE_RELEASE_PROBE_V1_COMPILER_END__' +
            '& echo __SOLSTONE_RELEASE_PROBE_V1_COMPILER_EXIT__=!CLX!' +
            ') & exit /b 0'
        $info = New-Object Diagnostics.ProcessStartInfo
        $info.FileName = [IO.Path]::GetFullPath($env:ComSpec)
        $info.Arguments = "/d /v:on /c $command"
        $info.UseShellExecute = $false
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $info.CreateNoWindow = $true
        $process = [Diagnostics.Process]::Start($info)
        $stdout = $process.StandardOutput.ReadToEnd()
        $stderr = $process.StandardError.ReadToEnd()
        $process.WaitForExit()
        return [pscustomobject]@{ launched = $true; status = $process.ExitCode; stdout = $stdout; stderr = $stderr }
    } catch {
        return [pscustomobject]@{ launched = $false; status = -1; stdout = ""; stderr = $_.Exception.Message }
    }
}

function Read-Sentinel([string[]]$Lines, [string]$Prefix, [switch]$Exact) {
    $hits = @()
    for ($index = 0; $index -lt $Lines.Count; $index++) {
        $matched = if ($Exact) { $Lines[$index] -eq $Prefix } else { $Lines[$index].StartsWith($Prefix, [StringComparison]::Ordinal) }
        if ($matched) { $hits += [pscustomobject]@{ index = $index; value = $Lines[$index].Substring($Prefix.Length) } }
    }
    if ($hits.Count -eq 0) { return [pscustomobject]@{ valid = $false; actual = "unavailable" } }
    if ($hits.Count -ne 1) { return [pscustomobject]@{ valid = $false; actual = "multiple: $($hits.Count)" } }
    return [pscustomobject]@{ valid = $true; actual = $hits[0].value; index = $hits[0].index }
}

function Observe-VersionTool([string]$ToolName, [string]$CommandName, [string[]]$Arguments, [string]$Expected) {
    $entry = $Contract.tools.PSObject.Properties[$ToolName].Value
    $path = Resolve-NamedTool $ToolName $CommandName $entry.repair
    if ($null -eq $path) { return }
    $probe = Invoke-Observed $path $Arguments
    $actual = if ($probe.status -eq 0) { First-Version $probe.text } else { $null }
    if ($actual -ne $Expected) {
        Add-Mismatch "$ToolName.version" $Expected $actual $entry.repair
        return
    }
    $Selections[$ToolName] = [ordered]@{ path = $path; version = $actual }
}

$rust = $Contract.tools.rustc
$rustcPath = Resolve-NamedTool "rustc" "rustc" $rust.repair
if ($null -ne $rustcPath) {
    $probe = Invoke-Observed $rustcPath @("-Vv")
    $releaseMatches = if ($probe.status -eq 0) { @([regex]::Matches($probe.text, "(?m)^release:\s*(\S+)\s*$")) } else { @() }
    $hostMatches = if ($probe.status -eq 0) { @([regex]::Matches($probe.text, "(?m)^host:\s*(\S+)\s*$")) } else { @() }
    $release = if ($releaseMatches.Count -eq 1) { $releaseMatches[0].Groups[1].Value } else { $null }
    $hostName = if ($hostMatches.Count -eq 1) { $hostMatches[0].Groups[1].Value } else { $null }
    if ($release -ne $rust.expected.release) { Add-Mismatch "rustc.release" $rust.expected.release $release $rust.repair }
    if ($hostName -ne $rust.expected.host) { Add-Mismatch "rustc.host" $rust.expected.host $hostName $rust.repair }
    if ($release -eq $rust.expected.release -and $hostName -eq $rust.expected.host) {
        $Selections["rustc"] = [ordered]@{ path = $rustcPath; version = $release; host = $hostName }
    }
}

Observe-VersionTool "cargo" "cargo" @("--version") $Contract.tools.cargo.expected.version
Observe-VersionTool "cargo-deny" "cargo-deny" @("--version") $Contract.tools.'cargo-deny'.expected.version
Observe-VersionTool "dotnet" "dotnet" @("--version") $Contract.tools.dotnet.expected.version

$vpk = $Contract.tools.vpk
if ($Selections.Contains("dotnet")) {
    $probe = Invoke-Observed $Selections["dotnet"].path @("tool", "list", "-g")
    $rows = @()
    if ($probe.status -eq 0) {
        foreach ($line in ($probe.text -split "`r?`n")) {
            $columns = @($line.Trim() -split "\s+" | Where-Object { $_ })
            if ($columns.Count -ge 3 -and $columns[0] -eq $vpk.expected.packageId) {
                $rows += ,$columns
            }
        }
    }
    if ($rows.Count -ne 1) {
        Add-Mismatch "vpk.globalToolRow" "one $($vpk.expected.packageId) row" $(if ($rows.Count -eq 0) { "unavailable" } else { "$($rows.Count) rows" }) $vpk.repair
    } else {
        $row = $rows[0]
        if ($row[1] -ne $vpk.expected.version) { Add-Mismatch "vpk.version" $vpk.expected.version $row[1] $vpk.repair }
        if ($row[2] -ne $vpk.expected.command) { Add-Mismatch "vpk.command" $vpk.expected.command $row[2] $vpk.repair }
        $profile = if ([string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_FAKE_USERPROFILE)) { $env:USERPROFILE } else { $env:SOLSTONE_RELEASE_TOOLS_FAKE_USERPROFILE }
        $vpkPath = [IO.Path]::GetFullPath((Join-Path $profile ".dotnet\tools\vpk.exe"))
        if (-not (Test-Path -LiteralPath $vpkPath -PathType Leaf)) {
            Add-Mismatch "vpk.path" $vpkPath "unavailable" $vpk.repair
        } elseif ($row[1] -eq $vpk.expected.version -and $row[2] -eq $vpk.expected.command) {
            $Selections["vpk"] = [ordered]@{ path = $vpkPath; version = $row[1]; packageId = $row[0] }
        }
    }
} else {
    Add-Mismatch "vpk.globalToolRow" "one $($vpk.expected.packageId) row" "unavailable" $vpk.repair
}

Observe-VersionTool "node" "node" @("--version") $Contract.tools.node.expected.version
Resolve-NpmTool

$msvc = $Contract.tools.'msvc-cl'
if ($msvc.expected.host -ne "x64") { Add-Mismatch "msvc-cl.host" $msvc.expected.host "x64" $msvc.repair }
if ($msvc.expected.target -ne "x64") { Add-Mismatch "msvc-cl.target" $msvc.expected.target "x64" $msvc.repair }
$vswherePath = if ([string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN)) {
    Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
} else {
    Join-Path $env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN "vswhere.cmd"
}
if (-not (Test-Path -LiteralPath $vswherePath -PathType Leaf)) {
    Add-Mismatch "msvc-cl.vswhere" "exact executable" "unavailable" $msvc.repair
} else {
    $vsProbe = Invoke-Observed $vswherePath @("-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath")
    $installations = @(if ($vsProbe.status -eq 0) { @($vsProbe.text -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }) } else { @() })
    if ($installations.Count -ne 1) {
        Add-Mismatch "msvc-cl.installationPath" "one installation" $(if ($installations.Count -eq 0) { "unavailable" } else { "$($installations.Count) installations" }) $msvc.repair
    } else {
        $installation = [IO.Path]::GetFullPath($installations[0].Trim())
        $toolsetRoot = Join-Path $installation "VC\Tools\MSVC\$($msvc.expected.toolsetVersion)"
        $clName = if ([string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN)) { "cl.exe" } else { "cl.cmd" }
        $clPath = Join-Path $toolsetRoot "bin\Hostx64\x64\$clName"
        $vcvarsall = Join-Path $installation "VC\Auxiliary\Build\vcvarsall.bat"
        if (-not (Test-Path -LiteralPath $toolsetRoot -PathType Container)) {
            Add-Mismatch "msvc-cl.toolsetVersion" $msvc.expected.toolsetVersion "unavailable" $msvc.repair
        } elseif (-not (Test-Path -LiteralPath $clPath -PathType Leaf)) {
            Add-Mismatch "msvc-cl.path" $clPath "unavailable" $msvc.repair
        } elseif (-not (Test-Path -LiteralPath $vcvarsall -PathType Leaf)) {
            Add-Mismatch "msvc-cl.vcvarsallPath" $vcvarsall "unavailable" $msvc.repair
        } else {
            $unsafePath = $false
            if ($vcvarsall -match '[!%^]') {
                Add-Mismatch "msvc-cl.vcvarsallPath" "absolute path free of cmd metacharacters (^ % !)" $vcvarsall $msvc.repair
                $unsafePath = $true
            }
            if ($clPath -match '[!%^]') {
                Add-Mismatch "msvc-cl.path" "absolute path free of cmd metacharacters (^ % !)" $clPath $msvc.repair
                $unsafePath = $true
            }
            if (-not $unsafePath) {
                $vcvarsVersionArg = "-vcvars_ver=$($msvc.expected.toolsetVersion)"
                $probe = Invoke-ActivatedMsvcProbe $vcvarsall $vcvarsVersionArg $clPath
                $valid = $true
                $compilerVersion = $null
                if (-not $probe.launched) {
                    Add-Mismatch "msvc-cl.probeLaunch" "complete activated cmd.exe child evidence" "launch failed" $msvc.repair
                    $valid = $false
                } elseif ($probe.status -ne 0) {
                    Add-Mismatch "msvc-cl.probeLaunch" "complete activated cmd.exe child evidence" "child exit $($probe.status)" $msvc.repair
                    $valid = $false
                } else {
                    $lines = @($probe.stdout -split "`r?`n")
                    $exitPrefix = "__SOLSTONE_RELEASE_PROBE_V1_VCVARS_EXIT__="
                    $hostPrefix = "__SOLSTONE_RELEASE_PROBE_V1_HOST__="
                    $targetPrefix = "__SOLSTONE_RELEASE_PROBE_V1_TARGET__="
                    $toolsetPrefix = "__SOLSTONE_RELEASE_PROBE_V1_TOOLSET__="
                    $compilerExitPrefix = "__SOLSTONE_RELEASE_PROBE_V1_COMPILER_EXIT__="
                    $vcvarsExitEvidence = Read-Sentinel $lines $exitPrefix
                    $vcvarsExit = $null
                    if (-not $vcvarsExitEvidence.valid) {
                        Add-Mismatch "msvc-cl.activationExit" "0" $vcvarsExitEvidence.actual $msvc.repair
                        $valid = $false
                    } elseif ($vcvarsExitEvidence.actual -notmatch "^[0-9]+$") {
                        Add-Mismatch "msvc-cl.activationExit" "0" "malformed: $($vcvarsExitEvidence.actual)" $msvc.repair
                        $valid = $false
                    } else {
                        $vcvarsExit = [int]$vcvarsExitEvidence.actual
                        if ($vcvarsExit -ne 0) {
                            Add-Mismatch "msvc-cl.activationExit" "0" "$vcvarsExit" $msvc.repair
                            $valid = $false
                        }
                    }

                    if ($null -ne $vcvarsExit -and $vcvarsExit -eq 0) {
                        $sentinelIndexes = @($vcvarsExitEvidence.index)
                        foreach ($identity in @(
                            [pscustomobject]@{ field = "msvc-cl.activatedHost"; prefix = $hostPrefix; expected = $msvc.expected.host; unresolved = "!VSCMD_ARG_HOST_ARCH!" },
                            [pscustomobject]@{ field = "msvc-cl.activatedTarget"; prefix = $targetPrefix; expected = $msvc.expected.target; unresolved = "!VSCMD_ARG_TGT_ARCH!" },
                            [pscustomobject]@{ field = "msvc-cl.activatedToolsetVersion"; prefix = $toolsetPrefix; expected = $msvc.expected.toolsetVersion; unresolved = "!VCToolsVersion!" }
                        )) {
                            $evidence = Read-Sentinel $lines $identity.prefix
                            if ($evidence.valid) { $sentinelIndexes += $evidence.index }
                            $actual = if ($evidence.valid -and -not [string]::IsNullOrWhiteSpace($evidence.actual) -and $evidence.actual -ne $identity.unresolved) { $evidence.actual } elseif ($evidence.valid) { $null } else { $evidence.actual }
                            if ($actual -ne $identity.expected) {
                                Add-Mismatch $identity.field $identity.expected $actual $msvc.repair
                                $valid = $false
                            }
                        }

                        $environment = [ordered]@{}
                        foreach ($name in $MsvcEnvironmentNames) {
                            $prefix = "__SOLSTONE_RELEASE_PROBE_V1_ENV_${name}__="
                            $evidence = Read-Sentinel $lines $prefix
                            $actual = if ($evidence.valid -and -not [string]::IsNullOrWhiteSpace($evidence.actual) -and $evidence.actual -ne "!$name!") {
                                $evidence.actual
                            } elseif ($evidence.valid) {
                                $null
                            } else {
                                $evidence.actual
                            }
                            if ([string]::IsNullOrWhiteSpace($actual)) {
                                Add-Mismatch "msvc-cl.environment.$name" "one non-empty value from the activated child" $actual $msvc.repair
                                $valid = $false
                            } else {
                                $environment[$name] = $actual
                            }
                        }

                        $beginMarker = "__SOLSTONE_RELEASE_PROBE_V1_COMPILER_BEGIN__"
                        $endMarker = "__SOLSTONE_RELEASE_PROBE_V1_COMPILER_END__"
                        $beginEvidence = Read-Sentinel $lines $beginMarker -Exact
                        $endEvidence = Read-Sentinel $lines $endMarker -Exact
                        $compilerExitEvidence = Read-Sentinel $lines $compilerExitPrefix
                        foreach ($evidence in @($beginEvidence, $endEvidence, $compilerExitEvidence)) {
                            if ($evidence.valid) { $sentinelIndexes += $evidence.index }
                        }
                        $channelValid = $beginEvidence.valid -and $endEvidence.valid -and $endEvidence.index -gt $beginEvidence.index
                        if (-not $channelValid) {
                            $channelActual = if (-not $beginEvidence.valid) { "begin $($beginEvidence.actual)" } elseif (-not $endEvidence.valid) { "end $($endEvidence.actual)" } else { "out of order" }
                            Add-Mismatch "msvc-cl.compilerChannel" "one ordered sentinel-delimited compiler channel" $channelActual $msvc.repair
                            $valid = $false
                        } else {
                            $channelLines = if ($endEvidence.index -gt ($beginEvidence.index + 1)) {
                                @($lines[($beginEvidence.index + 1)..($endEvidence.index - 1)])
                            } else {
                                @()
                            }
                            $channelText = $channelLines -join "`n"
                            $compilerExit = $null
                            if (-not $compilerExitEvidence.valid) {
                                Add-Mismatch "msvc-cl.compilerExit" "2" $compilerExitEvidence.actual $msvc.repair
                                $valid = $false
                            } elseif ($compilerExitEvidence.actual -notmatch "^[0-9]+$") {
                                Add-Mismatch "msvc-cl.compilerExit" "2" "malformed: $($compilerExitEvidence.actual)" $msvc.repair
                                $valid = $false
                            } else {
                                $compilerExit = [int]$compilerExitEvidence.actual
                            }
                            if ($sentinelIndexes.Count -eq 7) {
                                $orderedIndexes = @($sentinelIndexes | Sort-Object)
                                if (($sentinelIndexes -join ",") -ne ($orderedIndexes -join ",")) {
                                    Add-Mismatch "msvc-cl.compilerChannel" "sentinels exactly once and in order" "out of order" $msvc.repair
                                    $valid = $false
                                }
                            }

                            $bannerLines = @($channelLines | Where-Object { $_ -match "Compiler Version" })
                            $launchFailed = $bannerLines.Count -eq 0 -and ($compilerExit -eq 9009 -or $compilerExit -eq 193)
                            if ($launchFailed) {
                                Add-Mismatch "msvc-cl.compilerLaunch" "exact pinned compiler launched" "exit $compilerExit without compiler banner" $msvc.repair
                                $valid = $false
                            } else {
                                if ($bannerLines.Count -eq 0) {
                                    Add-Mismatch "msvc-cl.compilerVersion" $msvc.expected.compilerVersion "unavailable" $msvc.repair
                                    $valid = $false
                                } elseif ($bannerLines.Count -ne 1) {
                                    Add-Mismatch "msvc-cl.compilerVersion" $msvc.expected.compilerVersion "multiple: $($bannerLines.Count) banners" $msvc.repair
                                    $valid = $false
                                } else {
                                    $bannerMatch = [regex]::Match($bannerLines[0], "Compiler Version\s+([0-9]+(?:\.[0-9]+)+)\s+for\s+x64\s*$")
                                    if (-not $bannerMatch.Success) {
                                        Add-Mismatch "msvc-cl.compilerVersion" $msvc.expected.compilerVersion "malformed" $msvc.repair
                                        $valid = $false
                                    } else {
                                        $compilerVersion = $bannerMatch.Groups[1].Value
                                        if ($compilerVersion -ne $msvc.expected.compilerVersion) {
                                            Add-Mismatch "msvc-cl.compilerVersion" $msvc.expected.compilerVersion $compilerVersion $msvc.repair
                                            $valid = $false
                                        }
                                    }
                                }
                                if ($channelText -notmatch "\bD8003\b") {
                                    Add-Mismatch "msvc-cl.compilerDiagnostic" "D8003" "unavailable" $msvc.repair
                                    $valid = $false
                                }
                                if ($null -ne $compilerExit -and $compilerExit -ne 2) {
                                    Add-Mismatch "msvc-cl.compilerExit" "2" "$compilerExit" $msvc.repair
                                    $valid = $false
                                }
                            }
                        }
                    }
                }

                if ($valid) {
                    $MsvcEnvironment = $environment
                    $Selections["msvc-cl"] = [ordered]@{
                        path = [IO.Path]::GetFullPath($clPath)
                        compilerVersion = $compilerVersion
                        toolsetVersion = $msvc.expected.toolsetVersion
                        host = $msvc.expected.host
                        target = $msvc.expected.target
                        vcvarsallPath = [IO.Path]::GetFullPath($vcvarsall)
                        vcvarsVersionArg = $vcvarsVersionArg
                        installationPath = $installation
                    }
                }
            }
        }
    }
}

$sdk = $Contract.tools.'windows-sdk'
$kitsRoot = if ([string]::IsNullOrWhiteSpace($env:SOLSTONE_RELEASE_TOOLS_FAKE_WINDOWS_KITS)) {
    Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10"
} else {
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_WINDOWS_KITS
}
$sdkPath = [IO.Path]::GetFullPath((Join-Path $kitsRoot "Lib\$($sdk.expected.version)"))
if (-not (Test-Path -LiteralPath $sdkPath -PathType Container)) {
    Add-Mismatch "windows-sdk.version" $sdk.expected.version "unavailable" $sdk.repair
} else {
    $Selections["windows-sdk"] = [ordered]@{ path = $sdkPath; version = $sdk.expected.version }
}

$ps = $Contract.tools.powershell
$psVersion = "$($PSVersionTable.PSVersion.Major).$($PSVersionTable.PSVersion.Minor)"
$psPath = (Get-Process -Id $PID).Path
if ($psVersion -ne $ps.expected.majorMinor) {
    Add-Mismatch "powershell.version" $ps.expected.majorMinor $psVersion $ps.repair
} else {
    $Selections["powershell"] = [ordered]@{ path = [IO.Path]::GetFullPath($psPath); version = $psVersion }
}

if ($SignEnabled) {
    Observe-VersionTool "smctl" "smctl" @("--version") $Contract.tools.smctl.expected.version

    $signtool = $Contract.tools.signtool
    $signtoolPath = [IO.Path]::GetFullPath($signtool.expected.path)
    if (-not (Test-Path -LiteralPath $signtoolPath -PathType Leaf)) {
        Add-Mismatch "signtool.path" $signtool.expected.path "unavailable" $signtool.repair
    } else {
        $metadata = [Diagnostics.FileVersionInfo]::GetVersionInfo($signtoolPath)
        if ($metadata.ProductVersion -ne $signtool.expected.productVersion) {
            Add-Mismatch "signtool.productVersion" $signtool.expected.productVersion $metadata.ProductVersion $signtool.repair
        }
        if ($metadata.OriginalFilename -ne $signtool.expected.originalFilename) {
            Add-Mismatch "signtool.originalFilename" $signtool.expected.originalFilename $metadata.OriginalFilename $signtool.repair
        }
        if ($metadata.ProductVersion -eq $signtool.expected.productVersion -and $metadata.OriginalFilename -eq $signtool.expected.originalFilename) {
            $Selections["signtool"] = [ordered]@{
                path = $signtoolPath
                version = $metadata.ProductVersion
                originalFilename = $metadata.OriginalFilename
            }
        }
    }
}

if ($Errors.Count -ne 0) {
    foreach ($message in $Errors) { [Console]::Error.WriteLine($message) }
    exit 1
}

if ($null -eq $MsvcEnvironment) {
    Fail-Contract "validated MSVC environment was not captured; rerun the pinned vcvars preflight."
}

function Selected-Action([string]$Name) {
    $template = $Contract.selection.actions.PSObject.Properties[$Name].Value
    $toolName = $template.tool
    if (-not $Selections.Contains($toolName)) {
        Fail-Contract "selection.actions.$Name references unavailable selected tool '$toolName'."
    }
    return [ordered]@{
        program = $Selections[$toolName].path
        argv = @($template.argv)
    }
}

$Actions = [ordered]@{
    npm_ci = (Selected-Action "npm_ci")
    npm_build = (Selected-Action "npm_build")
    cargo_release_build = (Selected-Action "cargo_release_build")
}
if ($SignEnabled) { $Actions["signing_auth_preflight"] = (Selected-Action "signing_auth_preflight") }
$Actions["vpk_pack"] = (Selected-Action "vpk_pack")
if ($SignEnabled) {
    $Actions["smctl_sign"] = (Selected-Action "smctl_sign")
    $Actions["signtool_verify"] = (Selected-Action "signtool_verify")
}
$Actions["cargo_deny_advisories"] = (Selected-Action "cargo_deny_advisories")
$Actions["native_smoke"] = (Selected-Action "native_smoke")

$mode = if ($SignEnabled) { "signed" } else { "unsigned" }
$record = [ordered]@{
    schema = "solstone.release-tool-selection.v1"
    mode = $mode
    tools = $Selections
    actions = $Actions
    msvc_environment = $MsvcEnvironment
}
[Console]::Out.WriteLine(($record | ConvertTo-Json -Depth 12 -Compress))
