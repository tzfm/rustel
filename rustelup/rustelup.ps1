$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($PSVersionTable.PSVersion.Major -lt 5) {
    [Console]::Error.WriteLine('rustelup: need PowerShell 5 or later')
    exit 1
}

if ($env:OS -ne 'Windows_NT') {
    [Console]::Error.WriteLine('rustelup: this updater is for Windows; use the bash rustelup on this platform')
    exit 1
}

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
}

# The public repository this installer serves.
$Repo = 'tzfm/rustel'
if ($env:RUSTELUP_REPO) { $Repo = $env:RUSTELUP_REPO }

$HomeDir = $env:USERPROFILE
if (-not $HomeDir) { $HomeDir = $HOME }
$RustelDir = $env:RUSTEL_DIR
if (-not $RustelDir) { $RustelDir = Join-Path $HomeDir '.rustel' }
$BinDir = Join-Path $RustelDir 'bin'
$SrcDir = Join-Path $RustelDir 'src'
$BaseUrl = "https://github.com/$Repo"

function Show-Usage {
    [Console]::Error.WriteLine(@'
rustelup: install or update the rustel live-coding engine.

USAGE:
    rustelup [OPTIONS]

OPTIONS:
    -v, --version TAG     Install a specific release tag (default: latest)
    -b, --branch BRANCH   Build and install a branch from source
    -c, --commit HASH     Build and install a commit from source
    -l, --list            List available releases
    -h, --help            Print this help
'@)
}

function say([string]$Message) {
    [Console]::Out.WriteLine("rustelup: $Message")
}

function err([string]$Message) {
    [Console]::Error.WriteLine("rustelup: $Message")
    exit 1
}

function Need-Command([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        err "need '$Name' (command not found)"
    }
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

function Get-RemoteJson([string]$Url) {
    $tmpJson = Join-Path ([System.IO.Path]::GetTempPath()) ("rustelup-json-" + [guid]::NewGuid().ToString('N'))
    try {
        Invoke-RustelFetch $Url $tmpJson "download failed: $Url"
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

$Tag = ''
$Branch = ''
$Commit = ''
$scriptArgs = @($args)
$i = 0
while ($i -lt $scriptArgs.Count) {
    $arg = [string]$scriptArgs[$i]
    if ($arg -in '-v', '--version', '-b', '--branch', '-c', '--commit') {
        $flag = $arg
        $i++
        if ($i -ge $scriptArgs.Count) {
            Show-Usage
            err "$flag requires a value"
        }
        $next = [string]$scriptArgs[$i]
        if ($next -eq '' -or $next.StartsWith('-')) {
            Show-Usage
            err "$flag requires a value"
        }
        if ($flag -in '-v', '--version') {
            $Tag = $next
        } elseif ($flag -in '-b', '--branch') {
            $Branch = $next
        } else {
            $Commit = $next
        }
    } elseif ($arg -eq '-l' -or $arg -eq '--list') {
        $releases = Get-RemoteJson "https://api.github.com/repos/$Repo/releases?per_page=20"
        foreach ($release in @($releases)) {
            $name = [string]$release.tag_name
            if ($name -match '^v') {
                [Console]::Out.WriteLine($name)
            }
        }
        exit 0
    } elseif ($arg -eq '-h' -or $arg -eq '--help') {
        Show-Usage
        exit 0
    } else {
        Show-Usage
        err "unknown option: $arg"
    }
    $i++
}

if (@($Tag, $Branch, $Commit | Where-Object { $_ }).Count -gt 1) {
    Show-Usage
    err 'use one of --version, --branch and --commit'
}

$FromSource = [bool]($Branch -or $Commit)
if ($FromSource) {
    Need-Command git
    Need-Command cargo
} else {
    Need-Command tar
}

$archEnv = $env:PROCESSOR_ARCHITECTURE
$wowArch = $env:PROCESSOR_ARCHITEW6432
$Arch = $null
if ($archEnv -eq 'AMD64' -or $wowArch -eq 'AMD64') {
    $Arch = 'amd64'
} else {
    err "windows builds ship for x86_64 only"
}

$Bin = 'rustel.exe'
$Revision = ''

if ($FromSource) {
    $Name = $Commit
    $Ref = $Commit
    if ($Branch) {
        $Name = $Branch
        $Ref = "refs/heads/$Branch"
    }
    if ($Branch -notmatch '^[A-Za-z0-9._/-]*$') {
        err "invalid branch name: $Branch"
    }
    # A shallow fetch finds a commit by its full hash only.
    if ($Commit -cnotmatch '^[0-9a-f]*$') {
        err "invalid commit hash: $Commit"
    }
    if ($Commit -and $Commit.Length -ne 40) {
        err '--commit requires the full 40-character hash'
    }
} else {
    if (-not $Tag) {
        $latest = Get-RemoteJson "https://api.github.com/repos/$Repo/releases/latest"
        $Tag = [string]$latest.tag_name
        if (-not $Tag) { err "no releases found at $Repo" }
    }
    if ($Tag -notmatch '^v[0-9]') {
        err "invalid release tag: $Tag"
    }
    if ($Tag -notmatch '^[A-Za-z0-9._-]+$') {
        err "invalid release tag: $Tag"
    }
}

$Tmp = $null
$Stage = $null
try {
    if ($FromSource) {
        # Build in $SrcDir with the release profile. The checkout and its
        # `target` directory stay, so the next build compiles the changes only.
        $origin = 'built'
        say "building rustel $Name from source (win32 $Arch)"
        if (-not (Test-Path -LiteralPath (Join-Path $SrcDir '.git'))) {
            & git init -q $SrcDir
            if ($LASTEXITCODE -ne 0) { err "could not create $SrcDir" }
        }
        & git -C $SrcDir fetch --depth 1 "$BaseUrl.git" $Ref
        if ($LASTEXITCODE -ne 0) { err "could not fetch $Name from $BaseUrl" }
        & git -C $SrcDir checkout -q --detach FETCH_HEAD
        if ($LASTEXITCODE -ne 0) { err "could not check out $Name in $SrcDir" }
        # A local edit would build under the hash of the commit.
        if (& git -C $SrcDir status --porcelain --untracked-files=no) {
            err "$SrcDir has local changes"
        }
        $Revision = ([string](& git -C $SrcDir rev-parse HEAD)).Trim()

        # rustup reads the pinned toolchain from the working directory.
        # `--target` and `--target-dir` keep the binary in one place when the
        # Cargo environment or configuration sets another. `rustel doctor`
        # and session tapes report BUILD_REVISION.
        $triple = ''
        $built = $false
        $env:BUILD_REVISION = $Revision
        Push-Location -LiteralPath $SrcDir
        try {
            $triple = ([string](& rustc -vV | Where-Object { $_ -like 'host: *' })) -replace '^host: ', ''
            if ($triple) {
                & cargo build --locked --release -p rustel --target $triple --target-dir target
                $built = ($LASTEXITCODE -eq 0)
            }
        } finally {
            Pop-Location
            Remove-Item Env:\BUILD_REVISION -ErrorAction SilentlyContinue
        }
        if (-not $triple) { err 'could not read the host target from rustc' }
        if (-not $built) { err "build failed in $SrcDir" }
        $newBin = Join-Path $SrcDir "target\$triple\release\$Bin"
        if (-not (Test-Path -LiteralPath $newBin -PathType Leaf)) {
            err "build did not produce $newBin"
        }
    } else {
        $origin = 'downloaded'
        $Archive = "rustel_${Tag}_win32_${Arch}.tar.gz"
        $Url = "$BaseUrl/releases/download/$Tag/$Archive"
        say "installing rustel $Tag (win32 $Arch)"

        $Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("rustelup-" + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $Tmp | Out-Null

        $archivePath = Join-Path $Tmp $Archive
        $sidecarPath = Join-Path $Tmp ($Archive + '.sha256')
        Invoke-RustelFetch $Url $archivePath "download failed: $Url"
        Invoke-RustelFetch ($Url + '.sha256') $sidecarPath "checksum download failed: $Url.sha256"
        Test-Sha256 $archivePath $sidecarPath

        & tar.exe -xzf $archivePath -C $Tmp
        if ($LASTEXITCODE -ne 0) { err "failed to extract $Archive" }
        $newBin = Join-Path $Tmp $Bin
        if (-not (Test-Path -LiteralPath $newBin -PathType Leaf)) {
            err "archive did not contain $Bin"
        }
    }

    if (-not (Test-Path -LiteralPath $BinDir)) {
        New-Item -ItemType Directory -Path $BinDir | Out-Null
    }
    $stageName = '.rustelup.' + [guid]::NewGuid().ToString('N').Substring(0, 8)
    $Stage = Join-Path $BinDir $stageName
    New-Item -ItemType Directory -Path $Stage | Out-Null
    $staged = Join-Path $Stage $Bin
    Copy-Item -LiteralPath $newBin -Destination $staged -Force
    & $staged --version > $null
    if ($LASTEXITCODE -ne 0) {
        err "$origin $Bin failed its version check"
    }

    $dest = Join-Path $BinDir $Bin
    $previous = Join-Path $Stage 'previous'
    if (Test-Path -LiteralPath $dest) {
        Copy-Item -LiteralPath $dest -Destination $previous -Force
    }
    if (-not (Move-InstalledFile $staged $dest)) {
        if ((Test-Path -LiteralPath $previous) -and -not (Test-Path -LiteralPath $dest)) {
            if (-not (Move-InstalledFile $previous $dest)) {
                err "could not restore previous $Bin"
            }
        }
        err "could not replace $dest"
    }

    $version = $null
    $ok = $false
    try {
        $version = & $dest --version 2>$null
        $ok = ($LASTEXITCODE -eq 0)
    } catch {
        $ok = $false
    }
    if (-not $ok) {
        if (Test-Path -LiteralPath $previous) {
            if (-not (Move-InstalledFile $previous $dest)) {
                err "could not restore previous $Bin"
            }
            err "installed $Bin failed its version check; previous binary restored"
        }
        Remove-Item -LiteralPath $dest -Force -ErrorAction SilentlyContinue
        err "installed $Bin failed its version check and was removed"
    }

    if ($Revision) { $version = "$version at " + $Revision.Substring(0, 12) }
    say "installed: $version"
    say "run 'rustel studio' to open the studio, or 'rustel' for the CLI"
    $pathMatch = ';' + $env:Path + ';'
    if ($pathMatch -notlike ('*;' + $BinDir + ';*')) {
        say "add $BinDir to your PATH to run 'rustel' from anywhere"
    }
} finally {
    if ($Tmp -and (Test-Path -LiteralPath $Tmp)) {
        Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ($Stage -and (Test-Path -LiteralPath $Stage)) {
        Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
    }
}
