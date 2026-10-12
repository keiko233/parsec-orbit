# Marketplace

Status: implemented (`crates/parsec-orbit-market`, the `parsec-orbit-market` tool). There is no hosted service: a *market* is an index file, the packages it lists, and a signature, served from anywhere (a GitHub release, a static web server, a folder).

## What is trusted

Nothing is trusted because of where it came from.

1. A **package** is trusted if its sha256 and size equal the entry of a signed index.
2. An **index** is trusted if one of the keys in the user's `trusted-keys.txt` signed its exact bytes (ed25519).
3. **Plugins are native code** in the client process. The signature says who published a package, not that it is harmless; see the [security notes](plugin-abi.md#security-notes).

The key list is `%APPDATA%\ParsecOrbit\market\trusted-keys.txt` (or `--keys`): one `ed25519 <base64 public key> [comment]` per line, `#` for comments. No key is built into the tool; users add the keys of the publishers they trust. With an empty list nothing verifies. `--allow-unsigned` accepts an index whose signature is missing or wrong (and says so); the digests in it are then only as good as the connection it came over.

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
      "url": "parsec-orbit-plugin-hooks.zip",
      "sha256": "<64 lowercase hex digits>",
      "size": 161561,
      "requires": ["com.example.base"]
    }]
  }]
}
```

* `format` is `1`; anything else is refused.
* `id` is 1 to 128 of `[A-Za-z0-9._-]`, not only dots: it becomes a folder name. Ids are unique; versions are semantic versions, unique per plugin.
* `url` is absolute or relative to the index; a relative one may not contain `..`.
* `api` is the loader ABI version the release needs (default 1); releases that need more than the tool's are not offered.
* `requires` is copied from the plugin's manifest (version constraints dropped); `install --with-deps` installs what the index lists.
* `size` is at most 256 MiB.

The newest release that is compatible and not a pre-release is the default; `--version` picks another, pre-releases included.

## The package

A zip with exactly one folder, holding the plugin as the loader expects it:

```text
parsec-orbit-plugin-hooks/plugin.toml
parsec-orbit-plugin-hooks/parsec_orbit_plugin_hooks.dll
```

It is checked before anything is written: names are plain relative paths with `/` and no `..`, drive letters, `:`, reserved Windows names or trailing dots/spaces; no symbolic links; no duplicates (ignoring case); at most 4096 entries, 128 MiB per file and 256 MiB in all, counted as read rather than as the headers claim. `plugin.toml` has to be there, its `id` and `version` have to be the index entry's, and its `entry` has to be in the package.

## Installing

Plugins go to `%APPDATA%\ParsecOrbit\plugins\<id>\` (or `--plugins-dir`).

1. fetch the index and its signature, verify it;
2. fetch the package, compare size and sha256, check it;
3. write it to a staging folder next to the target with a `market.json` (id, version, sha256, index, time) and swap it in; the old version is renamed away first and restored if the swap fails, so an install is never half done;
4. the plugin loads the next time the client starts.

A plugin that is not in the plugins folder through the market (no `market.json`) is never replaced or removed without `--force`. A plugin whose DLL the running client holds cannot be replaced; the tool says so and asks to close the client.

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
```

`--index` can also come from `PARSEC_ORBIT_INDEX`. Locations are files or `http(s)` URLs.

## Publishing

The release workflow packages the plugins (`scripts/package.ps1`), builds `index.json` from the zips, and signs it if the repository has the secret `MARKET_SIGNING_KEY` (the content of the `.key` file made by `keygen`). Everything is attached to the GitHub release, so the index of a release is `https://github.com/<owner>/<repo>/releases/download/<tag>/index.json`. Without the secret the index is published unsigned. Generating and guarding the real key is the maintainer's job; the repository contains none.

## Not (yet) provided

* The `parsec-orbit.market` interface (install source and update metadata of a plugin to other plugins) is reserved; the metadata is the `market.json` file for now.
* No automatic updates, no in-client UI, no revocation list (a leaked key is dealt with by changing the key list).
* No version constraints in `requires` (see the [manifest](plugin-manifest.md)).
