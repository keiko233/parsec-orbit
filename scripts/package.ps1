# Stages the release files into dist\:
#
#   bcrypt.dll                        the loader, goes next to parsecd.exe
#   parsec-orbit-plugin-webview-bridge.zip         official plugin, provides parsec-orbit.webview
#                                                  (the webview patch below needs it)
#   parsec-orbit-plugin-hooks.zip                  official plugin, provides parsec-orbit.hooks (optional download)
#   parsec-orbit-plugin-config.zip                 official plugin, provides parsec-orbit.config (optional download)
#   parsec-orbit-plugin-js-runtime.zip             official plugin, runs kind = "js" plugins (optional download)
#   parsec-orbit-plugin-high-bitrate-webview.zip   official plugin, extract into <client>\plugins\
#   parsec-orbit-plugin-high-bitrate-native.zip    official plugin, same layout
#   parsec-orbit-plugin-stun-swap.zip  experimental plugin, same layout
#   parsec-orbit-plugin-hello.zip     example plugin, same layout
#   parsec-orbit-market.exe           the plugin market tool (verifies and installs)
#   index.json / index.json.sig       signed plugin index for the market tool
#   release.json / release.json.sig   signed distribution manifest (loader, tool,
#                                     index and which plugins are required/optional)
#   SHA256SUMS.txt
#
# Each zip contains the plugin folder itself, so extracting it into the
# client's plugins\ directory yields <plugins>\<plugin>\plugin.toml + the DLL.
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$SigningKeyFile = ''
)

$ErrorActionPreference = 'Stop'
$root = (Split-Path $PSScriptRoot -Parent).TrimEnd('\')
$dist = Join-Path $root 'dist'
$stage = Join-Path $root 'target\package'
$release = Join-Path $root 'target\release'

# The release manifest has to agree with the workspace version, exactly like
# the tag check in the Release workflow.
$versionMatch = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"' |
    Select-Object -First 1
if (-not $versionMatch) { throw 'could not read the workspace version from Cargo.toml' }
$workspaceVersion = $versionMatch.Matches[0].Groups[1].Value

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
            Name = 'parsec-orbit-plugin-stun-swap'
            Source = 'crates\parsec-orbit-plugin-stun-swap'
            Dll = 'parsec_orbit_plugin_stun_swap.dll'
        },
        @{
            Name = 'parsec-orbit-plugin-hello'
            Source = 'examples\parsec-orbit-plugin-hello'
            Dll = 'parsec_orbit_plugin_hello.dll'
        }
    )

    # The distribution manifest publishes what the installer offers, which is
    # not the same as "everything that builds". This table is the release
    # policy: required plugins cannot be deselected, default ones are offered
    # pre-selected, and group is only for the installer's prompt.
    $pluginPolicy = @(
        @{ Id = 'com.parsec-orbit.webview-bridge'; Required = $true;  Default = $true;  Group = 'WebView';    Note = 'Provides parsec-orbit.webview for scripts, evaluation and messages' },
        @{ Id = 'com.parsec-orbit.webview';         Required = $false; Default = $true;  Group = 'UI patches'; Note = 'Adds bandwidth limits above 50 Mbps to the WebView UI' },
        @{ Id = 'com.parsec-orbit.native';          Required = $false; Default = $true;  Group = 'UI patches'; Note = 'Adds bandwidth limits above 50 Mbps to the native UI' },
        @{ Id = 'com.parsec-orbit.config';          Required = $false; Default = $false; Group = 'Core';       Note = 'Provides parsec-orbit.config: settings that survive restarts' },
        @{ Id = 'com.parsec-orbit.js-runtime';      Required = $false; Default = $false; Group = 'Extensions'; Note = 'Runs JavaScript plugins (kind = "js") on Boa' },
        @{ Id = 'com.parsec-orbit.hooks';           Required = $false; Default = $false; Group = 'Extensions'; Note = 'Provides parsec-orbit.hooks: inline function hooks' },
        @{ Id = 'com.keiko233.stun-swap';           Required = $false; Default = $false; Group = 'Extensions'; Note = 'Redirects stun.parsec.app to mainland-China STUN servers' },
        @{ Id = 'com.example.hello';                Required = $false; Default = $false; Group = 'Examples';    Note = 'Example plugin that logs and writes a report file' }
    )

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $pluginIds = @{}
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

        $manifestText = Get-Content -LiteralPath (Join-Path $source 'plugin.toml') -Raw
        $idMatch = [regex]::Match($manifestText, '(?m)^\s*id\s*=\s*"([^"]+)"')
        if (-not $idMatch.Success) {
            throw "plugin.toml in $($plugin.Source) has no id"
        }
        $pluginIds[$plugin.Name] = $idMatch.Groups[1].Value
    }

    # The market tool ships with the release now: the network installer uses
    # it both to verify the signed manifests and to install the plugins.
    Copy-Item -LiteralPath (Join-Path $release 'parsec-orbit-market.exe') -Destination $dist
    $installerScript = Join-Path $root 'scripts\orbit-manager.ps1'
    if (-not (Test-Path -LiteralPath $installerScript)) {
        throw "missing $installerScript"
    }
    Copy-Item -LiteralPath $installerScript -Destination $dist

    $marketExe = Join-Path $release 'parsec-orbit-market.exe'
    $indexJson = Join-Path $dist 'index.json'
    & $marketExe index $dist --out $indexJson --name ("Parsec Orbit v" + $workspaceVersion)
    if ($LASTEXITCODE) { throw 'building index.json failed' }

    $abiMatch = Select-String -Path (Join-Path $root 'crates\parsec-orbit-plugin-abi\src\lib.rs') `
        -Pattern 'pub const ABI_VERSION: u32 = (\d+)' | Select-Object -First 1
    if (-not $abiMatch) { throw 'could not read ABI_VERSION' }
    $loaderApi = [int]$abiMatch.Matches[0].Groups[1].Value

    function Get-Sha256([string]$Path) {
        (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    }

    $built = Get-Content -LiteralPath $indexJson -Raw | ConvertFrom-Json
    $builtIds = @($built.plugins | ForEach-Object { $_.id })
    foreach ($policy in $pluginPolicy) {
        if ($builtIds -notcontains $policy.Id) {
            throw "plugin policy names $($policy.Id) but the release has no such package"
        }
    }
    foreach ($builtId in $builtIds) {
        $listed = $false
        foreach ($policy in $pluginPolicy) {
            if ($policy.Id -eq $builtId) { $listed = $true }
        }
        if (-not $listed) {
            throw "$builtId is packaged but missing from the installer plugin policy"
        }
    }

    $loaderPath = Join-Path $dist 'bcrypt.dll'
    $marketPath = Join-Path $dist 'parsec-orbit-market.exe'
    $releaseJson = Join-Path $dist 'release.json'
    $manifestPlugins = @(
        foreach ($policy in $pluginPolicy) {
            [ordered]@{
                id       = $policy.Id
                required = $policy.Required
                default  = $policy.Default
                group    = $policy.Group
                note     = $policy.Note
            }
        }
    )
    $releaseManifest = [ordered]@{
        format  = 1
        name    = 'Parsec Orbit'
        version = $workspaceVersion
        loader  = [ordered]@{
            url    = 'bcrypt.dll'
            sha256 = Get-Sha256 $loaderPath
            size   = (Get-Item -LiteralPath $loaderPath).Length
            api    = $loaderApi
        }
        market  = [ordered]@{
            url    = 'parsec-orbit-market.exe'
            sha256 = Get-Sha256 $marketPath
            size   = (Get-Item -LiteralPath $marketPath).Length
        }
        index   = [ordered]@{
            url    = 'index.json'
            sha256 = Get-Sha256 $indexJson
            size   = (Get-Item -LiteralPath $indexJson).Length
        }
        plugins = $manifestPlugins
    }
    ConvertTo-Json -InputObject ([pscustomobject]$releaseManifest) -Depth 8 |
        Set-Content -Encoding ascii -LiteralPath $releaseJson

    if ($SigningKeyFile) {
        if (-not (Test-Path -LiteralPath $SigningKeyFile)) {
            throw "signing key not found: $SigningKeyFile"
        }
        & $marketExe sign $releaseJson --key $SigningKeyFile
        if ($LASTEXITCODE) { throw 'signing release.json failed' }
        & $marketExe sign $indexJson --key $SigningKeyFile
        if ($LASTEXITCODE) { throw 'signing index.json failed' }
    } else {
        Write-Warning 'no signing key: release.json and index.json are unsigned'
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
