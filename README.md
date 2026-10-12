# Parsec Orbit

An unofficial, local-only proof of concept for loading plugins in the Parsec
client. It uses a `bcrypt.dll` proxy and does not modify Parsec files.

See the [plugin ABI reference](docs/plugin-abi.md) for the plugin interface.
See [interface patches](docs/patches.md) for the runtime patches that widen the
bandwidth limit selections in the WebView and native UIs.
