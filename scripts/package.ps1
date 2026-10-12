# Stages the release files into dist\:
#
#   bcrypt.dll                        the loader, goes next to parsecd.exe
#   parsec-orbit-market.exe           finds, checks and installs plugins (see docs/spec/marketplace.md)
#   parsec-orbit-plugin-webview-bridge.zip         official plugin, provides parsec-orbit.webview
#                                                  (the webview patch below needs it)
#   parsec-orbit-plugin-hooks.zip                  official plugin, provides parsec-orbit.hooks (optional download)
#   parsec-orbit-plugin-config.zip                 official plugin, provides parsec-orbit.config (optional download)
#   parsec-orbit-plugin-js-runtime.zip             official plugin, runs kind = "js" plugins (optional download)
#   parsec-orbit-plugin-high-bitrate-webview.zip   official plugin, extract into <client>\plugins\
#   parsec-orbit-plugin-high-bitrate-native.zip    official plugin, same layout
#   parsec-orbit-plugin-hello.zip     example plugin, same layout
#   SHA256SUMS.txt
#
# Each zip contains the plugin folder itself, so extracting it into the
# client's plugins\ directory yields <plugins>\<plugin>\plugin.toml + the DLL.
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = (Split-Path $PSScriptRoot -Parent).TrimEnd('\')
$dist = Join-Path $root 'dist'
$stage = Join-Path $root 'target\package'
$release = Join-Path $root 'target\release'

Push-Location $root
try {
    if (-not $SkipBuild) {
        Write-Host 'cargo build --release --locked'
        cargo build --release --locked | Out-Host
        if ($LASTEXITCODE) { throw 'cargo build failed' }
    }

    foreach ($path in @($dist, $stage)) {
        $full = [System.IO.Path]::GetFullPath($path)
        if (-not $full.StartsWith($root + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "refusing to clean ${full}: outside $root"
        }
        if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $full | Out-Null
    }

    Copy-Item -LiteralPath (Join-Path $release 'bcrypt.dll') -Destination $dist
    Copy-Item -LiteralPath (Join-Path $release 'parsec-orbit-market.exe') -Destination $dist

    $plugins = @(
        @{
            Name = 'parsec-orbit-plugin-hooks'
            Source = 'crates\parsec-orbit-plugin-hooks'
            Dll = 'parsec_orbit_plugin_hooks.dll'
        },
        @{
            Name = 'parsec-orbit-plugin-config'
            Source = 'crates\parsec-orbit-plugin-config'
            Dll = 'parsec_orbit_plugin_config.dll'
        },
        @{
            Name = 'parsec-orbit-plugin-js-runtime'
            Source = 'crates\parsec-orbit-plugin-js-runtime'
            Dll = 'parsec_orbit_plugin_js_runtime.dll'
        },
        @{
            Name = 'parsec-orbit-plugin-webview-bridge'
            Source = 'crates\parsec-orbit-plugin-webview-bridge'
            Dll = 'parsec_orbit_plugin_webview_bridge.dll'
        },
        @{
            Name = 'parsec-orbit-plugin-high-bitrate-webview'
            Source = 'crates\parsec-orbit-plugin-high-bitrate-webview'
            Dll = 'parsec_orbit_plugin_high_bitrate_webview.dll'
        },
        @{
            Name = 'parsec-orbit-plugin-high-bitrate-native'
            Source = 'crates\parsec-orbit-plugin-high-bitrate-native'
            Dll = 'parsec_orbit_plugin_high_bitrate_native.dll'
        },
        @{
            Name = 'parsec-orbit-plugin-hello'
            Source = 'examples\parsec-orbit-plugin-hello'
            Dll = 'parsec_orbit_plugin_hello.dll'
        }
    )

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    foreach ($plugin in $plugins) {
        $source = Join-Path $root $plugin.Source
        $folder = Join-Path $stage $plugin.Name
        New-Item -ItemType Directory -Force -Path $folder | Out-Null
        Copy-Item -LiteralPath (Join-Path $source 'plugin.toml') -Destination $folder
        Get-ChildItem -LiteralPath $source -Filter '*.md' -File | Copy-Item -Destination $folder
        Copy-Item -LiteralPath (Join-Path $release $plugin.Dll) -Destination $folder

        $zip = Join-Path $dist ($plugin.Name + '.zip')
        # Entry names are written by hand with forward slashes: Windows
        # PowerShell's CreateFromDirectory uses backslashes, which the plugin
        # market (and every other zip tool) rejects or misreads.
        $archive = [System.IO.Compression.ZipFile]::Open($zip, [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($file in (Get-ChildItem -LiteralPath $folder -Recurse -File | Sort-Object FullName)) {
                $relative = $file.FullName.Substring($folder.Length).TrimStart('\', '/').Replace('\', '/')
                [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                    $archive,
                    $file.FullName,
                    ($plugin.Name + '/' + $relative),
                    [System.IO.Compression.CompressionLevel]::Optimal
                )
            }
        }
        finally {
            $archive.Dispose()
        }
    }

    Get-ChildItem -LiteralPath $dist -File | Sort-Object Name | ForEach-Object {
        '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
    } | Set-Content -Encoding ascii (Join-Path $dist 'SHA256SUMS.txt')

    Write-Host ''
    Get-ChildItem -LiteralPath $dist | Format-Table Name, Length | Out-Host
    Write-Host 'release files staged in dist\'
} finally {
    Pop-Location
}
