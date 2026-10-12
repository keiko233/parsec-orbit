# Parsec Orbit

An unofficial, local-only proof of concept for loading plugins in the Parsec
client. It uses a `bcrypt.dll` proxy and does not modify Parsec files.

## Layout

* `parsec-orbit-loader/` - the `bcrypt.dll` proxy. It forwards every export to
  the real `bcrypt.dll`, opens the WebView2 DevTools endpoint and hosts the
  plugin system.
* `parsec-orbit-plugin-sdk/` - the plugin ABI; see the
  [plugin ABI reference](docs/plugin-abi.md).
* `plugins/parsec-orbit-plugin-webview/` - official plugin that widens the
  "Bandwidth Limit" choices in the WebView2 UI; see its
  [README](plugins/parsec-orbit-plugin-webview/README.md).
* `plugins/parsec-orbit-plugin-native/` - official plugin for the same options
  in the native (libmatoya) UI; see its
  [README](plugins/parsec-orbit-plugin-native/README.md).
* `examples/parsec-orbit-plugin-hello/` - smallest useful sample plugin.
* `verify/parsec-orbit-verify-*` - verification tools for the loader and the
  WebView patch delivery.

## Build and install

```powershell
cargo build --release
scripts\package.ps1        # stages dist\ with bcrypt.dll, plugin zips, SHA256SUMS
```

Install by copying `bcrypt.dll` next to `parsecd.exe` (typically
`C:\Program Files\Parsec`) and the plugin folder into
`C:\Program Files\Parsec\plugins\`. The client loads the proxy at start-up;
everything it logs goes to `%APPDATA%\ParsecOrbit\parsec-orbit.log`.
