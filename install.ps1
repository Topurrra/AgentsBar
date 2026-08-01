# AgentsBar installer - https://github.com/Topurrra/AgentsBar
#
# WHAT THIS SCRIPT DOES (and it does NOTHING else - this is on purpose):
#   1. Downloads AgentsBar-x64.zip and its .sha256 from the latest GitHub release.
#   2. Verifies the SHA-256 hash. Mismatch => abort, delete download.
#   3. Stops AgentsBar if it is running (it holds its own exe open, so an in-place
#      upgrade cannot work otherwise). It tells you before it does that.
#   4. Extracts to %LOCALAPPDATA%\Programs\AgentsBar, replacing any previous install.
#      Any previous install is renamed to <dir>.old first and only deleted once the
#      new copy is in place, so a failure leaves you with the version you had.
#   5. Creates a Start Menu shortcut in your own profile, and offers to start the app.
#   6. Prints how to uninstall.
#
#   No admin. No PATH changes (AgentsBar is a tray app, not a CLI). No registry
#   writes. No telemetry, no analytics, no hidden prompts. The only paths written
#   are %LOCALAPPDATA%\Programs\AgentsBar, the staging directory
#   %LOCALAPPDATA%\AgentsBar\install-tmp (deleted on the way out), and the Start
#   Menu shortcut. All inside your user profile. Deliberately NOT %TEMP%: that is
#   world-writable on machines where it has been redirected. Nothing under
#   %APPDATA%\AgentsBar (your settings and history) is touched, created or
#   deleted. Read it top to bottom - that is the whole thing.
#
# Usage:
#   iwr -useb https://volibear.dev/agentsbar | iex
#   iwr -useb https://github.com/Topurrra/AgentsBar/releases/latest/download/install.ps1 | iex
#
# Dev/testing:
#   .\install.ps1 -ZipPath C:\path\to\AgentsBar-x64.zip
#     Skips the download and installs from a local zip. If a sibling
#     <zip>.sha256 exists it is verified; otherwise the hash step is skipped.
#   .\install.ps1 -NoStart
#     Never prompts to launch the app.

[CmdletBinding()]
param(
    [string] $ZipPath,
    [switch] $NoStart
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Old PowerShell (5.1) defaults to TLS 1.0; GitHub requires 1.2+.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$BaseUrl = 'https://github.com/Topurrra/AgentsBar/releases/latest/download'
$ZipName = 'AgentsBar-x64.zip'
$ExeName = 'agentsbar.exe'

function Write-Info  { param($m) Write-Host $m -ForegroundColor Cyan }
function Write-Ok    { param($m) Write-Host $m -ForegroundColor Green }
function Write-Warn2 { param($m) Write-Host $m -ForegroundColor Yellow }

function Get-Download {
    param([string] $Url, [string] $OutFile)
    # Invoke-WebRequest surfaces HTTP 404 as a terminating error we catch below.
    Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing
}

function Invoke-Retry {
    param([scriptblock] $Action)
    # Windows holds the image lock on an exe for a beat after the process dies, so a delete
    # or rename right after Stop-Process loses a race we can simply wait out.
    # Measured: it clears in well under a second. 10 tries is 5s of headroom.
    for ($i = 1; $i -lt 10; $i++) {
        try { & $Action; return } catch { Start-Sleep -Milliseconds 500 }
    }
    & $Action   # last attempt, and this one is allowed to throw
}

function Assert-Hash {
    param([string] $ZipFile, [string] $ShaFile)
    # The .sha256 file's first whitespace-delimited token is the hex digest
    # (tolerates both a bare hash and sha256sum's "<hash>  <name>" format).
    $expected = ((Get-Content $ShaFile -Raw).Trim() -split '\s+')[0].ToLower()
    $actual   = (Get-FileHash $ZipFile -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) {
        throw "SHA-256 mismatch: expected $expected, got $actual"
    }
}

# No admin required. Some users insist on running elevated - allow it, but warn: the
# install dir and the Start Menu shortcut land in the elevated account's profile,
# which may not be yours.
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Warn2 'Warning: running elevated. AgentsBar needs no admin; the install and shortcut go to the elevated account.'
}

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\AgentsBar'
# NOT %TEMP%. That is only per-user by default; where it has been redirected (TEMP=C:\Temp
# is common) icacls reports BUILTIN\Users:(OI)(CI)(F), so another local account could swap
# the payload in the window between the hash check and the copy. %LOCALAPPDATA% cannot be
# redirected out of the profile by an environment variable, and it is where the reference
# voli installer stages too.
$tmpDir     = Join-Path $env:LOCALAPPDATA 'AgentsBar\install-tmp'
$backupDir  = "$installDir.old"
$startMenu  = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
$shortcut   = Join-Path $startMenu 'AgentsBar.lnk'

# Fresh temp dir every run.
if (Test-Path $tmpDir) { Remove-Item -Recurse -Force $tmpDir }
New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null

$cleanupZip    = $null   # a download we own and should delete on failure
$restoreBackup = $false  # true while the previous install is renamed aside

try {
    if ($ZipPath) {
        # ---- Local zip (dev/testing) ----
        if (-not (Test-Path $ZipPath)) {
            throw "zip not found: $ZipPath"
        }
        $zip = (Resolve-Path $ZipPath).Path
        $shaFile = "$zip.sha256"
        if (Test-Path $shaFile) {
            Write-Info "Verifying $zip against $shaFile ..."
            Assert-Hash -ZipFile $zip -ShaFile $shaFile
            Write-Ok 'Hash OK.'
        } else {
            Write-Warn2 'No sibling .sha256 found; skipping hash verification (local zip).'
        }
    } else {
        # ---- Download from GitHub releases/latest ----
        $zip     = Join-Path $tmpDir $ZipName
        $shaFile = "$zip.sha256"
        $cleanupZip = $zip

        Write-Info "Downloading AgentsBar from $BaseUrl/$ZipName ..."
        Get-Download -Url "$BaseUrl/$ZipName"         -OutFile $zip
        Get-Download -Url "$BaseUrl/$ZipName.sha256"  -OutFile $shaFile

        Write-Info 'Verifying SHA-256 ...'
        Assert-Hash -ZipFile $zip -ShaFile $shaFile
        Write-Ok 'Hash OK.'
    }

    # ---- Extract and sanity check before touching the existing install ----
    $extractDir = Join-Path $tmpDir 'extract'
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
    Write-Info 'Extracting ...'
    Expand-Archive -Path $zip -DestinationPath $extractDir -Force

    $newExe = Join-Path $extractDir $ExeName
    if (-not (Test-Path $newExe)) {
        throw "$ExeName not found at the root of the archive"
    }

    # ---- Stop a running copy: it holds its own exe open ----
    $running = @(Get-Process -Name 'agentsbar' -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        Write-Warn2 'AgentsBar is running and holds its own exe open. Stopping it so it can be replaced ...'
        $running | Stop-Process -Force
        # Wait-Process, not a sleep: the file lock is released when the process exits.
        $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    }

    # ---- Install, replacing any previous copy ----
    # The previous install is renamed aside, not deleted, so a copy that fails halfway
    # (disk full, AV quarantining the exe) leaves you with the working version you had
    # rather than with nothing. The backup is deleted only once the copy succeeded.
    if (Test-Path $installDir) {
        Write-Info "Replacing previous install in $installDir ..."
        if (Test-Path $backupDir) { Invoke-Retry { Remove-Item -Recurse -Force $backupDir } }
        Invoke-Retry { Move-Item -LiteralPath $installDir -Destination $backupDir -Force }
        $restoreBackup = $true
    } else {
        Write-Info "Installing to $installDir ..."
    }
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Copy-Item -Path (Join-Path $extractDir '*') -Destination $installDir -Recurse -Force
    if ($restoreBackup) {
        $restoreBackup = $false
        Invoke-Retry { Remove-Item -Recurse -Force $backupDir }
    }

    $exe = Join-Path $installDir $ExeName

    # ---- Start Menu shortcut (per-user, no admin) ----
    New-Item -ItemType Directory -Force -Path $startMenu | Out-Null
    $wsh = New-Object -ComObject WScript.Shell
    $lnk = $wsh.CreateShortcut($shortcut)
    $lnk.TargetPath       = $exe
    $lnk.WorkingDirectory = $installDir
    $lnk.Description      = 'AgentsBar - AI coding usage limits in your tray'
    $lnk.Save()

    $version = (Get-Item $exe).VersionInfo.ProductVersion

    Write-Host ''
    Write-Ok "Installed AgentsBar $version to $installDir"
    Write-Host "Start Menu shortcut: $shortcut"
    Write-Host ''
    Write-Host 'To uninstall:'
    Write-Host '  1. Quit AgentsBar from its tray menu (or: Stop-Process -Name agentsbar -Force)'
    Write-Host "  2. Remove-Item -Recurse -Force `"$installDir`""
    Write-Host "  3. Remove-Item -Force `"$shortcut`""
    Write-Host "  4. If you enabled 'Launch at startup', the app wrote an autostart entry. Remove it:"
    Write-Host "     Remove-ItemProperty 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run' -Name AgentsBar -ErrorAction SilentlyContinue"
    Write-Host "  5. Optional, this deletes your settings and history: Remove-Item -Recurse -Force `"$env:APPDATA\AgentsBar`""
    Write-Host ''

    if (-not $NoStart) {
        # The only prompt in this script, and it is skippable with -NoStart.
        try { $answer = Read-Host 'Start AgentsBar now? [Y/n]' } catch { $answer = 'n' }
        if ($answer -notmatch '^\s*n') {
            Start-Process -FilePath $exe -WorkingDirectory $installDir
            Write-Ok 'Started. Look for the AgentsBar icon in your tray.'
        }
    }
}
catch {
    # Captured before the restore below, whose own try/catch would otherwise shadow $_.
    $failure = $_.Exception.Message
    Write-Host ''
    if ($restoreBackup) {
        # The copy did not finish. Put the working install back rather than leaving nothing.
        Write-Warn2 "Install failed partway. Restoring the previous copy from $backupDir ..."
        try {
            if (Test-Path $installDir) { Invoke-Retry { Remove-Item -Recurse -Force $installDir } }
            Invoke-Retry { Move-Item -LiteralPath $backupDir -Destination $installDir -Force }
            Write-Ok 'Previous install restored.'
        } catch {
            Write-Warn2 "Could not restore automatically. Your previous install is at $backupDir; rename it back to $installDir."
        }
    }
    if ($failure -match '404' -or $failure -match 'Not Found') {
        Write-Warn2 'No published AgentsBar release was found (404). Check https://github.com/Topurrra/AgentsBar/releases.'
    } else {
        Write-Host "Install failed: $failure" -ForegroundColor Red
    }
    if ($cleanupZip -and (Test-Path $cleanupZip)) { Remove-Item -Force $cleanupZip -ErrorAction SilentlyContinue }
    exit 1
}
finally {
    # Always clean up the temp extraction dir.
    if (Test-Path $tmpDir) { Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue }
}
