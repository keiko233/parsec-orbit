# Verifies that the loader forwards every call to the real bcrypt.dll
# unchanged.
#
#   callpaths  four ways of calling one export, plus the argument registers
#              the real function receives (needs the diagnostics feature)
#   roundtrip  SHA-256 / HMAC-SHA256 known-answer vectors through the loader
[CmdletBinding()]
param(
    [string]$Loader
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
if (-not $Loader) {
    $Loader = Join-Path $root 'target\release\bcrypt.dll'
}

Push-Location $root
try {
    Write-Host 'building with the diagnostics feature:'
    Write-Host '  cargo build --release -p parsec-orbit-loader-windows --features diagnostics'
    cargo build --release -p parsec-orbit-loader-windows --features diagnostics | Out-Host
    if ($LASTEXITCODE) { throw 'cargo build failed' }
    if (-not (Test-Path $Loader)) { throw "Missing $Loader" }

    Write-Host ''
    Write-Host '== callpaths =='
    & (Join-Path $root 'target\release\callpaths.exe') $Loader
    if ($LASTEXITCODE) { throw 'callpaths failed' }

    Write-Host ''
    Write-Host '== roundtrip =='
    & (Join-Path $root 'target\release\roundtrip.exe') $Loader
    if ($LASTEXITCODE) { throw 'roundtrip failed' }

    # Restore the default-feature build so target\release\bcrypt.dll is the
    # artifact that goes into a release.
    Write-Host ''
    Write-Host 'rebuilding without diagnostics:'
    cargo build --release -p parsec-orbit-loader-windows | Out-Host
    if ($LASTEXITCODE) { throw 'cargo build failed' }
} finally {
    Pop-Location
}

Write-Host ''
Write-Host 'all checks passed'
