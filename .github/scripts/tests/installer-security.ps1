$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ('rustel-security-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $runner = Join-Path $work 'run.ps1'
    @'
param($Root, $Work, $Program, $Scenario)
$ErrorActionPreference = 'Stop'
$env:RUSTEL_DIR = Join-Path $Work 'installed'
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$UseBasicParsing, $Headers)
    if ($Uri -like '*/latest') {
        [IO.File]::WriteAllText($OutFile, '{"tag_name":"v1.2.3"}')
    } elseif ($Uri -like '*.sha256') {
        if ($Scenario -eq 'missing') { throw 'missing checksum' }
        $digest = '0' * 64
        if ($Scenario -eq 'invalid') { $digest = 'invalid' }
        [IO.File]::WriteAllText($OutFile, $digest)
    } else {
        [IO.File]::WriteAllText($OutFile, 'tampered download')
    }
}
function tar.exe { throw 'Extraction must not run before checksum verification' }
& (Join-Path $Root "rustelup/$Program") --version v1.2.3
exit $LASTEXITCODE
'@ | Set-Content -LiteralPath $runner -Encoding ascii
    $bin = Join-Path $work 'installed/bin'
    $lib = Join-Path $work 'installed/lib'
    New-Item -ItemType Directory -Path $bin, $lib | Out-Null
    $targets = @((Join-Path $bin 'rustel.exe'), (Join-Path $bin 'rustelup.cmd'), (Join-Path $lib 'rustelup.ps1'))
    foreach ($target in $targets) { [IO.File]::WriteAllText($target, 'previous installation') }
    foreach ($program in 'install.ps1', 'rustelup.ps1') {
        foreach ($scenario in 'tampered', 'invalid', 'missing') {
            $ErrorActionPreference = 'Continue'
            $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $runner $root $work $program $scenario 2>&1
            $ErrorActionPreference = 'Stop'
            if ($LASTEXITCODE -eq 0) { throw "$program accepted $scenario input" }
            if (($output | Out-String) -notmatch 'checksum') { throw "$program failed outside checksum verification: $output" }
            foreach ($target in $targets) {
                if ([IO.File]::ReadAllText($target) -ne 'previous installation') { throw 'Previous installation changed' }
            }
        }
    }
    Write-Output 'PowerShell installer security tests passed'
} finally {
    $resolved = [IO.Path]::GetFullPath($work)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
# A CI step exits with the last native exit code, and the child installers
# above fail on purpose. Own the outcome.
exit 0
