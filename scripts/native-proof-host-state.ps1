# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Native proof host-state guard. The native proof runs the candidate's real
# Setup.exe (--silent --installto <proof root>), and Velopack's setup registers
# that copy with the signed-in user whatever LOCALAPPDATA says: it rewrites the
# HKCU Uninstall\Solstone entry and the Desktop and Start Menu shortcuts so they
# point into the proof root. The proof runs on a shared box, so xtask brackets
# setup and smoke with this script and puts all of it back, pass or fail.
#
#   -Mode Capture -StatePath <file>
#       Record the Uninstall entry, the HKCU Run login item, and every top-level
#       shortcut in the Desktop, Start Menu Programs and Startup folders.
#       Prints HOST_STATE_CAPTURED.
#   -Mode Restore -StatePath <file> -ProofRoot <dir>
#       Put back the recorded Uninstall entry and Run value, and restore (or
#       remove, if it did not exist before) every shortcut that now targets the
#       proof root. Then check the result: the entries must equal what was
#       recorded, and no shortcut, Uninstall entry or Run value may still name
#       the proof root. Prints one before/after line per location and
#       HOST_STATE_RESTORED.
#
# Every line printed is also written to <StatePath>.log, because xtask keeps its
# children's output private.
#
# The locations are parameters so scripts\lib\native-proof-host-state.test.ps1
# can run the round trip against a scratch registry key and scratch folders.
#
# ASCII-only by policy: Windows PowerShell 5.1 reads non-BOM .ps1 in the system
# codepage, so smart punctuation can corrupt and break parsing.

param(
    [Parameter(Mandatory = $true)][ValidateSet("Capture", "Restore")][string]$Mode,
    [Parameter(Mandatory = $true)][string]$StatePath,
    [string]$ProofRoot,
    [string]$UninstallRootSubKey = "Software\Microsoft\Windows\CurrentVersion\Uninstall",
    [string]$UninstallName = "Solstone",
    [string]$RunSubKey = "Software\Microsoft\Windows\CurrentVersion\Run",
    [string]$RunValueName = "Solstone",
    [string[]]$ShortcutFolders
)

$ErrorActionPreference = "Stop"
$Utf8 = New-Object System.Text.UTF8Encoding($false)
$LogPath = "$StatePath.log"

function Write-Report([string]$line) {
    Write-Host $line
    [IO.File]::AppendAllText($LogPath, $line + "`r`n", $Utf8)
}

function Get-Sha256Hex([byte[]]$bytes) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace("-", "").ToLowerInvariant() }
    finally { $sha.Dispose() }
}

function ConvertTo-ComparablePath([string]$path) {
    if ([string]::IsNullOrWhiteSpace($path)) { return "" }
    if ($path.StartsWith("\\?\UNC\")) { $path = "\\" + $path.Substring(8) }
    elseif ($path.StartsWith("\\?\")) { $path = $path.Substring(4) }
    return [IO.Path]::GetFullPath($path).TrimEnd("\")
}

function Test-UnderRoot([string]$path, [string]$root) {
    if ([string]::IsNullOrWhiteSpace($path)) { return $false }
    try { $candidate = ConvertTo-ComparablePath $path } catch { return $false }
    return $candidate.Equals($root, [StringComparison]::OrdinalIgnoreCase) -or
        $candidate.StartsWith($root + "\", [StringComparison]::OrdinalIgnoreCase)
}

# --- registry -------------------------------------------------------------

function ConvertFrom-RegistryData($kind, $data) {
    switch ($kind) {
        "String" { return [string]$data }
        "ExpandString" { return [string]$data }
        "DWord" { return ([int32]$data).ToString([Globalization.CultureInfo]::InvariantCulture) }
        "QWord" { return ([int64]$data).ToString([Globalization.CultureInfo]::InvariantCulture) }
        "Binary" { return [Convert]::ToBase64String([byte[]]$data) }
        "MultiString" { return [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes((@($data) -join "`0"))) }
        default { throw "registry value kind $kind is not supported by the native-proof host-state guard" }
    }
}

function ConvertTo-RegistryData([string]$kind, [string]$text) {
    switch ($kind) {
        "String" { return $text }
        "ExpandString" { return $text }
        "DWord" { return [int32]::Parse($text, [Globalization.CultureInfo]::InvariantCulture) }
        "QWord" { return [int64]::Parse($text, [Globalization.CultureInfo]::InvariantCulture) }
        # The leading comma keeps PowerShell from unrolling the array on return.
        "Binary" { return ,([Convert]::FromBase64String($text)) }
        "MultiString" { return ,([string[]]([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($text)).Split([char]0))) }
        default { throw "registry value kind $kind is not supported by the native-proof host-state guard" }
    }
}

# Returns $null when the key is absent, else its values in storage order, so a
# restore writes them back in the order they were found.
function Read-RegistryKey([string]$subKey) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subKey)
    if ($null -eq $key) { return $null }
    try {
        if ($key.SubKeyCount -ne 0) {
            throw "HKCU\$subKey has subkeys, which the native-proof host-state guard cannot restore"
        }
        $values = @()
        foreach ($name in @($key.GetValueNames())) {
            $kind = [string]$key.GetValueKind($name)
            $data = $key.GetValue($name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $values += [pscustomobject]@{ Name = $name; Kind = $kind; Data = (ConvertFrom-RegistryData $kind $data) }
        }
        return ,$values
    } finally { $key.Dispose() }
}

function Write-RegistryKey([string]$subKey, $values) {
    [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($subKey, $false)
    if ($null -eq $values) { return }
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($subKey)
    try {
        foreach ($value in @($values)) {
            $kind = [Microsoft.Win32.RegistryValueKind]$value.Kind
            $key.SetValue([string]$value.Name, (ConvertTo-RegistryData $value.Kind $value.Data), $kind)
        }
    } finally { $key.Dispose() }
}

function Read-RegistryValue([string]$subKey, [string]$name) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subKey)
    if ($null -eq $key) { return $null }
    try {
        if (@($key.GetValueNames()) -notcontains $name) { return $null }
        $kind = [string]$key.GetValueKind($name)
        $data = $key.GetValue($name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        return [pscustomobject]@{ Name = $name; Kind = $kind; Data = (ConvertFrom-RegistryData $kind $data) }
    } finally { $key.Dispose() }
}

function Write-RegistryValue([string]$subKey, [string]$name, $value) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($subKey)
    try {
        if ($null -eq $value) { $key.DeleteValue($name, $false) }
        else { $key.SetValue($name, (ConvertTo-RegistryData $value.Kind $value.Data), [Microsoft.Win32.RegistryValueKind]$value.Kind) }
    } finally { $key.Dispose() }
}

# One canonical string per registry state, so before/after compare exactly.
# Sorted by name: value order carries no meaning, and setup rewrites it.
function Format-RegistryState($values) {
    if ($null -eq $values) { return "<absent>" }
    return (@($values) | Where-Object { $null -ne $_ } | Sort-Object Name |
        ForEach-Object { "{0}|{1}|{2}" -f $_.Name, $_.Kind, $_.Data }) -join "`n"
}

function Test-RegistryNamesRoot($values, [string]$root) {
    foreach ($value in @($values)) {
        if ($null -eq $value) { continue }
        if ($value.Kind -in @("String", "ExpandString") -and
            $value.Data.IndexOf($root, [StringComparison]::OrdinalIgnoreCase) -ge 0) { return $true }
    }
    return $false
}

# --- shortcuts ------------------------------------------------------------

function Get-DefaultShortcutFolders {
    return @(
        [Environment]::GetFolderPath("Desktop"),
        [Environment]::GetFolderPath("Programs"),
        [Environment]::GetFolderPath("Startup")
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
}

function Get-Shortcuts([string[]]$folders) {
    $found = @()
    foreach ($folder in $folders) {
        if (-not (Test-Path -LiteralPath $folder -PathType Container)) { continue }
        foreach ($file in @(Get-ChildItem -LiteralPath $folder -Filter "*.lnk" -File -Force)) {
            $found += $file
        }
    }
    return ,$found
}

$Shell = $null
function Get-ShortcutTarget([string]$path) {
    if ($null -eq $script:Shell) { $script:Shell = New-Object -ComObject WScript.Shell }
    $link = $script:Shell.CreateShortcut($path)
    return [pscustomobject]@{ Target = [string]$link.TargetPath; WorkingDirectory = [string]$link.WorkingDirectory }
}

# --- modes ----------------------------------------------------------------

$UninstallSubKey = "$UninstallRootSubKey\$UninstallName"
if ($null -eq $ShortcutFolders -or $ShortcutFolders.Count -eq 0) { $ShortcutFolders = @(Get-DefaultShortcutFolders) }

if ($Mode -eq "Capture") {
    if (Test-Path -LiteralPath $StatePath) { throw "host-state file already exists; every native proof captures into a fresh file" }
    if (Test-Path -LiteralPath $LogPath) { Remove-Item -LiteralPath $LogPath -Force }
    $shortcuts = @()
    foreach ($file in (Get-Shortcuts $ShortcutFolders)) {
        $bytes = [IO.File]::ReadAllBytes($file.FullName)
        $shortcuts += [pscustomobject]@{
            Path = $file.FullName
            Sha256 = (Get-Sha256Hex $bytes)
            LastWriteTimeUtcTicks = $file.LastWriteTimeUtc.Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)
            Bytes = [Convert]::ToBase64String($bytes)
        }
    }
    $uninstall = Read-RegistryKey $UninstallSubKey
    $run = Read-RegistryValue $RunSubKey $RunValueName
    $state = [pscustomobject]@{
        schema = "solstone.native-proof-host-state.v1"
        uninstall_present = ($null -ne $uninstall)
        uninstall = @($uninstall)
        run_present = ($null -ne $run)
        run = $run
        shortcut_folders = @($ShortcutFolders)
        shortcuts = @($shortcuts)
    }
    [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 6), $Utf8)
    Write-Report ("host-state before: uninstall entry {0} ({1} values); login item {2}; {3} shortcuts in {4} folders" -f
        $(if ($null -ne $uninstall) { "present" } else { "absent" }), @($uninstall).Count,
        $(if ($null -ne $run) { "present" } else { "absent" }), $shortcuts.Count, $ShortcutFolders.Count)
    Write-Report "HOST_STATE_CAPTURED"
    exit 0
}

# Restore.
if ([string]::IsNullOrWhiteSpace($ProofRoot)) { throw "restore requires -ProofRoot" }
if (-not (Test-Path -LiteralPath $StatePath -PathType Leaf)) { throw "host-state file is missing; nothing was captured to restore" }
$state = [IO.File]::ReadAllText($StatePath, $Utf8) | ConvertFrom-Json
if ($state.schema -ne "solstone.native-proof-host-state.v1") { throw "host-state file has an unknown schema" }
$Root = ConvertTo-ComparablePath $ProofRoot
$Errors = @()

$CapturedUninstall = if ($state.uninstall_present) { @($state.uninstall | Where-Object { $null -ne $_ }) } else { $null }
$CapturedRun = if ($state.run_present) { $state.run } else { $null }
$Folders = @($state.shortcut_folders)
$CapturedShortcuts = @{}
foreach ($shortcut in @($state.shortcuts | Where-Object { $null -ne $_ })) { $CapturedShortcuts[$shortcut.Path.ToLowerInvariant()] = $shortcut }

# Uninstall entry: put the recorded key back exactly.
$Before = Read-RegistryKey $UninstallSubKey
$BeforeText = Format-RegistryState $Before
$WantText = Format-RegistryState $CapturedUninstall
if ($BeforeText -ne $WantText) {
    $changed = @()
    $wantByName = @{}
    foreach ($value in @($CapturedUninstall)) { if ($null -ne $value) { $wantByName[$value.Name] = "$($value.Kind)|$($value.Data)" } }
    foreach ($value in @($Before)) {
        if ($null -eq $value) { continue }
        if ($wantByName[$value.Name] -ne "$($value.Kind)|$($value.Data)") { $changed += $value.Name }
    }
    Write-RegistryKey $UninstallSubKey $CapturedUninstall
    Write-Report ("host-state uninstall entry: changed during the proof ({0}); restored" -f $(if ($changed.Count) { $changed -join ", " } else { "key presence" }))
} else {
    Write-Report "host-state uninstall entry: unchanged"
}

# Login item: the smoke restores it too; this covers a proof that failed before
# the smoke ran.
$BeforeRun = Read-RegistryValue $RunSubKey $RunValueName
if ((Format-RegistryState $BeforeRun) -ne (Format-RegistryState $CapturedRun)) {
    Write-RegistryValue $RunSubKey $RunValueName $CapturedRun
    Write-Report "host-state login item: changed during the proof; restored"
} else {
    Write-Report "host-state login item: unchanged"
}

# Shortcuts: anything that now targets the proof root is the proof's.
$restoredPaths = @()
$removedCount = 0
foreach ($file in (Get-Shortcuts $Folders)) {
    $link = Get-ShortcutTarget $file.FullName
    if (-not ((Test-UnderRoot $link.Target $Root) -or (Test-UnderRoot $link.WorkingDirectory $Root))) { continue }
    $captured = $CapturedShortcuts[$file.FullName.ToLowerInvariant()]
    if ($null -ne $captured) {
        [IO.File]::WriteAllBytes($file.FullName, [Convert]::FromBase64String($captured.Bytes))
        $ticks = [int64]::Parse($captured.LastWriteTimeUtcTicks, [Globalization.CultureInfo]::InvariantCulture)
        [IO.File]::SetLastWriteTimeUtc($file.FullName, (New-Object DateTime -ArgumentList @($ticks, [DateTimeKind]::Utc)))
        $restoredPaths += $file.FullName
        Write-Report "host-state shortcut $($file.Name): pointed into the proof root; restored"
    } else {
        Remove-Item -LiteralPath $file.FullName -Force
        $removedCount++
        Write-Report "host-state shortcut $($file.Name): created by the proof; removed"
    }
}
if ($restoredPaths.Count + $removedCount -eq 0) { Write-Report "host-state shortcuts: none pointed into the proof root" }

# After: check what is on the box now, not what this script meant to do.
if ((Format-RegistryState (Read-RegistryKey $UninstallSubKey)) -ne $WantText) {
    $Errors += "the uninstall entry does not equal its recorded state"
}
if ((Format-RegistryState (Read-RegistryValue $RunSubKey $RunValueName)) -ne (Format-RegistryState $CapturedRun)) {
    $Errors += "the login item does not equal its recorded state"
}
$UninstallRoot = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($UninstallRootSubKey)
if ($null -ne $UninstallRoot) {
    try {
        foreach ($name in @($UninstallRoot.GetSubKeyNames())) {
            $sub = $UninstallRoot.OpenSubKey($name)
            if ($null -eq $sub) { continue }
            try {
                foreach ($valueName in @($sub.GetValueNames())) {
                    $data = $sub.GetValue($valueName, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                    if ($data -is [string] -and $data.IndexOf($Root, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                        $Errors += "uninstall entry $name still names the proof root"
                        break
                    }
                }
            } finally { $sub.Dispose() }
        }
    } finally { $UninstallRoot.Dispose() }
}
if (Test-RegistryNamesRoot @(Read-RegistryValue $RunSubKey $RunValueName) $Root) { $Errors += "the login item still names the proof root" }
foreach ($file in (Get-Shortcuts $Folders)) {
    $link = Get-ShortcutTarget $file.FullName
    if ((Test-UnderRoot $link.Target $Root) -or (Test-UnderRoot $link.WorkingDirectory $Root)) {
        $Errors += "shortcut $($file.Name) still points into the proof root"
    }
}
foreach ($path in $restoredPaths) {
    $captured = $CapturedShortcuts[$path.ToLowerInvariant()]
    if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or
        (Get-Sha256Hex ([IO.File]::ReadAllBytes($path))) -ne $captured.Sha256) {
        $Errors += "shortcut $(Split-Path -Leaf $path) does not equal its recorded bytes"
    }
}

if ($Errors.Count -gt 0) {
    Write-Report ("host-state after: FAILED - " + ($Errors -join "; "))
    exit 1
}
Write-Report "host-state after: uninstall entry, login item and shortcuts match the recorded state; nothing names the proof root"
Write-Report "HOST_STATE_RESTORED"
exit 0
