# Official registry

This folder is the catalogue of Parsec Orbit plugins and of the loader and
installer releases. It holds small text files only: what exists, where each
package is, and the sha256 it must have. The packages themselves are not
committed. They stay where they are built (GitHub release assets, or any HTTPS
URL), and git history is the audit log. The signature, not the hosting, is what
makes the registry trustworthy.

The clients read only the generated files `index.json` and `host.json` (with
their `.sig`), never these sources. They are produced by
`parsec-orbit-release registry build`.

## Layout

```text
manifest/
  README.md                    this file
  host/releases.toml          loader and installer, one [[release]] per version and platform
  plugins/<id>/entry.toml      identity and presentation of a plugin or library
  plugins/<id>/releases.toml   one [[release]] per package
  index.json  index.json.sig   GENERATED: every plugin and library
  host.json   host.json.sig    GENERATED: loader and installer
```

`<id>` is the plugin id, used as the folder name exactly as the loader and
`orbit` use it. The id must be 1 to 128 of `A-Z a-z 0-9 . _ -`, not only dots.

## entry.toml

Everything but `id` is optional. Most of the rest comes from the package's own
`plugin.toml`.

```toml
id = "com.parsec-orbit.webview"
default = true                 # pre-selected by the installer (false when absent)
group = "UI patches"           # grouping in the installer prompt and the market UI
category = "UI patches"        # overrides the derived category
summary = "Adds bandwidth limits above 50 Mbps to the WebView UI"   # at most 120 characters
homepage = "https://github.com/keiko233/parsec-orbit"

[build]                        # official modules only
platforms = ["windows-x86_64"]
```

* `id` equals the folder name and is a valid plugin id.
* `summary` is at most 120 characters. `homepage` is an `https://` URL.
* `[build]` marks an official module that CI builds from this repository.
  `platforms` lists the platforms it ships on, as `<os>-<arch>`
  (`windows-x86_64`, `macos-arm64`).
* The ids starting with `com.parsec-orbit.` are reserved for official modules.
  An entry with that prefix must have a `[build]` table, and an entry without
  `[build]` may not use the prefix.

## releases.toml

```toml
[[release]]
version = "0.2.0"              # must equal the version in the package's plugin.toml
platform = "windows-x86_64"    # optional; windows-x86_64 when absent
url = "https://github.com/keiko233/parsec-orbit/releases/download/v0.2.0/parsec-orbit-plugin-high-bitrate-webview-windows-x86_64.zip"
mirrors = []                   # optional; other locations, tried in order after url
sha256 = "<64 lowercase hex digits>"
published = 2026-11-01         # optional TOML date
yanked = false                 # optional; true hides the release from selection
```

* `url` is `https://`, or a relative path with no `..`. Packages are never
  committed here.
* `sha256` is the digest of the exact bytes of the package.
* `(version, platform)` is unique within the file.
* A new block is appended by a release job, so do not reorder or reformat
  existing blocks. `yanked` is a human edit.

## Official modules

Official modules (an entry with `[build]`) are added by the release process,
not by hand:

1. `parsec-orbit-release package --platform <platform>` builds each module's
   package, and CI uploads it as a release asset.
2. `parsec-orbit-release registry add --tag <tag> --assets <dir>` appends the
   `[[release]]` blocks to `plugins/<id>/releases.toml` and the loader and
   installer to `host/releases.toml`.
3. On a push to `main` that touches this folder, CI runs
   `parsec-orbit-release registry build --sign <key>`. It writes `index.json`,
   `host.json` and their signatures, and commits them.

The serial in the generated files rises only when their content changes.

## Third-party plugins

A third-party plugin is listed by a pull request that adds
`plugins/<id>/entry.toml` (without `[build]`) and `plugins/<id>/releases.toml`
with one `[[release]]` per package. The package must be served over HTTPS, must
not change once listed (publish a new version instead), and its sha256 must
match. Run `parsec-orbit-release registry check` before submitting.

A listed package is signed by the maintainers' key, so listing means "the
maintainers have looked at this entry". The signature does not vouch for what
the plugin does: a plugin is native code running in the client process.

## Checks

`parsec-orbit-release registry check` validates this folder offline: the
schema, the ids, the reserved prefix, the URLs, the digests, the uniqueness of
releases and the `summary` length. It runs on every pull request that touches
`manifest/`. It does not download packages; `registry build` does that and
verifies every digest.
