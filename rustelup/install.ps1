# Bootstrap for rustelup: puts the updater on your machine, then rustelup
# fetches the engine.
# Invoke-Expression runs in the caller's scope. Keep preferences and helpers local.
& {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest

    if ($PSVersionTable.PSVersion.Major -lt 5) {
        throw 'rustelup-init: need PowerShell 5 or later'
    }

    if ($env:OS -ne 'Windows_NT') {
        throw 'rustelup-init: this installer is for Windows; use the bash installer on this platform'
    }

    $previousSecurityProtocol = [Net.ServicePointManager]::SecurityProtocol
    try {
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
        $RustelDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($RustelDir)
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
                Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing -TimeoutSec 60 -Headers @{ 'User-Agent' = 'rustelup' }
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
            try {
                & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Path --help > $null 2>&1
                return ($LASTEXITCODE -eq 0)
            } catch {
                return $false
            }
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
            # A previous install may have updated the registry after this terminal opened.
            $processParts = @($env:Path -split ';')
            if (-not ($processParts | Where-Object { [string]::Equals($_, $Directory, [StringComparison]::OrdinalIgnoreCase) })) {
                $env:Path = $Directory + ';' + $env:Path
            }
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
            return $true
        }

        $latest = Get-RemoteJson "https://api.github.com/repos/$Repo/releases/latest" "could not find a published release at $Repo; check your connection or try again after a release is published"
        if (-not $latest -or -not $latest.PSObject.Properties['tag_name']) {
            err 'invalid or missing release tag'
        }
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
        $KeepStage = $false
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
            $shim = Join-Path $BinDir 'rustelup.cmd'
            $previous = Join-Path $Stage 'previous'
            $previousShim = Join-Path $Stage 'previous.cmd'
            foreach ($file in @($dest, $shim)) {
                if (Test-Path -LiteralPath $file -PathType Container) {
                    err "$file is a directory"
                }
            }
            if (Test-Path -LiteralPath $dest) {
                Copy-Item -LiteralPath $dest -Destination $previous -Force
            }
            if (Test-Path -LiteralPath $shim) {
                Copy-Item -LiteralPath $shim -Destination $previousShim -Force
            }
            Write-UpdaterShim $Stage
            $updaterTouched = $false
            $shimTouched = $false
            try {
                $updaterTouched = $true
                if (-not (Move-InstalledFile $staged $dest)) { err "could not replace $dest" }
                Unblock-File -LiteralPath $dest -ErrorAction SilentlyContinue
                if (-not (Test-UpdaterHelp $dest)) { err 'installed updater failed its help check' }
                $shimTouched = $true
                if (-not (Move-InstalledFile (Join-Path $Stage 'rustelup.cmd') $shim)) {
                    err "could not replace $shim"
                }
            } catch {
                $installError = $_
                $restoreErrors = @()
                foreach ($item in @(
                    @{ Touched = $updaterTouched; Backup = $previous; Target = $dest },
                    @{ Touched = $shimTouched; Backup = $previousShim; Target = $shim }
                )) {
                    if (-not $item.Touched) { continue }
                    try {
                        if (Test-Path -LiteralPath $item.Backup) {
                            Copy-Item -LiteralPath $item.Backup -Destination $item.Target -Force
                        } elseif (Test-Path -LiteralPath $item.Target) {
                            Remove-Item -LiteralPath $item.Target -Force
                        }
                    } catch {
                        $restoreErrors += $item.Target
                    }
                }
                if ($restoreErrors.Count -gt 0) {
                    $KeepStage = $true
                    err "$installError; could not restore $($restoreErrors -join ', '); backups kept at $Stage"
                }
                throw $installError
            }
            say "installed rustelup to $dest"

            if (Add-UserPath $BinDir) {
                say "added $BinDir to PATH"
            }
            if ((Get-ExecutionPolicy) -eq 'Restricted') {
                say 'PowerShell is Restricted; run rustelup.cmd'
            }
        } finally {
            if ($Tmp -and (Test-Path -LiteralPath $Tmp)) {
                Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
            }
            if ($Stage -and -not $KeepStage -and (Test-Path -LiteralPath $Stage)) {
                Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
            }
        }

        say 'open a new terminal, then run: rustelup.cmd'
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $previousSecurityProtocol
    }
}
