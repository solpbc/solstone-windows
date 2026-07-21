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
    $Contract = Get-Content -LiteralPath $ContractPath -Raw -Encoding UTF8 | ConvertFrom-Json
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
Assert-ExactNames $Contract.groups.unsigned $UnsignedNames "groups.unsigned"
Assert-ExactNames $Contract.groups.signedAdditional $SignedNames "groups.signedAdditional"
Assert-ExactNames $Contract.tools.PSObject.Properties.Name $AllNames "tools"

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
Observe-VersionTool "npm" "npm" @("--version") $Contract.tools.npm.expected.version

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
    $installations = if ($vsProbe.status -eq 0) { @($vsProbe.text -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }) } else { @() }
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
            $clProbe = Invoke-Observed $clPath @("/Bv")
            $matches = if ($clProbe.status -eq 0) { @([regex]::Matches($clProbe.text, "Compiler Version\s+([0-9.]+)\s+for\s+x64")) } else { @() }
            $compilerVersion = if ($matches.Count -eq 1) { $matches[0].Groups[1].Value } else { $null }
            if ($compilerVersion -ne $msvc.expected.compilerVersion) {
                Add-Mismatch "msvc-cl.compilerVersion" $msvc.expected.compilerVersion $compilerVersion $msvc.repair
            } else {
                $Selections["msvc-cl"] = [ordered]@{
                    path = [IO.Path]::GetFullPath($clPath)
                    compilerVersion = $compilerVersion
                    toolsetVersion = $msvc.expected.toolsetVersion
                    host = $msvc.expected.host
                    target = $msvc.expected.target
                    vcvarsallPath = [IO.Path]::GetFullPath($vcvarsall)
                    installationPath = $installation
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

$mode = if ($SignEnabled) { "signed" } else { "unsigned" }
$record = [ordered]@{
    schema = "solstone.release-tool-selection.v1"
    mode = $mode
    tools = $Selections
}
[Console]::Out.WriteLine(($record | ConvertTo-Json -Depth 8 -Compress))
