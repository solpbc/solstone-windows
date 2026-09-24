# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Round-trips scripts\native-proof-host-state.ps1 against a scratch HKCU key and
# scratch shortcut folders: a fake "setup" repoints them into a proof root the
# way Velopack's Setup.exe does, and the restore must put back exactly what was
# captured, fail closed while anything still names the proof root, and leave
# unrelated shortcuts alone. Never touches the real Uninstall or Run keys.

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Guard = Join-Path $RepoRoot "scripts\native-proof-host-state.ps1"
$Assertions = 0

function Assert-True([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw "native-proof-host-state.test.ps1: assertion failed: $Label" }
    $script:Assertions++
}

$Id = [guid]::NewGuid().ToString("N")
$ScratchKey = "Software\solstone-native-proof-host-state-test-$Id"
$UninstallRoot = "$ScratchKey\Uninstall"
$RunKey = "$ScratchKey\Run"
$Scratch = Join-Path ([IO.Path]::GetTempPath()) "solstone-host-state-test-$Id"
$Desktop = Join-Path $Scratch "Desktop"
$Programs = Join-Path $Scratch "Programs"
$ProofRoot = Join-Path $Scratch "release-native-proof\2.0.10\.native-proof-0123.tmp"
$RealApp = Join-Path $Scratch "Local\Solstone\current\solstone-windows-app.exe"
$ProofApp = Join-Path $ProofRoot "Solstone\current\solstone-windows-app.exe"
$Shell = New-Object -ComObject WScript.Shell

function New-Shortcut([string]$path, [string]$target) {
    $link = $Shell.CreateShortcut($path)
    $link.TargetPath = $target
    $link.WorkingDirectory = Split-Path -Parent $target
    $link.Save()
}
function Get-Hash([string]$path) { return (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
function Get-KeyText([string]$subKey) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subKey)
    if ($null -eq $key) { return "<absent>" }
    try {
        return (@($key.GetValueNames()) | ForEach-Object {
            $data = $key.GetValue($_, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            "{0}|{1}|{2}" -f $_, $key.GetValueKind($_), (@($data) -join ",")
        }) -join "`n"
    } finally { $key.Dispose() }
}
function Invoke-Guard([string]$mode, [string]$state) {
    $params = @{
        Mode = $mode
        StatePath = $state
        UninstallRootSubKey = $UninstallRoot
        RunSubKey = $RunKey
        ShortcutFolders = @($Desktop, $Programs)
    }
    if ($mode -eq "Restore") { $params.ProofRoot = $ProofRoot }
    $global:LASTEXITCODE = 0
    $text = (& $Guard @params 6>&1 | Out-String)
    return [pscustomobject]@{ Exit = $LASTEXITCODE; Text = $text }
}
function Test-Line([string]$text, [string]$line) {
    return @($text -split "`r?`n") -contains $line
}
function Reset-Scratch {
    [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($ScratchKey, $false)
    if (Test-Path -LiteralPath $Scratch) { Remove-Item -LiteralPath $Scratch -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $Desktop, $Programs, $ProofRoot | Out-Null
}

try {
    # Case 1: a registered install, repointed by setup, comes back exactly.
    Reset-Scratch
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("$UninstallRoot\Solstone")
    $key.SetValue("DisplayName", "solstone", [Microsoft.Win32.RegistryValueKind]::String)
    $key.SetValue("InstallLocation", (Split-Path -Parent (Split-Path -Parent $RealApp)), [Microsoft.Win32.RegistryValueKind]::String)
    $key.SetValue("DisplayIcon", "%LOCALAPPDATA%\Solstone\app.exe", [Microsoft.Win32.RegistryValueKind]::ExpandString)
    $key.SetValue("NoModify", 1, [Microsoft.Win32.RegistryValueKind]::DWord)
    $key.SetValue("EstimatedSize", [int64]21060, [Microsoft.Win32.RegistryValueKind]::QWord)
    $key.SetValue("Blob", [byte[]](0, 1, 254, 255), [Microsoft.Win32.RegistryValueKind]::Binary)
    $key.SetValue("Lines", [string[]]@("a", "b"), [Microsoft.Win32.RegistryValueKind]::MultiString)
    $key.Dispose()
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($RunKey)
    $key.SetValue("Solstone", "`"$RealApp`" --from-autostart", [Microsoft.Win32.RegistryValueKind]::String)
    $key.Dispose()
    New-Shortcut (Join-Path $Desktop "solstone.lnk") $RealApp
    New-Shortcut (Join-Path $Desktop "other.lnk") "C:\Windows\notepad.exe"
    $BaselineUninstall = Get-KeyText "$UninstallRoot\Solstone"
    $BaselineRun = Get-KeyText $RunKey
    $BaselineDesktopHash = Get-Hash (Join-Path $Desktop "solstone.lnk")
    $BaselineDesktopTime = (Get-Item (Join-Path $Desktop "solstone.lnk")).LastWriteTimeUtc
    $BaselineOtherHash = Get-Hash (Join-Path $Desktop "other.lnk")

    $State = "$ProofRoot.host-state.json"
    $capture = Invoke-Guard "Capture" $State
    Assert-True ($capture.Exit -eq 0 -and (Test-Line $capture.Text "HOST_STATE_CAPTURED")) "capture succeeds and says so"
    Assert-True (Test-Path -LiteralPath "$State.log") "capture writes its report beside the state file"

    # What Velopack's setup does to the signed-in user.
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("$UninstallRoot\Solstone")
    $key.SetValue("InstallLocation", (Join-Path $ProofRoot "Solstone"), [Microsoft.Win32.RegistryValueKind]::String)
    $key.SetValue("UninstallString", "`"$ProofRoot\Solstone\Update.exe`" --uninstall", [Microsoft.Win32.RegistryValueKind]::String)
    $key.Dispose()
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($RunKey)
    $key.SetValue("Solstone", "`"$ProofApp`" --from-autostart", [Microsoft.Win32.RegistryValueKind]::String)
    $key.Dispose()
    New-Shortcut (Join-Path $Desktop "solstone.lnk") $ProofApp
    New-Shortcut (Join-Path $Programs "solstone.lnk") $ProofApp
    Assert-True ((Get-KeyText "$UninstallRoot\Solstone") -ne $BaselineUninstall) "fake setup changed the uninstall entry"
    Assert-True ((Get-Hash (Join-Path $Desktop "solstone.lnk")) -ne $BaselineDesktopHash) "fake setup changed the desktop shortcut"

    $restore = Invoke-Guard "Restore" $State
    Assert-True ($restore.Exit -eq 0 -and (Test-Line $restore.Text "HOST_STATE_RESTORED")) "restore succeeds and says so"
    Assert-True ($restore.Text.Contains("uninstall entry: changed during the proof (InstallLocation, UninstallString); restored")) "restore names the changed uninstall values"
    Assert-True ((Get-KeyText "$UninstallRoot\Solstone") -eq $BaselineUninstall) "uninstall entry is back, every kind, in order"
    Assert-True ((Get-KeyText $RunKey) -eq $BaselineRun) "login item is back"
    Assert-True ((Get-Hash (Join-Path $Desktop "solstone.lnk")) -eq $BaselineDesktopHash) "desktop shortcut bytes are back"
    Assert-True ((Get-Item (Join-Path $Desktop "solstone.lnk")).LastWriteTimeUtc -eq $BaselineDesktopTime) "desktop shortcut time is back"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $Programs "solstone.lnk"))) "a shortcut the proof created is removed"
    Assert-True ((Get-Hash (Join-Path $Desktop "other.lnk")) -eq $BaselineOtherHash) "an unrelated shortcut is untouched"

    $again = Invoke-Guard "Restore" $State
    Assert-True ($again.Exit -eq 0 -and (Test-Line $again.Text "HOST_STATE_RESTORED") -and
        $again.Text.Contains("uninstall entry: unchanged") -and
        $again.Text.Contains("shortcuts: none pointed into the proof root")) "a second restore is a no-op"

    $refused = $null
    try { Invoke-Guard "Capture" $State | Out-Null } catch { $refused = $_.Exception.Message }
    Assert-True ($null -ne $refused -and $refused.Contains("already exists")) "capture refuses a used state file"

    $missing = $null
    try { Invoke-Guard "Restore" (Join-Path $Scratch "never-captured.json") | Out-Null } catch { $missing = $_.Exception.Message }
    Assert-True ($null -ne $missing -and $missing.Contains("nothing was captured")) "restore refuses without a capture"

    # Case 2: nothing registered before the proof; restore removes what setup made.
    Reset-Scratch
    $State = "$ProofRoot.host-state.json"
    $capture = Invoke-Guard "Capture" $State
    Assert-True ($capture.Exit -eq 0 -and $capture.Text.Contains("uninstall entry absent")) "capture records an absent entry"
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("$UninstallRoot\Solstone")
    $key.SetValue("InstallLocation", (Join-Path $ProofRoot "Solstone"), [Microsoft.Win32.RegistryValueKind]::String)
    $key.Dispose()
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($RunKey)
    $key.SetValue("Solstone", "`"$ProofApp`" --from-autostart", [Microsoft.Win32.RegistryValueKind]::String)
    $key.Dispose()
    New-Shortcut (Join-Path $Desktop "solstone.lnk") $ProofApp
    $restore = Invoke-Guard "Restore" $State
    Assert-True ($restore.Exit -eq 0 -and (Test-Line $restore.Text "HOST_STATE_RESTORED")) "restore from an absent baseline succeeds"
    Assert-True ((Get-KeyText "$UninstallRoot\Solstone") -eq "<absent>") "an entry the proof created is removed"
    Assert-True (-not ((Get-KeyText $RunKey) -like "*Solstone*")) "a login item the proof created is removed"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $Desktop "solstone.lnk"))) "a desktop shortcut the proof created is removed"

    # Case 3: another registration still naming the proof root fails closed.
    Reset-Scratch
    $State = "$ProofRoot.host-state.json"
    Invoke-Guard "Capture" $State | Out-Null
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("$UninstallRoot\SomethingElse")
    $key.SetValue("InstallLocation", (Join-Path $ProofRoot "Solstone"), [Microsoft.Win32.RegistryValueKind]::String)
    $key.Dispose()
    $restore = Invoke-Guard "Restore" $State
    Assert-True ($restore.Exit -eq 1 -and -not (Test-Line $restore.Text "HOST_STATE_RESTORED")) "a leftover registration fails the restore"
    Assert-True ($restore.Text.Contains("uninstall entry SomethingElse still names the proof root")) "the failure names the leftover"
} finally {
    [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($ScratchKey, $false)
    if (Test-Path -LiteralPath $Scratch) { Remove-Item -LiteralPath $Scratch -Recurse -Force }
}

Write-Host "native-proof-host-state.test.ps1: $Assertions assertions passed"
