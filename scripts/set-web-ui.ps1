# Switches the client between its native UI and the WebView2 UI by writing
# client_web_ui into %APPDATA%\Parsec\config.json.
#
# Takes effect on the next client start. Together with the loader (which opens
# the WebView2 debugging port) the UI can then be inspected at
# http://127.0.0.1:9222/json.
[CmdletBinding()]
param(
    [switch]$Off,
    [string]$ConfigPath = (Join-Path $env:APPDATA 'Parsec\config.json')
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $ConfigPath)) {
    throw "Config file not found: $ConfigPath"
}

$json = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
if ($json.Count -lt 2 -or $null -eq $json[1]) {
    throw "$ConfigPath is not the expected [comment, settings] array; aborting to avoid damaging it."
}

$settings = $json[1]
$value = -not $Off

if ($settings.PSObject.Properties.Name -contains 'client_web_ui') {
    $settings.client_web_ui.value = $value
} else {
    $settings | Add-Member -NotePropertyName 'client_web_ui' `
        -NotePropertyValue ([pscustomobject]@{ value = $value })
}

$backup = "$ConfigPath.parsec-orbit-$(Get-Date -Format 'yyyyMMdd-HHmmss').bak"
Copy-Item -LiteralPath $ConfigPath -Destination $backup -Force

# Write UTF-8 without a BOM: the client rewrites this file itself and should
# never see a byte order mark.
$text = ConvertTo-Json -InputObject $json -Depth 10
[System.IO.File]::WriteAllText($ConfigPath, $text, [System.Text.UTF8Encoding]::new($false))

Write-Host ("client_web_ui = {0}" -f $value)
Write-Host "backup         : $backup"
Write-Host 'Restart the client to apply. %APPDATA%\Parsec\log_cl.txt reports enable_webview=1 for the WebView2 UI.'
if ($value) {
    Write-Host 'With the loader installed, inspect it at http://127.0.0.1:9222/json'
}
