# Installs, upgrades, uninstalls and manages Parsec Orbit from a signed
# release manifest. This is the network counterpart of scripts\install.ps1:
# it downloads bcrypt.dll, the market tool and the market index, verifies the
# signatures and package digests, then performs the same client changes.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 install
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 upgrade
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 uninstall
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 plugins list
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 plugins install <id...>
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 plugins remove <id...>
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 plugins update [id...]
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 status
#   powershell -ExecutionPolicy Bypass -File scripts\orbit-manager.ps1 help
#
# Options:
#   -Version <semver>        install a pinned release (v<version>) instead of latest
#   -Repo <owner/name>       GitHub repository (default: keiko233/parsec-orbit)
#   -ManifestUrl <url|path>  override the release.json location (local testing)
#   -ClientDir <path>        Parsec client directory
#   -ServiceName <name>      Parsec Windows service
#   -Plugin <ids|required|default|all|none>
#                            repeatable plugin selection; explicit ids always
#                            include every required plugin
#   -WebViewUi <Ask|Enable|Disable|Keep>
#   -Yes                     non-interactive confirmation
#   -AllowUnsigned           proceed without manifest/index signatures
#   -TrustKey <line>         append an `ed25519 <base64 key> [comment]` line
#   -Keys <file>             trusted keys file
#   -Force                   override safety checks
#   -NoRestart               do not restart the Parsec service
#   -DryRun                  print the plan only; no elevation required
#   -RemoveCache             uninstall: also delete downloads and the cached tool
#   -KeepPlugins / -RemovePlugins          uninstall plugin choice
#   -KeepWebViewUi / -RestoreWebViewUi     uninstall webview choice
#
# The script also works when the text is piped through Invoke-Expression; in
# that mode an empty argument list installs the latest release. It runs on
# Windows PowerShell 5.1 and needs no modules.

$ErrorActionPreference = 'Stop'

# PowerShell 5.1 does not negotiate TLS 1.2 in every .NET configuration.
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Older service points may not expose the property; HttpWebRequest still
    # negotiates supported protocols on current .NET Framework updates.
}

$script:OfficialEd25519PublicKey = '2LCW/gbjU6USD1CX5myen6o6Y0E/NN+KFjsRiSV7Sbs='
$script:StateFormat = 1
$script:MaximumPackageSize = 268435456
$script:SemverPattern = '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$'
$script:HexPattern = '^[a-f0-9]{64}$'
$script:IdPattern = '^[A-Za-z0-9._-]{1,128}$'

$script:LogPath = $null
$script:WorkDir = $null
$script:ToolDir = $null
$script:BackupDir = $null
$script:StatePath = $null
$script:KeysPath = $null
$script:ScriptPath = $null

function Write-Log([string]$Message) {
    $line = '[{0}] {1}' -f (Get-Date -Format 'HH:mm:ss'), $Message
    Write-Host $line
    if ($script:LogPath) {
        Add-Content -LiteralPath $script:LogPath -Value $line -Encoding utf8
    }
}

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Test-Interactive {
    try {
        return -not [Console]::IsInputRedirected
    } catch {
        return $false
    }
}

function Test-Url([string]$Location) {
    return $Location.StartsWith('http://', [System.StringComparison]::OrdinalIgnoreCase) -or
        $Location.StartsWith('https://', [System.StringComparison]::OrdinalIgnoreCase)
}

function Get-AppData {
    if ($env:APPDATA) { return $env:APPDATA }
    if ($env:USERPROFILE) { return (Join-Path $env:USERPROFILE 'AppData\Roaming') }
    throw 'neither APPDATA nor USERPROFILE is set'
}

function Get-LocalAppData {
    if ($env:LOCALAPPDATA) { return $env:LOCALAPPDATA }
    if ($env:USERPROFILE) { return (Join-Path $env:USERPROFILE 'AppData\Local') }
    throw 'neither LOCALAPPDATA nor USERPROFILE is set'
}

function Test-AbsolutePath([string]$Location) {
    if ($Location.StartsWith('\') -or $Location.StartsWith('/')) { return $true }
    return $Location.Length -ge 2 -and $Location[1] -eq ':' -and
        ($Location[0] -ge 'A' -and $Location[0] -le 'Z' -or $Location[0] -ge 'a' -and $Location[0] -le 'z')
}

function Test-LocalLocation([string]$Location) {
    if ($Location.StartsWith('file://', [System.StringComparison]::OrdinalIgnoreCase)) { return $true }
    return $Location.StartsWith('\\') -or $Location.StartsWith('//') -or (Test-AbsolutePath $Location)
}

function Assert-NoParentReference([string]$Location) {
    $parts = @($Location.Split(@('/', '\')))
    if ($parts -contains '..') {
        throw "'$Location' climbs out of the manifest folder"
    }
}

function Convert-FileUrlToLocal([string]$Location) {
    if ($Location.StartsWith('file:///', [System.StringComparison]::OrdinalIgnoreCase)) {
        return $Location.Substring(8)
    }
    return $Location
}

function Resolve-OrbitUrl([string]$Base, [string]$Relative) {
    if ([string]::IsNullOrWhiteSpace($Relative)) {
        throw 'manifest entries need a non-empty url'
    }
    if (Test-Url $Base) {
        if (Test-Url $Relative) { return $Relative }
        if (Test-LocalLocation $Relative) {
            throw "'$Relative' in a remote manifest is not an http(s) location"
        }
        Assert-NoParentReference $Relative
        $cut = $Base.LastIndexOf('/')
        if ($cut -lt 0) {
            $prefix = $Base
        } else {
            $prefix = $Base.Substring(0, $cut + 1)
        }
        $trimmed = $Relative
        while ($trimmed.StartsWith('./')) { $trimmed = $trimmed.Substring(2) }
        return $prefix + $trimmed
    }

    if (Test-Url $Relative) { return $Relative }
    if ($Relative.StartsWith('file://', [System.StringComparison]::OrdinalIgnoreCase)) { return $Relative }
    if (Test-AbsolutePath $Relative) { return $Relative }
    Assert-NoParentReference $Relative
    $baseLocal = Convert-FileUrlToLocal $Base
    $parent = [System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($baseLocal))
    if (-not $parent) { return $Relative }
    return (Join-Path -Path $parent -ChildPath $Relative)
}

function Get-BytesSha256([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha.Dispose()
    }
}

function Unblock-OrbitFile([string]$Path) {
    if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
        Unblock-File -LiteralPath $Path -ErrorAction SilentlyContinue
    }
}

function Get-OrbitFile {
    param(
        [string]$Source,
        [string]$Destination,
        [string]$ExpectedSha256 = '',
        [long]$ExpectedSize = 0,
        [long]$Limit = 1048576,
        [string]$Description
    )
    if ($ExpectedSize -gt 0) { $Limit = $ExpectedSize }
    $bytes = $null
    if (Test-Url $Source) {
        $request = [System.Net.HttpWebRequest]::Create($Source)
        $request.UserAgent = 'ParsecOrbitSetup/1'
        $request.AllowAutoRedirect = $true
        $request.Timeout = 120000
        $request.ReadWriteTimeout = 120000
        $request.Expect = ''
        $response = $null
        try {
            $response = $request.GetResponse()
            if ($response.ContentLength -ge 0 -and $response.ContentLength -gt $Limit) {
                throw "$Source is larger than $Limit bytes"
            }
            $stream = $response.GetResponseStream()
            $memory = New-Object System.IO.MemoryStream
            $buffer = New-Object byte[] 65536
            try {
                while ($true) {
                    $read = $stream.Read($buffer, 0, $buffer.Length)
                    if ($read -le 0) { break }
                    [void]$memory.Write($buffer, 0, $read)
                    if ($memory.Length -gt $Limit) {
                        throw "$Source is larger than $Limit bytes"
                    }
                }
                $bytes = $memory.ToArray()
            } finally {
                $memory.Dispose()
            }
        } finally {
            if ($response) { $response.Close() }
        }
    } else {
        $local = Convert-FileUrlToLocal $Source
        $local = [System.IO.Path]::GetFullPath($local)
        $info = Get-Item -LiteralPath $local -ErrorAction SilentlyContinue
        if (-not $info -or $info.PSIsContainer) {
            throw "${Source}: not found"
        }
        if ($info.Length -gt $Limit) {
            throw "$Source is larger than $Limit bytes"
        }
        $bytes = [System.IO.File]::ReadAllBytes($local)
    }

    if ($bytes.Length -gt $Limit) {
        throw "$Source is larger than $Limit bytes"
    }
    if ($ExpectedSize -gt 0 -and $bytes.Length -ne $ExpectedSize) {
        throw "$Description has the wrong size (expected $ExpectedSize, got $($bytes.Length))"
    }
    if ($ExpectedSha256) {
        $hash = Get-BytesSha256 $bytes
        if ($hash -ne $ExpectedSha256.ToLowerInvariant()) {
            throw "$Description sha256 mismatch (expected $ExpectedSha256, got $hash)"
        }
    }

    $tmp = "$Destination.tmp"
    if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }
    [System.IO.File]::WriteAllBytes($tmp, $bytes)
    $target = $Destination
    if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Force }
    Move-Item -LiteralPath $tmp -Destination $target -Force
    Unblock-OrbitFile $target
    return $target
}

function Get-JsonProp([object]$Object, [string]$Name) {
    if ($null -eq $Object) { return $null }
    $prop = $Object.PSObject.Properties[$Name]
    if ($null -eq $prop) { return $null }
    return $prop.Value
}

function Require([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Compare-Semver([string]$Left, [string]$Right) {
    # Numeric dotted comparison; prerelease sorts below the release, build
    # metadata is ignored. Returns -1 (Left older), 0 or 1.
    if ($Left -eq $Right) { return 0 }
    $leftParts = $Left.Split('-')[0].Split('+')[0].Split('.')
    $rightParts = $Right.Split('-')[0].Split('+')[0].Split('.')
    $count = [Math]::Max($leftParts.Count, $rightParts.Count)
    for ($i = 0; $i -lt $count; $i++) {
        $l = if ($i -lt $leftParts.Count) { [int]$leftParts[$i] } else { 0 }
        $r = if ($i -lt $rightParts.Count) { [int]$rightParts[$i] } else { 0 }
        if ($l -lt $r) { return -1 }
        if ($l -gt $r) { return 1 }
    }
    $leftPre = $null
    $rightPre = $null
    if ($Left.Contains('-')) { $leftPre = $Left.Split('-')[1].Split('+')[0] }
    if ($Right.Contains('-')) { $rightPre = $Right.Split('-')[1].Split('+')[0] }
    if (-not $leftPre -and -not $rightPre) { return 0 }
    if (-not $leftPre) { return 1 }
    if (-not $rightPre) { return -1 }
    return 0
}

function Invoke-MarketRaw {
    param([string]$Executable, [string[]]$Arguments)
    try {
        # Native stderr becomes an error record under 2>&1. Keep Stop from
        # turning ordinary tool diagnostics into a terminating exception.
        $oldPreference = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $output = & $Executable @Arguments 2>&1
        } finally {
            $ErrorActionPreference = $oldPreference
        }
        $exitCode = $LASTEXITCODE
        $lines = foreach ($item in @($output)) { [string]$item }
        return @{ ExitCode = $exitCode; Output = @($lines) }
    } catch {
        return @{ ExitCode = -1; Output = @($_.Exception.Message) }
    }
}

function Invoke-Market {
    param([string]$Executable, [string[]]$Arguments)
    $result = Invoke-MarketRaw -Executable $Executable -Arguments $Arguments
    foreach ($line in $result.Output) { Write-Log "market: $line" }
    if ($result.ExitCode -ne 0) {
        $details = ($result.Output | Where-Object { $_ }) -join '; '
        throw "market tool failed (exit $($result.ExitCode)): $details"
    }
    return $result.Output
}

function Stop-Parsec {
    param([string]$ServiceName)
    $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($service -and $service.Status -ne 'Stopped') {
        Stop-Service -Name $ServiceName -Force
        (Get-Service -Name $ServiceName).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        Write-Log "service $ServiceName stopped"
    } elseif (-not $service) {
        Write-Log "service $ServiceName not present"
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
}

function Start-Parsec {
    param([string]$ServiceName)
    $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if (-not $service) {
        Write-Log "service $ServiceName not present"
        return
    }
    Start-Service -Name $ServiceName
    Write-Log "service $ServiceName started"
}

function Show-LoaderLog {
    $loaderLog = Join-Path $env:APPDATA 'ParsecOrbit\parsec-orbit.log'
    if (-not (Test-Path -LiteralPath $loaderLog)) { return }
    Write-Log '--- loader log (tail) ---'
    foreach ($line in @(Get-Content -LiteralPath $loaderLog -Tail 30)) {
        Add-Content -LiteralPath $script:LogPath -Value $line -Encoding utf8
        Write-Host $line
    }
}

function Read-State {
    if (-not (Test-Path -LiteralPath $script:StatePath)) { return $null }
    try {
        $text = [System.IO.File]::ReadAllText($script:StatePath)
        $json = $text | ConvertFrom-Json
        $format = Get-JsonProp $json 'format'
        if ($null -eq $format -or $format -ne $script:StateFormat) {
            Write-Log "unsupported install state format in $($script:StatePath)"
            return $null
        }
        return $json
    } catch {
        Write-Log "install state is not readable: $($_.Exception.Message)"
        return $null
    }
}

function Write-State([hashtable]$State) {
    $text = ConvertTo-Json -InputObject $State -Depth 8
    $tmp = "$($script:StatePath).tmp"
    [System.IO.File]::WriteAllText($tmp, $text, [System.Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $tmp -Destination $script:StatePath -Force
}

function Read-WebViewSetting([string]$ConfigPath) {
    if (-not (Test-Path -LiteralPath $ConfigPath)) {
        return @{ Existed = $false; Value = $null }
    }
    $json = [System.IO.File]::ReadAllText($ConfigPath) | ConvertFrom-Json
    if (@($json).Count -lt 2 -or $null -eq $json[1]) {
        throw "$ConfigPath is not the expected [comment, settings] array; aborting to avoid damaging it."
    }
    $settings = $json[1]
    if ($settings.PSObject.Properties.Name -contains 'client_web_ui') {
        $current = $settings.PSObject.Properties['client_web_ui'].Value
        if ($current -is [bool]) {
            return @{ Existed = $true; Value = $current }
        }
        if ($current -and $current.PSObject.Properties.Name -contains 'value') {
            $value = $current.PSObject.Properties['value'].Value
            if ($value -is [bool]) {
                return @{ Existed = $true; Value = $value }
            }
        }
        throw "$ConfigPath has an unexpected client_web_ui shape; aborting to avoid damaging it."
    }
    return @{ Existed = $false; Value = $null }
}

function Set-WebViewSetting {
    param([string]$ConfigPath, [bool]$Value, [switch]$Remove)
    $json = [System.IO.File]::ReadAllText($ConfigPath) | ConvertFrom-Json
    if (@($json).Count -lt 2 -or $null -eq $json[1]) {
        throw "$ConfigPath is not the expected [comment, settings] array; aborting to avoid damaging it."
    }
    $settings = $json[1]
    if ($settings.PSObject.Properties.Name -contains 'client_web_ui') {
        $current = $settings.PSObject.Properties['client_web_ui']
        if ($Remove) {
            $settings.PSObject.Properties.Remove('client_web_ui')
        } elseif ($current.Value -is [bool]) {
            $current.Value = $Value
        } elseif ($current.Value -and $current.Value.PSObject.Properties.Name -contains 'value') {
            $current.Value.value = $Value
        } else {
            throw "$ConfigPath has an unexpected client_web_ui shape; aborting to avoid damaging it."
        }
    } elseif (-not $Remove) {
        $settings | Add-Member -NotePropertyName 'client_web_ui' `
            -NotePropertyValue ([pscustomobject]@{ value = $Value })
    } else {
        return $null
    }
    $backup = "$ConfigPath.parsec-orbit-$(Get-Date -Format 'yyyyMMdd-HHmmss').bak"
    Copy-Item -LiteralPath $ConfigPath -Destination $backup -Force
    # The client rewrites the file itself, so UTF-8 without BOM matters.
    $text = ConvertTo-Json -InputObject $json -Depth 10
    [System.IO.File]::WriteAllText($ConfigPath, $text, [System.Text.UTF8Encoding]::new($false))
    Write-Log "updated client_web_ui in $ConfigPath (backup: $backup)"
}

function Validate-TrustedKeyLine([string]$Line) {
    if ($Line -notmatch '^ed25519\s+(\S+)') { throw "not an ed25519 key line: $Line" }
    $key = [regex]::Match($Line, '^ed25519\s+(\S+)').Groups[1].Value
    try {
        $bytes = [Convert]::FromBase64String($key)
        if ($bytes.Length -ne 32) { throw 'wrong length' }
    } catch {
        throw "not a base64 ed25519 public key: $key"
    }
}

function Ensure-TrustedKeys {
    param([hashtable]$Options, [string[]]$TrustedKeyLines)
    $keysPath = $Options.Keys
    $directory = [System.IO.Path]::GetDirectoryName($keysPath)
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    $existing = ''
    if (Test-Path -LiteralPath $keysPath) {
        $existing = [System.IO.File]::ReadAllText($keysPath)
    }
    if ([string]::IsNullOrWhiteSpace($existing) -and -not $TrustedKeyLines) {
        if ($script:OfficialEd25519PublicKey) {
            Write-Log 'no trusted keys yet; adding the embedded official key'
            Validate-TrustedKeyLine "ed25519 $($script:OfficialEd25519PublicKey)"
            $existing = "ed25519 $($script:OfficialEd25519PublicKey)`n"
        } elseif (Test-Interactive) {
            $line = Read-Host "paste an ed25519 key line (ed25519 <base64 public key>) or press Enter to cancel"
            if (-not $line) { throw 'no trusted keys: pass -TrustKey' }
            Validate-TrustedKeyLine $line
            $existing = "$line`n"
        } else {
            throw "no trusted keys; pass -TrustKey 'ed25519 <base64 key>'"
        }
    }
    $lines = @($existing -split "`r?`n" | Where-Object { $_ })
    foreach ($line in $TrustedKeyLines) {
        Validate-TrustedKeyLine $line
        $keyPart = ([regex]::Match($line, '^ed25519\s+(\S+)')).Groups[1].Value
        $duplicate = $false
        foreach ($current in $lines) {
            if ($current -match '^ed25519\s+(\S+)' -and $Matches[1] -eq $keyPart) {
                $duplicate = $true
                break
            }
        }
        if ($duplicate) {
            Write-Log "trusted key already present: $keyPart"
            continue
        }
        Write-Log "appending key to $keysPath : $line"
        $lines += $line
    }
    if ($lines.Count -eq 0) { throw "no trusted keys in $keysPath" }
    [System.IO.File]::WriteAllText($keysPath, (($lines | ForEach-Object { $_ }) -join "`n") + "`n", [System.Text.UTF8Encoding]::new($false))
}

function Write-Usage {
    Write-Host $script:UsageText
}

$script:UsageText = @'
Usage: orbit-manager <command> [options]

Commands:
  install                  install Parsec Orbit (latest or pinned release)
  upgrade                  upgrade an existing install
  uninstall                remove Parsec Orbit and restore the original loader
  plugins list             show market index and installed plugins
  plugins install <id...>  install plugins
  plugins remove <id...>   remove plugins
  plugins update [id...]   update plugins
  status                   show install state and latest release version
  help                     show this text

Options:
  -Version <semver>        pin release v<version> (default: latest)
  -Repo <owner/name>       GitHub repository (default: keiko233/parsec-orbit)
  -ManifestUrl <url|path>  release.json override (offline/local testing)
  -ClientDir <path>        Parsec client directory (default: C:\Program Files\Parsec)
  -ServiceName <name>      Parsec service (default: Parsec)
  -Plugin <id|required|default|all|none>
  -WebViewUi <Ask|Enable|Disable|Keep>
  -Yes                     non-interactive confirmation
  -AllowUnsigned           skip manifest/index signature checks
  -TrustKey <line>         add an ed25519 trusted key line
  -Keys <file>             trusted keys file
  -Force                   override safety checks
  -NoRestart               do not restart the Parsec service
  -DryRun                  print the plan only
  -RemoveCache             uninstall: also remove downloads and cached tool
  -KeepPlugins             uninstall: keep installed plugins
  -RemovePlugins           uninstall: remove all managed plugins
  -KeepWebViewUi           uninstall: keep client_web_ui
  -RestoreWebViewUi        uninstall: restore the previous client_web_ui
'@

function Get-Option {
    param([string[]]$Arguments)
    $options = @{
        Version = ''
        Repo = 'keiko233/parsec-orbit'
        ManifestUrl = ''
        ClientDir = 'C:\Program Files\Parsec'
        ServiceName = 'Parsec'
        Plugin = @()
        WebViewUi = 'Ask'
        Yes = $false
        AllowUnsigned = $false
        TrustKey = @()
        Keys = Join-Path $env:APPDATA 'ParsecOrbit\market\trusted-keys.txt'
        Force = $false
        NoRestart = $false
        DryRun = $false
        RemoveCache = $false
        KeepPlugins = $false
        RemovePlugins = $false
        KeepWebViewUi = $false
        RestoreWebViewUi = $false
    }
    $i = 0
    while ($i -lt $Arguments.Count) {
        $token = [string]$Arguments[$i]
        if (-not $token.StartsWith('-')) {
            if (-not $options.PositionalIds) { $options.PositionalIds = @() }
            $options.PositionalIds += $token
            $i++
            continue
        }
        $name = $token.TrimStart('-')
        if ($name -eq '' -or $name -eq 'h' -or $name -eq 'help') {
            if ($name -eq '') { throw "unrecognized option: $token" }
            $options.Help = $true
            $i++
            continue
        }
        $hasValue = $name -match '^(?<n>[^=]+)=(?<v>.*)$'
        $value = $null
        if ($hasValue) {
            $name = $Matches['n']
            $value = $Matches['v']
        }
        $switches = @('yes','allowunsigned','force','norestart','dryrun','removecache','keepplugins','removeplugins','keepwebviewui','restorewebviewui')
        $valueNames = @('version','repo','manifesturl','clientdir','servicename','plugin','webviewui','trustkey','keys')
        $nameLower = $name.ToLowerInvariant()
        if ($switches -contains $nameLower) {
            if ($hasValue) { throw "option $name does not take a value" }
            switch ($nameLower) {
                'yes' { $options.Yes = $true }
                'allowunsigned' { $options.AllowUnsigned = $true }
                'force' { $options.Force = $true }
                'norestart' { $options.NoRestart = $true }
                'dryrun' { $options.DryRun = $true }
                'removecache' { $options.RemoveCache = $true }
                'keepplugins' { $options.KeepPlugins = $true }
                'removeplugins' { $options.RemovePlugins = $true }
                'keepwebviewui' { $options.KeepWebViewUi = $true }
                'restorewebviewui' { $options.RestoreWebViewUi = $true }
            }
            $i++
            continue
        }
        if ($valueNames -contains $nameLower) {
            if (-not $hasValue) {
                if ($i + 1 -ge $Arguments.Count) { throw "option $name needs a value" }
                $value = [string]$Arguments[$i + 1]
                if ($value.StartsWith('-')) { throw "option $name needs a value" }
                $i++
            }
            $i++
            switch ($nameLower) {
                'version' { $options.Version = $value }
                'repo' { $options.Repo = $value }
                'manifesturl' { $options.ManifestUrl = $value }
                'clientdir' { $options.ClientDir = $value }
                'servicename' { $options.ServiceName = $value }
                'plugin' { $options.Plugin += $value }
                'webviewui' { $options.WebViewUi = $value }
                'trustkey' { $options.TrustKey += $value }
                'keys' { $options.Keys = $value }
            }
            continue
        }
        throw "unknown option: $token"
    }
    return $options
}

function Validate-Manifest {
    param([object]$Manifest)
    Require ($Manifest -is [pscustomobject]) 'release.json is not a JSON object'
    $format = Get-JsonProp $Manifest 'format'
    $formatIsNumber = $format -is [int] -or $format -is [long] -or $format -is [int64]
    Require ($null -ne $format -and $formatIsNumber -and $format -eq $script:StateFormat) 'release.json: format must be 1'
    $name = Get-JsonProp $Manifest 'name'
    Require ($name -is [string] -and $name) 'release.json: name must be a string'
    $version = Get-JsonProp $Manifest 'version'
    Require ($version -is [string] -and $version -match $script:SemverPattern) 'release.json: version must be a semantic version'
    foreach ($key in @('loader','market','index')) {
        $section = Get-JsonProp $Manifest $key
        Require ($section -is [pscustomobject]) "release.json: $key must be an object"
        $url = Get-JsonProp $section 'url'
        $sha = Get-JsonProp $section 'sha256'
        $size = Get-JsonProp $section 'size'
        Require ($url -is [string] -and $url) "release.json: $key.url must be a string"
        Require ($sha -is [string] -and $sha -match $script:HexPattern) "release.json: $key.sha256 must be 64 lowercase hex chars"
        Require (($size -is [int] -or $size -is [int64]) -and $size -ge 1 -and $size -le $script:MaximumPackageSize) "release.json: $key.size must be a positive size <= 256 MiB"
    }
    $loader = Get-JsonProp $Manifest 'loader'
    $api = Get-JsonProp $loader 'api'
    $apiIsNumber = $api -is [int] -or $api -is [long] -or $api -is [int64]
    Require ($apiIsNumber -and $api -ge 1) 'release.json: loader.api must be a positive integer'

    $plugins = Get-JsonProp $Manifest 'plugins'
    Require ($null -ne $plugins -and @($plugins).Count -gt 0) 'release.json: plugins must be a non-empty array'
    foreach ($plugin in @($plugins)) {
        Require ($plugin -is [pscustomobject]) 'release.json: plugin entries must be objects'
        $id = Get-JsonProp $plugin 'id'
        $required = Get-JsonProp $plugin 'required'
        $default = Get-JsonProp $plugin 'default'
        Require ($id -is [string] -and $id -match $script:IdPattern -and $id -match '[A-Za-z0-9-]') "release.json: plugin id '$id' is invalid"
        Require ($required -is [bool]) "release.json: plugin $id.required must be a boolean"
        Require ($default -is [bool]) "release.json: plugin $id.default must be a boolean"
        foreach ($field in @('name','group','note')) {
            $value = Get-JsonProp $plugin $field
            Require ($null -eq $value -or $value -is [string]) "release.json: plugin $id.$field must be a string"
        }
    }
}

function Resolve-PluginSelection {
    param([object[]]$Entries, [string[]]$Choices)
    $requiredIds = @()
    foreach ($entry in $Entries) {
        if ([bool]$entry.required) { $requiredIds += [string]$entry.id }
    }
    $selected = New-Object System.Collections.Generic.HashSet[string]
    foreach ($id in $requiredIds) { [void]$selected.Add($id) }
    if ($Choices) {
        foreach ($choice in $Choices) {
            $choice = [string]$choice
            switch ($choice.ToLowerInvariant()) {
                'required' {
                    foreach ($entry in $Entries) {
                        if ([bool]$entry.required) { [void]$selected.Add([string]$entry.id) }
                    }
                }
                'default' {
                    foreach ($entry in $Entries) {
                        if ([bool]$entry.required -or [bool]$entry.default) { [void]$selected.Add([string]$entry.id) }
                    }
                }
                'all' {
                    foreach ($entry in $Entries) { [void]$selected.Add([string]$entry.id) }
                }
                'none' {
                    # Required entries stay; 'none' removes the optional ones.
                }
                default {
                    $entry = $Entries | Where-Object { ([string]$_.id) -eq $choice }
                    if (-not $entry) { throw "unknown plugin id: $choice" }
                    [void]$selected.Add([string]$entry.id)
                    foreach ($id in $requiredIds) { [void]$selected.Add($id) }
                }
            }
        }
    } else {
        foreach ($entry in $Entries) {
            if ([bool]$entry.required -or [bool]$entry.default) { [void]$selected.Add([string]$entry.id) }
        }
    }
    return @($Entries | Where-Object { $selected.Contains([string]$_.id) })
}

function Show-PluginMenu {
    param([object[]]$Entries)
    $groups = New-Object System.Collections.Specialized.OrderedDictionary
    foreach ($entry in $Entries) {
        $group = if ($entry.PSObject.Properties['group'] -and $entry.group) { [string]$entry.group } else { 'Other' }
        if (-not $groups.Contains($group)) { $groups[$group] = @() }
        $groups[$group] += $entry
    }
    foreach ($group in $groups.Keys) {
        Write-Log ("{0} :" -f $group)
        foreach ($entry in $groups[$group]) {
            $required = if ([bool]$entry.required) { '[R] ' } else { '    ' }
            $default = if ([bool]$entry.default) { '[x] ' } else { '[ ] ' }
            $note = if ($entry.PSObject.Properties['note'] -and $entry.note) { " - $([string]$entry.note)" } else { '' }
            Write-Log "$required$default $([string]$entry.id)$note"
        }
    }
}

function Get-ReleaseBundle {
    param([hashtable]$Options, [string]$WorkDir)
    $repo = $Options.Repo
    $manifestUrl = $Options.ManifestUrl
    if (-not $manifestUrl) {
        if ($Options.Version) {
            $manifestUrl = "https://github.com/$repo/releases/download/v$($Options.Version)/release.json"
        } else {
            $manifestUrl = "https://github.com/$repo/releases/latest/download/release.json"
        }
    }
    $manifestPath = Join-Path $WorkDir 'release.json'
    $manifestSigPath = "$manifestPath.sig"
    $indexPath = Join-Path $WorkDir 'index.json'
    $indexSigPath = "$indexPath.sig"
    $marketPath = Join-Path $WorkDir 'parsec-orbit-market.exe'
    $cachedMarketPath = Join-Path $script:ToolDir 'parsec-orbit-market.exe'

    [void](Get-OrbitFile -Source $manifestUrl -Destination $manifestPath -Limit 1048576 -Description 'release manifest')
    if (-not $Options.AllowUnsigned) {
        [void](Get-OrbitFile -Source "$manifestUrl.sig" -Destination $manifestSigPath -Limit 4096 -Description 'manifest signature')
    }
    $manifestText = [System.IO.File]::ReadAllText($manifestPath)
    $manifest = $manifestText | ConvertFrom-Json
    Validate-Manifest $manifest
    $loaderUrl = Resolve-OrbitUrl $manifestUrl ([string](Get-JsonProp (Get-JsonProp $manifest 'loader') 'url'))
    $marketUrl = Resolve-OrbitUrl $manifestUrl ([string](Get-JsonProp (Get-JsonProp $manifest 'market') 'url'))
    $indexUrl = Resolve-OrbitUrl $manifestUrl ([string](Get-JsonProp (Get-JsonProp $manifest 'index') 'url'))

    $marketInfo = Get-JsonProp $manifest 'market'
    $indexInfo = Get-JsonProp $manifest 'index'
    $loaderInfo = Get-JsonProp $manifest 'loader'
    [void](Get-OrbitFile -Source $marketUrl -Destination $marketPath `
        -ExpectedSha256 ([string](Get-JsonProp $marketInfo 'sha256')) `
        -ExpectedSize ([long](Get-JsonProp $marketInfo 'size')) `
        -Description 'market tool')
    [void](Get-OrbitFile -Source $indexUrl -Destination $indexPath `
        -ExpectedSha256 ([string](Get-JsonProp $indexInfo 'sha256')) `
        -ExpectedSize ([long](Get-JsonProp $indexInfo 'size')) `
        -Description 'market index')
    if (-not $Options.AllowUnsigned) {
        [void](Get-OrbitFile -Source "$indexUrl.sig" -Destination $indexSigPath -Limit 4096 -Description 'index signature')
    }

    $verifier = $cachedMarketPath
    $usedCache = $false
    if (Test-Path -LiteralPath $cachedMarketPath) {
        $usedCache = $true
    } else {
        # With no cache, the freshly downloaded exe (digest-checked against
        # the manifest) is the first-run verifier.
        $verifier = $marketPath
        Write-Log 'no cached verifier; trusting the pinned manifest URL for this first market download'
    }
    if (-not $Options.AllowUnsigned) {
        $keys = @()
        foreach ($line in $Options.TrustKey) { $keys += $line }
        Ensure-TrustedKeys -Options $Options -TrustedKeyLines $keys
        Write-Log 'verifying manifest and index signatures'
        $signatureResult = Invoke-MarketRaw -Executable $verifier -Arguments @('verify-any', $manifestPath, '--keys', $Options.Keys)
        if ($signatureResult.ExitCode -ne 0) {
            $output = $signatureResult.Output -join ' '
            if ($output -match 'unknown command') {
                throw "the cached or downloaded market tool does not support verify-any; re-download or pass -AllowUnsigned"
            }
            throw "manifest signature verification failed (exit $($signatureResult.ExitCode))"
        }
        $indexResult = Invoke-MarketRaw -Executable $verifier -Arguments @('verify', $indexPath, '--keys', $Options.Keys)
        if ($indexResult.ExitCode -ne 0) {
            throw "index signature verification failed (exit $($indexResult.ExitCode))"
        }
        if (-not $Options.DryRun) {
            # The verifier just validated this release and the freshly
            # downloaded exe matched the manifest digest: replace the cache.
            New-Item -ItemType Directory -Force -Path $script:ToolDir | Out-Null
            $tmp = "$cachedMarketPath.tmp"
            Copy-Item -LiteralPath $marketPath -Destination $tmp -Force
            Move-Item -LiteralPath $tmp -Destination $cachedMarketPath -Force
        }
    } else {
        Write-Log 'allow-unsigned: index/manifest integrity comes only from HTTPS plus the sha256 entries.'
    }
    return @{
        Manifest = $manifest
        ManifestPath = $manifestPath
        IndexPath = $indexPath
        IndexSigPath = $indexSigPath
        MarketPath = $verifier
        ManifestUrl = $manifestUrl
        IndexUrl = $indexUrl
        LoaderUrl = $loaderUrl
        LoaderSha256 = [string](Get-JsonProp $loaderInfo 'sha256')
        LoaderSize = [long](Get-JsonProp $loaderInfo 'size')
        KeysPath = $Options.Keys
    }
}

function Select-Plugins {
    param([hashtable]$Options, [object[]]$Entries, [object[]]$StatePlugins = @())
    $choices = @()
    if ($Options.Plugin) {
        $choices = @($Options.Plugin)
        return (Resolve-PluginSelection -Entries $Entries -Choices $choices)
    }
    $stateIds = @($StatePlugins | ForEach-Object { [string]$_ })
    if ($stateIds.Count -gt 0) {
        $choices = @()
        foreach ($id in $stateIds) {
            $entry = $Entries | Where-Object { ([string]$_.id) -eq $id }
            if (-not $entry) { continue }
            $choices += $id
        }
        foreach ($entry in $Entries) {
            if ([bool]$entry.required -and -not ($choices -contains [string]$entry.id)) {
                $choices += [string]$entry.id
            }
        }
        return (Resolve-PluginSelection -Entries $Entries -Choices $choices)
    }
    if (Test-Interactive) {
        Show-PluginMenu -Entries $Entries
        $answer = Read-Host "plugin ids (comma-separated), all, none, or Enter for defaults"
        if (-not $answer) {
            return (Resolve-PluginSelection -Entries $Entries -Choices @())
        }
        return (Resolve-PluginSelection -Entries $Entries -Choices (@($answer -split ',') | Where-Object { $_ }))
    }
    return (Resolve-PluginSelection -Entries $Entries -Choices @())
}

function Confirm-Plan([string[]]$Lines) {
    Write-Log 'plan:'
    foreach ($line in $Lines) { Write-Log "  $line" }
    if ($script:Options.Yes) { return }
    if (-not (Test-Interactive)) { throw 'this plan requires confirmation; run with -Yes' }
    $answer = Read-Host 'Proceed? [y/N]'
    if ($answer -notmatch '^(y|yes)$') { throw 'cancelled' }
}

function Remove-PluginDirectories {
    param([string[]]$Ids, [string]$PluginsDir)
    foreach ($id in $Ids) {
        $dir = Join-Path $PluginsDir $id
        if (-not (Test-Path -LiteralPath $dir)) { continue }
        $manifestPath = Join-Path $dir 'market.json'
        if (-not (Test-Path -LiteralPath $manifestPath)) {
            Write-Log "skipping $dir (not a market-installed plugin)"
            continue
        }
        $market = [System.IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
        $storedId = Get-JsonProp $market 'id'
        if ($storedId -ne $id) {
            Write-Log "skipping $dir (market.json id '$storedId' does not match '$id')"
            continue
        }
        Remove-Item -LiteralPath $dir -Recurse -Force
        Write-Log "removed plugin directory $dir"
    }
}

function Get-InstalledPlugins {
    param([string]$MarketPath, [string]$PluginsDir, [string]$KeysPath)
    if (-not $MarketPath -or -not (Test-Path -LiteralPath $MarketPath)) { return $null }
    $result = Invoke-MarketRaw -Executable $MarketPath -Arguments @('installed', '--plugins-dir', $PluginsDir, '--keys', $KeysPath)
    if ($result.ExitCode -ne 0) {
        Write-Log "could not list installed plugins (exit $($result.ExitCode))"
        return $null
    }
    return $result.Output
}

function Show-OrbitStatus {
    param([hashtable]$Options)
    $state = Read-State
    if ($state) {
        Write-Log "state format: $($state.format)"
        Write-Log "version: $($state.version)"
        Write-Log "client dir: $($state.clientDir)"
        Write-Log "loader sha256: $($state.loaderSha256)"
        $webview = $state.webview
        if ($webview) {
            Write-Log "webview changed: $([bool]$webview.changed); previous value: $($webview.previous)"
        } else {
            Write-Log 'webview state: unknown'
        }
    } else {
        Write-Log 'not installed by this script'
    }
    $marketPath = Join-Path $script:ToolDir 'parsec-orbit-market.exe'
    $pluginsDir = Join-Path $Options.ClientDir 'plugins'
    $keysPath = $Options.Keys
    $installed = Get-InstalledPlugins -MarketPath $marketPath -PluginsDir $pluginsDir -KeysPath $keysPath
    if ($installed) {
        Write-Log 'installed plugin versions:'
        foreach ($line in $installed) { Write-Log "  $line" }
    } else {
        Write-Log 'installed plugin versions: unavailable (no cached market tool)'
    }
    try {
        $bundle = Get-ReleaseBundle -Options $Options -WorkDir $script:WorkDir
        Write-Log "latest release: $($bundle.Manifest.version)"
    } catch {
        Write-Log "could not read the latest release: $($_.Exception.Message)"
    }
}

function Remove-CacheDirs {
    foreach ($dir in @($script:WorkDir, $script:ToolDir)) {
        if (Test-Path -LiteralPath $dir) {
            Remove-Item -LiteralPath $dir -Recurse -Force
            Write-Log "removed cache directory $dir"
        }
    }
}

function Invoke-Install {
    param([hashtable]$Options, [string[]]$PositionalIds)
    if (-not $Options.DryRun -and -not (Test-Elevated)) {
        throw 'this operation must run from an elevated prompt'
    }
    if ($PositionalIds) { throw 'install takes no positional arguments' }
    $state = Read-State
    if ($state -and -not $Options.Force) {
        throw "already installed v$($state.version); run upgrade (or install -Force to reinstall)"
    }
    $bundle = Get-ReleaseBundle -Options $Options -WorkDir $script:WorkDir
    $manifest = $bundle.Manifest
    $plugins = @(Get-JsonProp $manifest 'plugins')
    $selected = Select-Plugins -Options $Options -Entries $plugins -StatePlugins @()
    $clientDir = $Options.ClientDir
    if (-not (Test-Path -LiteralPath $clientDir)) { throw "Parsec client directory not found: $clientDir" }
    $loaderPath = Join-Path $script:WorkDir 'bcrypt.dll'
    [void](Get-OrbitFile -Source $bundle.LoaderUrl -Destination $loaderPath `
        -ExpectedSha256 $bundle.LoaderSha256 -ExpectedSize $bundle.LoaderSize `
        -Description 'loader')
    $loaderTarget = Join-Path $clientDir 'bcrypt.dll'
    $serviceLine = "service: $($Options.ServiceName)"
    $lines = @(
        "release: v$($manifest.version)"
        "client dir: $clientDir"
        $serviceLine
        "loader: $loaderTarget (sha256 $($bundle.LoaderSha256))"
    )
    foreach ($entry in $selected) { $lines += "plugin: $([string]$entry.id)" }
    $lines += "webview action: $($Options.WebViewUi)"
    Confirm-Plan $lines
    if ($Options.DryRun) { return }

    Stop-Parsec -ServiceName $Options.ServiceName
    $firstInstall = $true
    $originalBackup = $null
    if ($state) {
        $firstInstall = $false
        $originalBackup = $state.originalBcryptBackup
    }
    if ($firstInstall) {
        if (-not (Test-Path -LiteralPath $loaderTarget)) {
            throw "no existing bcrypt.dll in $clientDir; refusing to install without a backup"
        }
        $backup = Join-Path $script:BackupDir ("bcrypt.dll.{0}.bak" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
        New-Item -ItemType Directory -Force -Path (Split-Path $backup -Parent) | Out-Null
        Copy-Item -LiteralPath $loaderTarget -Destination $backup -Force
        $originalBackup = $backup
        Write-Log "previous bcrypt.dll backed up to $backup"
    }
    Copy-LoaderWithRetry -Source $loaderPath -Destination $loaderTarget
    Complete-Install -Options $Options -Bundle $bundle -Selected $selected -State $state
}

function Copy-LoaderWithRetry {
    param([string]$Source, [string]$Destination)
    $copied = $false
    $temp = $null
    $destDir = [System.IO.Path]::GetDirectoryName($Destination)
    $destLeaf = [System.IO.Path]::GetFileName($Destination)
    $sourceLeaf = [System.IO.Path]::GetFileName($Source)
    for ($attempt = 1; $attempt -le 20 -and -not $copied; $attempt++) {
        try {
            $temp = Join-Path $destDir ("{0}.orbit-{1}" -f $destLeaf, $PID)
            Copy-Item -LiteralPath $Source -Destination $temp -Force
            Move-Item -LiteralPath $temp -Destination $Destination -Force
            $copied = $true
        } catch {
            if ($temp -and (Test-Path -LiteralPath $temp)) { Remove-Item -LiteralPath $temp -Force }
            if ($attempt -eq 20) { throw }
            Start-Sleep -Milliseconds 500
        }
    }
    Write-Log "installed $sourceLeaf -> $Destination"
}

function Complete-Install {
    param(
        [hashtable]$Options,
        [hashtable]$Bundle,
        [object[]]$Selected,
        [object]$State,
        [string]$WebViewChoice = ''
    )
    $manifest = $Bundle.Manifest
    $clientDir = $Options.ClientDir
    $pluginsDir = Join-Path $clientDir 'plugins'
    foreach ($entry in $Selected) {
        Invoke-Market -Executable $Bundle.MarketPath -Arguments @(
            'install', [string]$entry.id,
            '--with-deps',
            '--index', $Bundle.IndexPath,
            '--plugins-dir', $pluginsDir,
            '--keys', $Bundle.KeysPath
        )
        Write-Log "installed plugin $([string]$entry.id)"
    }
    $webviewChoice = if ($WebViewChoice) { $WebViewChoice } else { $Options.WebViewUi }
    if ($webviewChoice -eq 'Ask') {
        if (Test-Interactive) {
            $answer = Read-Host "webview UI: 1) enable WebView2 UI 2) force native UI 3) leave unchanged [3]"
            switch ($answer) {
                '1' { $webviewChoice = 'Enable' }
                '2' { $webviewChoice = 'Disable' }
                default { $webviewChoice = 'Keep' }
            }
        } else {
            $webviewChoice = 'Keep'
        }
    }
    if ($webviewChoice -eq 'Keep' -and $State -and $State.webview) {
        # A Keep choice on upgrade leaves the existing webview record alone.
        $webviewRecord = @{
            changed = [bool]$State.webview.changed
            previous = $State.webview.previous
            value = $State.webview.value
        }
    } else {
        $webviewRecord = @{ changed = $false; previous = $null }
    }
    $configPath = Join-Path $env:APPDATA 'Parsec\config.json'
    if ($webviewChoice -eq 'Enable' -or $webviewChoice -eq 'Disable') {
        if (-not (Test-Path -LiteralPath $configPath)) {
            Write-Log "warn: $configPath not found; cannot change client_web_ui"
        } else {
            $previous = Read-WebViewSetting -ConfigPath $configPath
            $value = $webviewChoice -eq 'Enable'
            Set-WebViewSetting -ConfigPath $configPath -Value $value
            $webviewRecord = @{ changed = $true; previous = $previous.Value; value = $value }
        }
    }
    $newState = @{
        format = $script:StateFormat
        version = [string]$manifest.version
        manifestUrl = $Bundle.ManifestUrl
        clientDir = $clientDir
        serviceName = $Options.ServiceName
        loaderSha256 = $Bundle.LoaderSha256
        originalBcryptBackup = $State.originalBcryptBackup
        pluginsDir = $pluginsDir
        plugins = @($Selected | ForEach-Object { [string]$_.id })
        webview = $webviewRecord
        installedUtc = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    }
    Write-State $newState
    if (-not $Options.NoRestart) {
        Start-Parsec -ServiceName $Options.ServiceName
    }
    Show-LoaderLog
}

function Invoke-Upgrade {
    param([hashtable]$Options, [string[]]$PositionalIds)
    if (-not $Options.DryRun -and -not (Test-Elevated)) {
        throw 'this operation must run from an elevated prompt'
    }
    if ($PositionalIds) { throw 'upgrade takes no positional arguments' }
    $state = Read-State
    if (-not $state) {
        throw 'not installed by this script; run install'
    }
    $bundle = Get-ReleaseBundle -Options $Options -WorkDir $script:WorkDir
    $manifest = $bundle.Manifest
    $comparison = Compare-Semver -Left ([string]$manifest.version) -Right ([string]$state.version)
    if ($comparison -le 0 -and -not $Options.Force) {
        throw 'already up to date'
    }
    $plugins = @(Get-JsonProp $manifest 'plugins')
    $selected = Select-Plugins -Options $Options -Entries $plugins -StatePlugins @($state.plugins)
    if (-not $selected) { throw 'the plugin selection is empty' }
    $clientDir = [string]$state.clientDir
    if (-not (Test-Path -LiteralPath $clientDir)) { throw "Parsec client directory not found: $clientDir" }
    $loaderPath = Join-Path $script:WorkDir 'bcrypt.dll'
    [void](Get-OrbitFile -Source $bundle.LoaderUrl -Destination $loaderPath `
        -ExpectedSha256 $bundle.LoaderSha256 -ExpectedSize $bundle.LoaderSize `
        -Description 'loader')
    $loaderTarget = Join-Path $clientDir 'bcrypt.dll'
    $lines = @(
        "release: v$($manifest.version)"
        "client dir: $clientDir"
        "service: $([string]$state.serviceName)"
        "loader: $loaderTarget (sha256 $($bundle.LoaderSha256))"
    )
    foreach ($entry in $selected) { $lines += "plugin: $([string]$entry.id)" }
    foreach ($id in @($state.plugins)) {
        $id = [string]$id
        if (-not ($selected | Where-Object { ([string]$_.id) -eq $id })) {
            $lines += "remove plugin: $id"
        }
    }
    $webviewChoice = $Options.WebViewUi
    if ($webviewChoice -eq 'Ask' -and $state.webview -and $state.webview.changed) {
        if ($null -ne $state.webview.value) {
            $webviewChoice = if ([bool]$state.webview.value) { 'Enable' } else { 'Disable' }
        } elseif ($null -ne $state.webview.previous) {
            $webviewChoice = if ([bool]$state.webview.previous) { 'Disable' } else { 'Enable' }
        } else {
            $webviewChoice = 'Enable'
        }
    }
    $lines += "webview action: $webviewChoice"
    Confirm-Plan $lines
    if ($Options.DryRun) { return }
    Stop-Parsec -ServiceName ([string]$state.serviceName)
    Copy-LoaderWithRetry -Source $loaderPath -Destination $loaderTarget
    foreach ($id in @($state.plugins)) {
        $id = [string]$id
        if (-not ($selected | Where-Object { ([string]$_.id) -eq $id })) {
            Invoke-Market -Executable $bundle.MarketPath -Arguments @(
                'remove', $id, '--plugins-dir', ([string]$state.pluginsDir)
            )
            Write-Log "removed plugin $id"
        }
    }
    Complete-Install -Options $Options -Bundle $bundle -Selected $selected -State $state `
        -WebViewChoice $webviewChoice
}

function Invoke-Uninstall {
    param([hashtable]$Options, [string[]]$PositionalIds)
    if (-not $Options.DryRun -and -not (Test-Elevated)) {
        throw 'this operation must run from an elevated prompt'
    }
    if ($PositionalIds) { throw 'uninstall takes no positional arguments' }
    if ($Options.KeepPlugins -and $Options.RemovePlugins) {
        throw '-KeepPlugins and -RemovePlugins are mutually exclusive'
    }
    if ($Options.KeepWebViewUi -and $Options.RestoreWebViewUi) {
        throw '-KeepWebViewUi and -RestoreWebViewUi are mutually exclusive'
    }
    $state = Read-State
    if (-not $state) {
        if (-not ($Options.Force -and $Options.ClientDir)) {
            throw 'not installed by this script; run install'
        }
        Write-Log 'no install state; proceeding with an empty plugin list and no recorded backup'
    }
    $clientDir = if ($state) { [string]$state.clientDir } else { $Options.ClientDir }
    $pluginsDir = if ($state) { [string]$state.pluginsDir } else { Join-Path $Options.ClientDir 'plugins' }
    $pluginIds = @()
    if ($state) { $pluginIds = @($state.plugins | ForEach-Object { [string]$_ }) }
    $lines = @( "client dir: $clientDir" )
    if ($state) { $lines += "service: $([string]$state.serviceName)" }
    if ($state -and $state.originalBcryptBackup) {
        $lines += "restore bcrypt.dll from $($state.originalBcryptBackup)"
    } else {
        $lines += 'loader: leave bcrypt.dll untouched'
    }
    if ($Options.RemovePlugins) {
        foreach ($id in $pluginIds) { $lines += "remove plugin: $id" }
    }
    if ($Options.KeepWebViewUi) {
        $lines += 'webview: keep'
    } elseif ($state -and $state.webview -and $state.webview.changed) {
        $lines += 'webview: restore previous setting'
    }
    Confirm-Plan $lines
    if ($Options.DryRun) { return }
    $serviceName = if ($state) { [string]$state.serviceName } else { $Options.ServiceName }
    Stop-Parsec -ServiceName $serviceName
    $loaderTarget = Join-Path $clientDir 'bcrypt.dll'
    if ($state -and $state.originalBcryptBackup) {
        $backupPath = [string]$state.originalBcryptBackup
        if (-not (Test-Path -LiteralPath $backupPath)) {
            if (-not $Options.Force) {
                throw "backup not found: $backupPath"
            }
            Write-Log "warn: backup not found; Parsec must be reinstalled; bcrypt.dll left untouched"
        } else {
            Copy-LoaderWithRetry -Source $backupPath -Destination $loaderTarget
        }
    }
    if ($Options.RemovePlugins) {
        foreach ($id in $pluginIds) {
            $marketPath = Join-Path $script:ToolDir 'parsec-orbit-market.exe'
            if (Test-Path -LiteralPath $marketPath) {
                Invoke-Market -Executable $marketPath -Arguments @(
                    'remove', $id, '--plugins-dir', $pluginsDir
                )
            } else {
                Remove-PluginDirectories -Ids @($id) -PluginsDir $pluginsDir
            }
            Write-Log "removed plugin $id"
        }
    }
    if ($state -and $state.webview -and $state.webview.changed) {
        if (-not $Options.KeepWebViewUi) {
            $configPath = Join-Path $env:APPDATA 'Parsec\config.json'
            if (Test-Path -LiteralPath $configPath) {
                $webview = $state.webview
                $previous = $webview.previous
                $previousBool = $null
                if ($null -ne $previous) { $previousBool = [bool]$previous }
                if ($null -eq $previousBool) {
                    Set-WebViewSetting -ConfigPath $configPath -Remove
                } else {
                    Set-WebViewSetting -ConfigPath $configPath -Value $previousBool
                }
            } else {
                Write-Log "warn: $($env:APPDATA)\Parsec\config.json not found; cannot restore client_web_ui"
            }
        }
    }
    if (Test-Path -LiteralPath $script:StatePath) {
        Remove-Item -LiteralPath $script:StatePath -Force
        Write-Log "removed state file $($script:StatePath)"
    }
    if ($Options.RemoveCache) {
        Remove-CacheDirs
    }
    if (-not $Options.NoRestart) {
        Start-Parsec -ServiceName $serviceName
    }
}

function Invoke-Plugins {
    param([hashtable]$Options, [string[]]$PositionalIds)
    if ($PositionalIds.Count -lt 1) { throw 'plugins needs a subcommand' }
    $sub = [string]$PositionalIds[0]
    if (@('list','install','remove','update') -notcontains $sub) { throw "unknown plugins subcommand: $sub" }
    $ids = @($PositionalIds | Select-Object -Skip 1)
    if ($sub -in @('install','remove') -and $ids.Count -eq 0) {
        throw "plugins $sub needs at least one id"
    }
    $state = Read-State
    if (-not $state) { throw 'not installed by this script; run install' }
    $bundle = Get-ReleaseBundle -Options $Options -WorkDir $script:WorkDir
    $pluginsDir = [string]$state.pluginsDir
    $keys = $bundle.KeysPath
    $market = $bundle.MarketPath
    if ($sub -eq 'list') {
        Invoke-Market -Executable $market -Arguments @('list', '--index', $bundle.IndexPath, '--keys', $keys)
        Invoke-Market -Executable $market -Arguments @('installed', '--plugins-dir', $pluginsDir, '--keys', $keys)
        return
    }
    $arguments = @()
    foreach ($id in $ids) { $arguments += $id }
    if ($sub -in @('install','remove')) {
        if ($Options.Force) { $arguments += '--force' }
    }
    $toolArgs = @($sub) + $arguments + @('--plugins-dir', $pluginsDir, '--keys', $keys)
    if ($sub -ne 'remove') {
        $toolArgs += @('--index', $bundle.IndexPath)
    }
    Invoke-Market -Executable $market -Arguments $toolArgs
    Write-Log 'restart the Parsec client to apply'
}

function Main {
    param([string[]]$ArgumentList)
    $appData = Get-AppData
    $localData = Get-LocalAppData
    $script:LogPath = Join-Path $env:TEMP 'parsec-orbit-manager.log'
    if (-not $env:TEMP) {
        $script:LogPath = Join-Path $localData 'ParsecOrbit\parsec-orbit-manager.log'
    }
    $script:WorkDir = Join-Path $localData 'ParsecOrbit\setup'
    $fallbackWorkDir = Join-Path $env:TEMP 'ParsecOrbit\setup'
    if (-not $env:LOCALAPPDATA -and $env:TEMP) {
        $script:WorkDir = $fallbackWorkDir
    }
    $script:ToolDir = Join-Path $localData 'ParsecOrbit\tool'
    $script:BackupDir = Join-Path $appData 'ParsecOrbit\backups'
    $script:StatePath = Join-Path $appData 'ParsecOrbit\install-state.json'
    $script:KeysPath = Join-Path $appData 'ParsecOrbit\market\trusted-keys.txt'

    $command = $null
    if ($ArgumentList.Count -gt 0) { $command = [string]$ArgumentList[0] }
    if (-not $command) {
        if ($script:ScriptPath) {
            Write-Usage
            exit 2
        }
        $command = 'install'
    }
    $validCommands = @('install','upgrade','uninstall','plugins','status','help')
    if ($validCommands -notcontains $command) {
        Write-Usage
        exit 1
    }
    $rest = @()
    if ($ArgumentList.Count -gt 1) { $rest = @($ArgumentList[1..($ArgumentList.Count - 1)]) }
    $options = Get-Option -Arguments $rest
    $script:Options = $options
    if ($options.Help) { Write-Usage; exit 0 }
    if ($command -eq 'help') { Write-Usage; exit 0 }
    if ($options.Version -and $options.Version -notmatch $script:SemverPattern) { throw "invalid -Version: $($options.Version)" }
    if ($options.Repo -notmatch '^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$') { throw "invalid -Repo: $($Options.Repo)" }
    if ($options.WebViewUi -inotmatch '^(Ask|Enable|Disable|Keep)$') { throw "invalid -WebViewUi: $($options.WebViewUi)" }
    foreach ($choice in $options.Plugin) {
        if ($choice -inotmatch '^([A-Za-z0-9._-]+|required|default|all|none)$') {
            throw "invalid -Plugin: $choice"
        }
    }
    New-Item -ItemType Directory -Force -Path $script:WorkDir | Out-Null
    if (-not $options.DryRun) {
        New-Item -ItemType Directory -Force -Path $script:ToolDir | Out-Null
        New-Item -ItemType Directory -Force -Path $script:BackupDir | Out-Null
    }
    $script:KeysPath = $options.Keys
    if ($options.DryRun) { Write-Log 'dry run: no changes will be made' }

    switch ($command) {
        'install' { Invoke-Install -Options $options -PositionalIds $options.PositionalIds }
        'upgrade' { Invoke-Upgrade -Options $options -PositionalIds $options.PositionalIds }
        'uninstall' { Invoke-Uninstall -Options $options -PositionalIds $options.PositionalIds }
        'plugins' { Invoke-Plugins -Options $options -PositionalIds $options.PositionalIds }
        'status' { Show-OrbitStatus -Options $options }
    }
}

try {
    $script:ScriptPath = $MyInvocation.MyCommand.Path
    if (-not $script:ScriptPath) { $script:ScriptPath = $PSCommandPath }
    Main -ArgumentList $args
} catch {
    $message = $_.Exception.Message
    if ($_.Exception.InnerException) { $message = $message + ': ' + $_.Exception.InnerException.Message }
    Write-Log "error: $message"
    if ($_.InvocationInfo) {
        Write-Log ("at " + $_.InvocationInfo.PositionMessage.Trim())
    }
    exit 1
}
