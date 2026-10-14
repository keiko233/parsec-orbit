# Marketplace

Status: implemented (`crates/parsec-orbit-registry`, the `parsec-orbit-market` tool). There is no hosted service: a *market* is an index file, the packages it lists, and a signature, served from anywhere (a GitHub release, a static web server, a folder).

## What is trusted

Nothing is trusted because of where it came from.

1. A **package** is trusted if its sha256 and size equal the entry of a signed index.
2. An **index** is trusted if one of the keys in the user's `trusted-keys.toml` signed its exact bytes (ed25519).
3. **Plugins are native code** in the client process. The signature says who published a package, not that it is harmless; see the [security notes](plugin-abi.md#security-notes).

The key list is `<Orbit root>/market/trusted-keys.toml` (or `--keys`), with one `[[key]]` table per key: `algorithm = "ed25519"`, `public = "<base64 public key>"` and an optional `comment`. A `--keys` file whose name does not end in `.toml` is read in the older line format instead: one `ed25519 <base64 public key> [comment]` per line, `#` for comments. If there is no `trusted-keys.toml` but there is a `trusted-keys.txt` beside it, the text list is converted once into the TOML file; the text file is left as it was and is not read again. On Windows, the Orbit root is `%APPDATA%\ParsecOrbit`. On macOS arm64, it is `~/Library/Application Support/ParsecOrbit`, or the directory in `PARSEC_ORBIT_HOME`. No key is built into the tool; users add the keys of the publishers they trust. With an empty list nothing verifies. `--allow-unsigned` accepts an index whose signature is missing or wrong (and says so); the digests in it are then only as good as the connection it came over.

## The index

`index.json`, UTF-8 JSON, next to `index.json.sig` (base64 of the 64-byte ed25519 signature over the raw bytes of `index.json`).

```json
{
  "format": 1,
  "name": "Parsec Orbit v0.2.0",
  "plugins": [{
    "id": "com.parsec-orbit.hooks",
    "name": "Hooks",
    "description": "Provides parsec-orbit.hooks: ...",
    "author": "Parsec Orbit contributors",
    "license": "MIT",
    "versions": [{
      "version": "0.1.0",
      "api": 1,
      "platform": "windows-x86_64",
      "url": "parsec-orbit-lib-hooks.zip",
      "sha256": "<64 lowercase hex digits>",
      "size": 161561,
      "requires": ["com.example.base"]
    }]
  }]
}
```

* `format` is `1`; anything else is refused.
* `id` is 1 to 128 of `[A-Za-z0-9._-]`, not only dots: it becomes a folder name. Ids are unique; versions are semantic versions, unique per plugin and platform.
* `url` is absolute or relative to the index; a relative one may not contain `..`.
* `api` is the loader ABI version the release needs (default 1); releases that need more than the tool's are not offered.
* `platform` is optional. It is `<os>-<arch>`: lowercase letters and digits, a `-`, then lowercase letters, digits and `_` (`windows-x86_64`, `macos-arm64`). It is `windows-x86_64` when omitted, preserving existing indexes. Any other valid value is read, but the tool lists and installs only releases for its own platform, which is `windows-x86_64` or `macos-arm64`; a malformed value refuses the index. Two packages may use the same plugin version when they target different platforms, but a platform and version pair must be unique. See [macOS platform rules](macos-platform.md#9-marketplace-on-macos) for package format, ad-hoc signing and paths.
* `requires` is copied from the plugin's manifest (version constraints dropped); `install --with-deps` installs what the index lists for the host platform.
* `size` is at most 256 MiB.
* `serial` (optional integer) rises with every change of the index. A tool that has verified an index with a serial refuses a later index with a lower one (rollback); a missing serial counts as 0. `generated` (optional, RFC 3339) is informative.
* A plugin's `role` (optional) is `lib` or `plugin`, `plugin` when absent; any other value refuses the index. `summary`, `category`, `homepage`, `group`, `default`, `required` and `official` are presentation fields that are read and written without being acted on here.
* A release's `published` (optional, RFC 3339 date) is informative. `yanked` (default `false`) withdraws a release: it is never chosen by default, but `--version` still installs it.
* `mirrors` (optional, in order) are other locations of the package, resolved like `url`. When `url` fails its size or sha256 check, or cannot be fetched, each mirror is tried in turn and the first valid one is installed; a failure names every location tried.

The newest release for the host platform that is compatible and not a pre-release is the default; `--version` picks another matching release, pre-releases included. Asking for a version that exists only on another platform is refused.

## The package

A zip with exactly one folder, holding the plugin as the loader expects it:

```text
parsec-orbit-lib-hooks/plugin.toml
parsec-orbit-lib-hooks/parsec_orbit_lib_hooks.dll
```

Windows packages use `kind = "dll"` and a `.dll` entry. macOS arm64 packages use `kind = "dylib"` and a `.dylib` entry. A zip still contains exactly one plugin folder.

It is checked before anything is written: names are plain relative paths with `/` and no `..`, drive letters, `:`, reserved Windows names or trailing dots/spaces; no symbolic links; no duplicates (ignoring case); at most 4096 entries, 128 MiB per file and 256 MiB in all, counted as read rather than as the headers claim. `plugin.toml` has to be there, its `id` and `version` have to be the index entry's, and its `entry` has to be in the package.

## Installing

Plugins go to `<Orbit root>/plugins/<id>/` (or `--plugins-dir`).

The library can keep the last verified index of each source in a cache folder (`Market::with_cache`; the command line does not enable it yet). Only an index a trusted key signed is stored, as `index.json`, `index.json.sig` and `etag` (the sha256 of `index.json`) in `<cache>/<sha256 of the location>/`, each replaced atomically. A later index whose `serial` is lower than the cached one is refused.

1. fetch the index and its signature, verify it;
2. fetch the package, compare size and sha256, check it;
3. write it to a staging folder next to the target with a `market.json` (id, version, package sha256, index, time). On macOS, sign every staged `.dylib` with `codesign --force --sign -` and verify each signature before the swap. If signing or verification fails, discard the staging folder and leave the installed version in place. The digest in `market.json` remains the digest of the published zip, whose dylib bytes differ from the installed signed files. Then swap the staged folder in; the old version is renamed away first and restored if the swap fails, so an install is never half done;
4. the plugin loads the next time the client starts.

A plugin that is not in the plugins folder through the market (no `market.json`) is never replaced or removed without `--force`. On Windows, a plugin DLL held by the running client cannot be replaced; the tool says so and asks to close the client. On macOS, replacing a mapped dylib unlinks the old file while the running process keeps its inode; the new version loads the next time the client starts.

## Commands

```text
parsec-orbit-market list                      [--index <file or URL>]
parsec-orbit-market installed
parsec-orbit-market install <id>              [--version V] [--with-deps] [--force]
parsec-orbit-market update [<id>]
parsec-orbit-market remove <id>               [--force]

parsec-orbit-market index <folder> --out index.json [--name text]   # publishers
parsec-orbit-market keygen <name>             # writes <name>.key (secret), prints the public key line
parsec-orbit-market sign index.json --key <name>.key
parsec-orbit-market verify index.json [--keys file]
parsec-orbit-market verify-any file [--keys file]
```

`--index` can also come from `PARSEC_ORBIT_INDEX`. Locations are files or
`http(s)` URLs. `verify-any` checks a signature without requiring the file to
be an index; the network installer uses it for `release.json`.

## Publishing

The `index` publisher command marks packages with `kind = "dylib"` as
`macos-arm64`; other packages retain the `windows-x86_64` default.

`scripts/package.ps1` ships `parsec-orbit-market.exe` with every release,
builds `index.json` from the zips in `dist`, and writes `release.json` (the
distribution manifest the network installer reads). When the maintainer
passes `-SigningKeyFile`, the same step signs both JSON files. The Release
workflow turns the `MARKET_SIGNING_KEY` secret (the content of the `.key` file
made by `keygen`) into a temporary key file, so the index of a release is
`https://github.com/<owner>/<repo>/releases/download/<tag>/index.json`.
Generating and guarding the real key, and putting its public line into the
network installer's embedded key constant, is the maintainer's job; the
repository contains no secret.

## Not (yet) provided

* The `parsec-orbit.market` interface (install source and update metadata of a plugin to other plugins) is reserved; the metadata is the `market.json` file for now.
* No automatic background updates and no in-client UI; upgrades are explicit
  through `scripts/orbit-manager.ps1 upgrade` (see
  [network-installer.md](../design/network-installer.md)). No revocation list
  exists; a leaked key is dealt with by changing the key list.
* No version constraints in `requires` (see the [manifest](plugin-manifest.md)).
