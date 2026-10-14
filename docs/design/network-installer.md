# Orbit Manager (network installer)

Status: proposed. Implemented by the `orbit-manager` script
(`scripts/orbit-manager.ps1`), the artifacts it
consumes (`release.json` plus the marketplace `index.json`) and the packaging
changes in `scripts/package.ps1`.

## Goals

The local `scripts/install.ps1` assumes a checkout, a finished `cargo build`
and the Parsec client on the same machine. End users need the opposite: a
single script that fetches a released loader and its official plugins from the
network, upgrades an existing install and removes it again cleanly.

The four requirements map to this design as follows:

1. `install`, `uninstall` and `upgrade` commands. `install` refuses to run
   over an install it manages (it says to run `upgrade`); `uninstall` restores
   the original `bcrypt.dll` instead of deleting it.
2. Optional plugins are selected from a distribution manifest, not guessed
   from the plugin folders. The manifest marks entries as required, default,
   grouped and described; the loader DLL itself is always installed.
3. The user chooses the UI mode at install time. The script can force the
   WebView2 UI on or off, or leave the existing setting alone, and records
   what it changed so `uninstall` can restore it.
4. Plugin management goes through the existing `parsec-orbit-market` tool
   (`install`, `remove`, `update`, `list`). The script is a thin, signed
   wrapper around the same index a user could pass to the tool directly.

## Release artifacts

One GitHub release holds everything the script needs:

```text
bcrypt.dll                              the loader, next to parsecd.exe
orbit-manager.ps1                       the manager script itself
parsec-orbit-market.exe                 verifies the manifests and installs plugins
parsec-orbit-plugin-*.zip               official plugin packages
index.json  index.json.sig              signed market index (plugins, versions, digests)
release.json  release.json.sig          signed distribution manifest (loader, tool, policy)
SHA256SUMS.txt                          a human-readable copy of the digests
```

The manager script is the bootstrap root: it is fetched over the pinned HTTPS
URL and its digest appears in `SHA256SUMS.txt`; everything it downloads is
then checked against the signed manifests.

`release.json` describes the release itself, not the plugin packages:

```json
{
  "format": 1,
  "name": "Parsec Orbit",
  "version": "0.1.0",
  "loader": {
    "url": "bcrypt.dll",
    "sha256": "64 lowercase hex digits",
    "size": 374272,
    "api": 1
  },
  "market": {
    "url": "parsec-orbit-market.exe",
    "sha256": "...",
    "size": 1848320
  },
  "index": {
    "url": "index.json",
    "sha256": "...",
    "size": 4439
  },
  "plugins": [
    {
      "id": "com.parsec-orbit.webview-bridge",
      "required": true,
      "default": true,
      "group": "WebView",
      "note": "Provides parsec-orbit.webview"
    }
  ]
}
```

* `format` is `1`; anything else is refused.
* Every `sha256` is lowercase hex; every `size` is checked after download.
* URLs are relative to the manifest URL and may not contain a `..`
  component. A manifest fetched over `http(s)` may only name `http(s)`
  locations; the same rule as `parsec-orbit-market` keeps a remote file from
  pointing at a local disk path.
* `loader.api` is the loader ABI version; releases newer than the local
  loader are not offered by the marketplace.
* Plugin entries only carry selection policy. Artifact URLs and digests stay
  in `index.json`, which is already defined, tested and signed by the market
  tool.

The plugin policy in `scripts/package.ps1` is explicit: a packaged plugin that
is missing from the policy, or a policy id that is missing from the release,
fails packaging. The current defaults are the three UI plugins
(`webview-bridge`, `webview`, `native`), matching what `scripts/install.ps1`
installs today; `config`, `js-runtime`, `hooks`, `stun-swap` and the `hello`
example are offered but deselected.

## Trust model

The repository's marketplace rule stays in force: nothing is trusted because
of where it came from. The installer adds one bootstrap step that PowerShell
itself cannot do, because Windows PowerShell 5.1 has no Ed25519 API.

1. The script fetches `release.json`, `release.json.sig`, `index.json`,
   `index.json.sig` and `parsec-orbit-market.exe` over TLS from the pinned
   GitHub repository URL.
2. The market exe's sha256 is checked against `release.json` before it runs.
   On the very first install this is only TLS-to-the-pinned-URL, not a
   signature; the script says so.
3. The market exe verifies `release.json.sig` with the new `verify-any`
   command (which, unlike `verify`, does not require the file to be an index)
   and `index.json.sig` with `verify`, against the user's trusted keys in
   `%APPDATA%\ParsecOrbit\market\trusted-keys.txt`.
4. Only after verification succeeds is the downloaded exe cached at
   `%LOCALAPPDATA%\ParsecOrbit\tool\parsec-orbit-market.exe`. Later upgrades
   use the cached, previously trusted exe to verify the new manifests, then
   replace the cache with the new exe whose digest the new manifest vouches
   for. The first TLS fetch is therefore a one-time bootstrap, not a standing
   weakness.
5. Every artifact (loader, market exe, index, plugin zip) is digest-checked
   against a signed file before it is used or written.

The official public key is a constant in the script (`OfficialEd25519PublicKey`)
that the maintainer fills in before publishing a release. First-time users
get it added to their key file with a notice; users who prefer another key
pass `-TrustKey` or manage the file themselves. `-AllowUnsigned` skips steps
3-4 and is explicitly labelled as trusting the connection and digests only.

The installer is honest about the bootstrap rather than pretending PowerShell
can verify Ed25519: `verify-any` lives in the Rust tool where the crypto
already lives.

## Commands

```text
powershell -ExecutionPolicy Bypass -File orbit-manager.ps1 install [options]
powershell -ExecutionPolicy Bypass -File orbit-manager.ps1 upgrade [options]
powershell -ExecutionPolicy Bypass -File orbit-manager.ps1 uninstall [options]
powershell -ExecutionPolicy Bypass -File orbit-manager.ps1 plugins <list|install|remove|update> ...
powershell -ExecutionPolicy Bypass -File orbit-manager.ps1 status
```

Selected options:

```text
-Version <x.y.z>      pin a release instead of using latest
-Repo <owner/name>    default keiko233/parsec-orbit
-ManifestUrl <loc>    local file or https URL, for offline and testing
-ClientDir <dir>      default C:\Program Files\Parsec
-Plugin <selection>   required | default | all | none | plugin ids (repeatable)
-WebViewUi <mode>     Ask | Enable | Disable | Keep
-TrustKey <line>      append an ed25519 public key
-Keys <file>          trusted keys file (defaults to the market's file)
-AllowUnsigned        skip signature verification, keep sha256 checks
-Yes                  accept every confirmation
-DryRun               print the plan and change nothing
-Force                override the install/uninstall safety checks
-NoRestart            leave the Parsec service stopped
```

When no selection is given, `install` is interactive: it prints the grouped
plugin list and asks for ids, `all`, `none`, or Enter for the defaults, and
then asks whether to force the WebView2 UI on, force the native UI, or leave
the setting alone. `-Yes` takes the defaults without prompting. Pipelines such
as `irm <release>/orbit-manager.ps1 | iex` default to an interactive `install`.

## Install and upgrade

The loader and the official plugins are installed into the Parsec client
directory, matching the existing local installer:

* `bcrypt.dll` -> `<client>\bcrypt.dll`
* plugin packages -> `<client>\plugins\<id>\` through the market tool

State lives in `%APPDATA%\ParsecOrbit\install-state.json`:

```json
{
  "format": 1,
  "version": "0.1.0",
  "manifestUrl": "https://github.com/.../release.json",
  "clientDir": "C:\\Program Files\\Parsec",
  "loaderSha256": "...",
  "originalBcryptBackup": "C:\\Users\\...\\backups\\bcrypt.dll.20261013-123456.bak",
  "pluginsDir": "C:\\Program Files\\Parsec\\plugins",
  "plugins": ["com.parsec-orbit.webview-bridge", "..."],
  "webview": { "changed": true, "previous": null, "value": true },
  "installedUtc": "2026-10-13T00:00:00Z"
}
```

Install flow:

1. Verify elevation, fetch and verify the manifests.
2. Resolve the plugin selection against `release.json` (required ids are
   always included; dependencies such as `stun-swap` -> `hooks` are added by
   the market tool's `--with-deps`).
3. Stop the Parsec service and any `parsecd` process.
4. Back up the client's `bcrypt.dll` once, under
   `%APPDATA%\ParsecOrbit\backups\`, and record the path. The script refuses
   to touch `bcrypt.dll` when no backup can be made.
5. Replace the loader, install each selected plugin through the market tool,
   apply the WebView choice, write the state file atomically and restart the
   service.

`upgrade` keeps the original `bcrypt.dll` backup untouched, preserves the
selected plugin ids unless `-Plugin` overrides them, and re-applies a WebView
change the client may have reset. `market update` makes each selected plugin
upgrade independently of the distribution version, so a plugin-only fix does
not require a whole new release.

Every swap follows the existing safety rules: the loader copy retries around
the file lock, and the market tool stages a plugin next to its target before
renaming it in, so an interrupted upgrade leaves the old loader or plugin, not
a half-written one.

## Uninstall

`uninstall` is the inverse of install:

1. Stop the service.
2. Restore the recorded original `bcrypt.dll`. A missing backup is a hard
   error: the script will not delete a Windows DLL it cannot replace.
3. Remove the market-managed plugins, unless the user asks to keep them.
   Removal prefers the market tool; a fallback deletes a directory only when
   its `market.json` proves the market owns it, so a hand-installed plugin is
   never deleted by accident.
4. Restore the WebView setting recorded in state (including removing the
   `client_web_ui` key if the script added it).
5. Delete the state file. Backups and the tool cache survive by default
   (`-RemoveCache` clears the cache).

An install that did not come from this script has no state file; `uninstall`
requires `-Force` then and can only act on paths the user gives explicitly.

## Future plugin management

The `plugins` subcommands already delegate to `parsec-orbit-market` with the
same signed index, trusted-key file and plugin directory the installer uses,
so official plugins can be added, removed or updated later without touching
the loader. Third-party markets work too: point `-ManifestUrl` (or set the
market tool's `PARSEC_ORBIT_INDEX`) at another signed index, and the same
selection, verification and rollback rules apply.

## Release integration

`scripts/package.ps1` now also:

* copies `parsec-orbit-market.exe` into `dist\`;
* builds `index.json` with the market tool and validates the plugin policy;
* writes `release.json` with the workspace version, the loader ABI version and
  sha256/size for the loader, tool and index;
* signs both JSON files when `-SigningKeyFile` is given, and warns otherwise.

The Release workflow passes the `MARKET_SIGNING_KEY` secret (the content of a
`keygen`-produced `.key` file) to a temporary key file and removes it
immediately. Until the maintainer generates that key and pastes the public key
into `orbit-manager.ps1`, releases remain unsigned and users need
`-AllowUnsigned`.

## Testing

The installer never needs the real Parsec client to be exercised. A test run
uses a throwaway client directory and a local `dist\` manifest:

* `-DryRun install -ManifestUrl dist\release.json` prints the plan without
  elevation.
* An offline smoke test can install into a temporary client directory with a
  fake `parsecd.exe`, then run `upgrade` and `uninstall` and assert that the
  original `bcrypt.dll` bytes are back.
* The market half is already covered by `cargo test -p parsec-orbit-registry`;
  `verify-any` adds a test for arbitrary signed files.

## Open questions

* Whether the official public key should also be embedded in the README so it
  can be checked out of band.
* Whether `upgrade` should reinstall plugins the user explicitly removed, or
  record and honour that removal forever (currently it preserves the state
  list, so removal is honoured).
* How far to go with delta upgrades; today every artifact is fetched whole,
  which is fine for a few megabytes.
