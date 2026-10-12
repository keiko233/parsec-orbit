# Installs the loader and the official plugins into the Parsec client
# directory. The script stops the Parsec service (which ends any running
# client session), replaces bcrypt.dll, deploys the plugin folders and starts
# the service again.
#
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1 [-EnableWebViewUi]
#
# It has to run elevated: it writes into the client directory and controls the
# Parsec service. The previous bcrypt.dll is copied to
# %APPDATA%\ParsecOrbit\backups\ before it is replaced.
[CmdletBinding()]
param(
    [string]$ClientDir = 'C:\Program Files\Parsec',
    [string]$ServiceName = 'Parsec',
    [switch]$EnableWebViewUi
)

$ErrorActionPreference = 'Stop'

$script:logPath = Join-Path $env:TEMP 'parsec-orbit-install.log'
function Write-Log([string]$Message) {
    $line = '[{0}] {1}' -f (Get-Date -Format 'HH:mm:ss'), $Message
    Write-Host $line
    Add-Content -LiteralPath $script:logPath -Value $line -Encoding utf8
}

$root = Split-Path $PSScriptRoot -Parent
$release = Join-Path $root 'target\release'

Set-Content -LiteralPath $script:logPath -Encoding utf8 -Value @(
    "Parsec Orbit install $([char]0x2014) $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
)

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'this script has to run from an elevated prompt'
}
Write-Log "elevated as $($identity.Name)"

$loader = Join-Path $release 'bcrypt.dll'
if (-not (Test-Path -LiteralPath $loader)) {
    throw "missing $loader - build it first (cargo build --release, or scripts\package.ps1)"
}

$plugins = @(
    @{
        Name = 'parsec-orbit-plugin-high-bitrate-webview'
        Source = Join-Path $root 'plugins\parsec-orbit-plugin-high-bitrate-webview'
        Dll = 'parsec_orbit_plugin_high_bitrate_webview.dll'
    },
    @{
        Name = 'parsec-orbit-plugin-high-bitrate-native'
        Source = Join-Path $root 'plugins\parsec-orbit-plugin-high-bitrate-native'
        Dll = 'parsec_orbit_plugin_high_bitrate_native.dll'
    }
)
foreach ($plugin in $plugins) {
    $dll = Join-Path $release $plugin.Dll
    if (-not (Test-Path -LiteralPath $dll)) {
        throw "missing $dll - build it first (cargo build --release)"
    }
}

# The client keeps bcrypt.dll mapped, so it has to go before the copy. The
# service hosts the client and restarts it on its own, hence stopping it first.
$service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($service -and $service.Status -ne 'Stopped') {
    Stop-Service -Name $ServiceName -Force
    (Get-Service -Name $ServiceName).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    Write-Log "service $ServiceName stopped"
}

foreach ($process in @(Get-Process parsecd -ErrorAction SilentlyContinue)) {
    Write-Log "stopping parsecd pid $($process.Id)"
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
}
$deadline = (Get-Date).AddSeconds(20)
while ((Get-Process parsecd -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
}
if (Get-Process parsecd -ErrorAction SilentlyContinue) {
    throw 'parsecd.exe is still running'
}

$target = Join-Path $ClientDir 'bcrypt.dll'
$backup = Join-Path $env:APPDATA ('ParsecOrbit\backups\bcrypt.dll.{0}.bak' -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force -Path (Split-Path $backup -Parent) | Out-Null
if (Test-Path -LiteralPath $target) {
    Copy-Item -LiteralPath $target -Destination $backup -Force
    Write-Log "previous bcrypt.dll backed up to $backup"
}

$copied = $false
for ($attempt = 1; $attempt -le 20 -and -not $copied; $attempt++) {
    try {
        Copy-Item -LiteralPath $loader -Destination $target -Force
        $copied = $true
    } catch {
        if ($attempt -eq 20) { throw }
        Start-Sleep -Milliseconds 500
    }
}
Write-Log "installed $(Split-Path $loader -Leaf) -> $target"

foreach ($plugin in $plugins) {
    $dir = Join-Path $ClientDir ('plugins\{0}' -f $plugin.Name)
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item -LiteralPath (Join-Path $plugin.Source 'plugin.toml') -Destination $dir -Force
    $readme = Join-Path $plugin.Source 'README.md'
    if (Test-Path -LiteralPath $readme) {
        Copy-Item -LiteralPath $readme -Destination $dir -Force
    }
    Copy-Item -LiteralPath (Join-Path $release $plugin.Dll) -Destination $dir -Force
    Write-Log "installed $($plugin.Name)"
}

if ($EnableWebViewUi) {
    & (Join-Path $root 'scripts\set-web-ui.ps1') | ForEach-Object { Write-Log $_ }
}

if ($service) {
    Start-Service -Name $ServiceName
    Write-Log "service $ServiceName started"
}

$client = $null
$deadline = (Get-Date).AddSeconds(30)
do {
    Start-Sleep -Milliseconds 500
    $client = @(Get-Process parsecd -ErrorAction SilentlyContinue)
} while (-not $client -and (Get-Date) -lt $deadline)

if ($client) {
    Write-Log "client running: pid $(($client.Id) -join ', ')"
    Start-Sleep -Seconds 3
    $loaderLog = Join-Path $env:APPDATA 'ParsecOrbit\parsec-orbit.log'
    if (Test-Path -LiteralPath $loaderLog) {
        Write-Log '--- loader log (tail) ---'
        foreach ($line in (Get-Content -LiteralPath $loaderLog -Tail 30)) {
            Add-Content -LiteralPath $script:logPath -Value $line -Encoding utf8
            Write-Host $line
        }
    }
} else {
    Write-Log 'no client process after 30 s; open Parsec from the Start menu'
}
