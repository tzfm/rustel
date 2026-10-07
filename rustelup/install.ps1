# Bootstrap for rustelup: puts the updater on your machine, then rustelup
# fetches the engine.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# `irm | iex` runs this text in the caller's session. A failure must throw:
# `exit` would close that session before the message can be read.
if ($PSVersionTable.PSVersion.Major -lt 5) {
    throw 'rustelup-init: need PowerShell 5 or later'
}

if ($env:OS -ne 'Windows_NT') {
    throw 'rustelup-init: this installer is for Windows; use the bash installer on this platform'
}

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
}

$Repo = 'tzfm/rustel'
if ($env:RUSTELUP_REPO) { $Repo = $env:RUSTELUP_REPO }

$HomeDir = $env:USERPROFILE
if (-not $HomeDir) { $HomeDir = $HOME }
$RustelDir = $env:RUSTEL_DIR
if (-not $RustelDir) { $RustelDir = Join-Path $HomeDir '.rustel' }
$BinDir = Join-Path $RustelDir 'bin'

function say([string]$Message) {
    [Console]::Out.WriteLine("rustelup-init: $Message")
}

function err([string]$Message) {
    throw "rustelup-init: $Message"
}

function Get-FileSha256([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-Sha256([string]$Path, [string]$Sidecar) {
    $expected = $null
    try {
        $expected = (Get-Content -LiteralPath $Sidecar -ErrorAction Stop | Out-String).Trim()
    } catch {
        err "cannot read checksum for $Path"
    }
    $expected = $expected.ToLowerInvariant()
    if ($expected -notmatch '^[0-9a-f]{64}$') {
        err "invalid checksum for $Path"
    }
    $actual = Get-FileSha256 $Path
    if ($expected -ne $actual) {
        err "checksum mismatch for $Path"
    }
}

function Invoke-RustelFetch([string]$Url, [string]$OutFile, [string]$FailMessage) {
    if (-not $FailMessage) { $FailMessage = "download failed: $Url" }
    try {
        Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing -Headers @{ 'User-Agent' = 'rustelup' }
    } catch {
        err $FailMessage
    }
}

function Get-RemoteJson([string]$Url, [string]$FailMessage) {
    $tmpJson = Join-Path ([System.IO.Path]::GetTempPath()) ("rustelup-init-json-" + [guid]::NewGuid().ToString('N'))
    try {
        Invoke-RustelFetch $Url $tmpJson $FailMessage
        Get-Content -LiteralPath $tmpJson -Raw | ConvertFrom-Json
    } finally {
        if (Test-Path -LiteralPath $tmpJson) {
            Remove-Item -LiteralPath $tmpJson -Force -ErrorAction SilentlyContinue
        }
    }
}

function Move-InstalledFile([string]$From, [string]$To) {
    try {
        Move-Item -LiteralPath $From -Destination $To -Force
        return $true
    } catch {
        return $false
    }
}

function Test-UpdaterHelp([string]$Path) {
    # The updater prints its help on stderr. Under 'Stop', Windows PowerShell
    # 5.1 turns redirected native stderr into a terminating error.
    $ErrorActionPreference = 'Continue'
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Path --help > $null 2>&1
    return ($LASTEXITCODE -eq 0)
}

function Write-UpdaterShim([string]$Directory) {
    $cmd = Join-Path $Directory 'rustelup.cmd'
    $lines = @(
        '@echo off'
        'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0rustelup.ps1" %*'
    )
    Set-Content -LiteralPath $cmd -Value $lines -Encoding Ascii
}

function Add-UserPath([string]$Directory) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $userPath) { $userPath = '' }
    $parts = @()
    if ($userPath -ne '') {
        $parts = @($userPath -split ';' | Where-Object { $_ -ne '' })
    }
    foreach ($part in $parts) {
        if ([string]::Equals($part, $Directory, [StringComparison]::OrdinalIgnoreCase)) {
            return $false
        }
    }
    if ($userPath -eq '') {
        $newPath = $Directory
    } else {
        $newPath = $Directory + ';' + $userPath
    }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = $Directory + ';' + $env:Path
    return $true
}

$latest = Get-RemoteJson "https://api.github.com/repos/$Repo/releases/latest" "could not find the latest release at $Repo"
$Tag = [string]$latest.tag_name
if ($Tag -notmatch '^v[0-9]') {
    err "invalid release tag: $Tag"
}
if ($Tag -notmatch '^[A-Za-z0-9._-]+$') {
    err "invalid release tag: $Tag"
}
$RustelupUrl = "https://github.com/$Repo/releases/download/$Tag/rustelup.ps1"

if (-not (Test-Path -LiteralPath $BinDir)) {
    New-Item -ItemType Directory -Path $BinDir | Out-Null
}

$Tmp = $null
$Stage = $null
$RunCommand = 'rustelup'
try {
    $Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("rustelup-init-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $Tmp | Out-Null

    $downloaded = Join-Path $Tmp 'rustelup.ps1'
    $sidecar = Join-Path $Tmp 'rustelup.ps1.sha256'
    Invoke-RustelFetch $RustelupUrl $downloaded "download failed: $RustelupUrl"
    Invoke-RustelFetch ($RustelupUrl + '.sha256') $sidecar "checksum download failed: $RustelupUrl.sha256"
    Test-Sha256 $downloaded $sidecar

    $errors = $null
    $tokens = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($downloaded, [ref]$tokens, [ref]$errors)
    if ($errors -and $errors.Count -gt 0) {
        err 'downloaded updater failed syntax check'
    }

    $stageName = '.rustelup-init.' + [guid]::NewGuid().ToString('N').Substring(0, 8)
    $Stage = Join-Path $BinDir $stageName
    New-Item -ItemType Directory -Path $Stage | Out-Null
    $staged = Join-Path $Stage 'rustelup.ps1'
    Copy-Item -LiteralPath $downloaded -Destination $staged -Force
    Unblock-File -LiteralPath $staged -ErrorAction SilentlyContinue
    if (-not (Test-UpdaterHelp $staged)) {
        err 'downloaded updater failed its help check'
    }

    $dest = Join-Path $BinDir 'rustelup.ps1'
    $previous = Join-Path $Stage 'previous'
    if (Test-Path -LiteralPath $dest) {
        Copy-Item -LiteralPath $dest -Destination $previous -Force
    }
    if (-not (Move-InstalledFile $staged $dest)) {
        if ((Test-Path -LiteralPath $previous) -and -not (Test-Path -LiteralPath $dest)) {
            if (-not (Move-InstalledFile $previous $dest)) {
                err 'could not restore previous updater'
            }
        }
        err "could not replace $dest"
    }
    Unblock-File -LiteralPath $dest -ErrorAction SilentlyContinue
    if (-not (Test-UpdaterHelp $dest)) {
        if (Test-Path -LiteralPath $previous) {
            if (-not (Move-InstalledFile $previous $dest)) {
                err 'could not restore previous updater'
            }
            err 'installed updater failed its help check; previous updater restored'
        }
        Remove-Item -LiteralPath $dest -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $BinDir 'rustelup.cmd') -Force -ErrorAction SilentlyContinue
        err 'installed updater failed its help check and was removed'
    }
    Write-UpdaterShim $BinDir
    say "installed rustelup to $dest"

    if (Add-UserPath $BinDir) {
        say "added $BinDir to PATH"
    }
    if ((Get-ExecutionPolicy) -eq 'Restricted') {
        # PowerShell finds rustelup.ps1 before the shim and refuses to run it.
        $RunCommand = 'rustelup.cmd'
        say 'PowerShell is Restricted; run rustelup.cmd or set CurrentUser RemoteSigned'
    }
} finally {
    if ($Tmp -and (Test-Path -LiteralPath $Tmp)) {
        Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ($Stage -and (Test-Path -LiteralPath $Stage)) {
        Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
    }
}

say "open a new terminal, then run: $RunCommand"
