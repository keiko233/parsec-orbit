# Registry

Status: partly implemented. The library (`crates/parsec-orbit-registry`) reads, checks and keeps the format-1 index fields below, the cache, `trusted-keys.toml` and the `host.json` model. The `manifest/` source tree and the release tool (`tools/parsec-orbit-release`) exist; the tool's `registry` commands produce the generated files. Not yet implemented: the CI release job that runs them; `scripts/` and `.github/` still produce `release.json` and the `parsec-orbit-market index` output until that cutover. The base rules, the package format and installation are in [marketplace.md](marketplace.md).

Conventions: a document has an integer `format` (1); readers ignore unknown keys; an unknown `format` is refused. Locations are `https://` or relative to the document, without `..`. Hashes are lowercase hex sha256.

## Index fields added to format 1

`index.json` keeps `format` 1. Each addition is optional and is left out when unset, so an index without them is unchanged.

| Level | Field | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| index | `serial` | integer | absent (counts as 0) | Rises with every change of content. See [rollback](#serial-and-rollback). |
| index | `generated` | RFC 3339 string | absent | When the index was built. Not checked. |
| plugin | `role` | `"lib"` or `"plugin"` | `"plugin"` | Any other value refuses the index. |
| plugin | `summary`, `category`, `homepage`, `group` | string | absent | Presentation. Read and kept; the tool does not act on them. |
| plugin | `default`, `required`, `official` | boolean | `false` | Presentation and selection hints. Read and kept. |
| release | `platform` | `<os>-<arch>` | `windows-x86_64` | Lowercase letters and digits, `-`, then lowercase letters, digits and `_`. Other valid values are read but never selected by a tool that does not serve them. A malformed value refuses the index. |
| release | `published` | RFC 3339 date | absent | Informative. |
| release | `yanked` | boolean | `false` | See [yanked](#yanked-releases). |
| release | `mirrors` | list of locations | `[]` | See [mirrors](#mirrors). |

## Yanked releases

A yanked release is never chosen when no version is asked for. An exact version still selects it, so a pinned install keeps working.

## Mirrors

`mirrors` are other locations of the package, resolved like `url` and tried in order after it. A location is accepted when its size and sha256 match the index; the first one that does is installed. Bytes that fail either check are discarded. When no location is valid, the error names every location tried.

## Serial and rollback

A client that has verified an index keeps it (see [cache](#cache)). A later index for the same location whose `serial` is lower than the cached serial is refused. A missing serial counts as 0. The check runs only when a cache is configured, and only for indexes a trusted key signed.

## Cache

With a cache directory, a verified index is stored in `<cache>/<sha256 of the location>/` as:

| File | Content |
| --- | --- |
| `index.json` | The index, byte for byte as verified. |
| `index.json.sig` | Its signature. |
| `etag` | The sha256 of `index.json`. The transport does not expose response headers, so the digest stands in for an ETag. |

Each file is written to a temporary name and renamed over the old one. An index is stored only after its signature checks; an unsigned index (accepted with `--allow-unsigned`) is never stored. The command line does not enable the cache yet.

## `host.json`

The loader and installer of each release, signed like the index. It replaces `release.json` and `release-macos.json`. Model and checks: `crates/parsec-orbit-registry/src/host.rs`.

```json
{
  "format": 1,
  "serial": 3,
  "generated": "2026-11-01T00:00:00Z",
  "latest": "0.2.0",
  "releases": [{
    "version": "0.2.0",
    "api": 1,
    "platform": "windows-x86_64",
    "loader": { "url": "bcrypt.dll", "sha256": "<64 hex digits>", "size": 374272 },
    "installer": { "url": "orbit.exe", "sha256": "<64 hex digits>", "size": 1234 }
  }]
}
```

| Field | Rule |
| --- | --- |
| `format` | Must be 1. |
| `serial`, `generated` | Optional; as in the index. |
| `latest` | A semantic version, equal to the version of one release. |
| `releases[].version` | A semantic version. |
| `releases[].api` | Loader ABI version the release needs. Required. |
| `releases[].platform` | Required; same rule as the index. |
| `loader`, `installer` | `url`, `sha256` and `size`: `size` is 1 to 256 MiB in bytes. |
| `releases` | `(version, platform)` is unique. |

Selection: the exact version when one is asked for; otherwise the newest version for the platform that the loader API can run and that is not a pre-release.

## `trusted-keys.toml`

The user's key list, at `<Orbit root>/market/trusted-keys.toml`. One table per key:

```toml
[[key]]
algorithm = "ed25519"
public = "<base64 public key>"
comment = "the maintainers"   # optional
```

* Unknown keys are ignored. An unknown `algorithm`, a key that is not a valid base64 ed25519 public key, or malformed TOML refuses the list.
* A `--keys` file whose name ends in `.toml` is read this way. Any other `--keys` file is read in the older line format, `ed25519 <base64 public key> [comment]`, one per line.
* When `trusted-keys.toml` does not exist but `trusted-keys.txt` does, the text list is converted once: the TOML file is written beside it and the text file is left as it was. After that the text file is not read; edits to it have no effect.

## Publishing: `parsec-orbit-release`

The tool reads and writes the sources in `manifest/` ([manifest/README.md](../../manifest/README.md)) and the release assets. Exit status: 0 success, 1 error (a `registry check` that finds problems is an error), 2 usage.

| Command | Does |
| --- | --- |
| `version [--workspace DIR]` | Prints `[workspace.package] version` of the workspace. |
| `package --platform ID --out DIR [--workspace DIR] [--target-dir DIR]` | Zips each official module whose `[build]` lists the platform as `<crate package name>-<platform>.zip`, with one top folder named by the id. The staged `plugin.toml` gets the workspace `version` and the platform's `kind` (`dll` or `dylib`) and `entry`; other lines are kept. Copies `README*.md`, the loader if it is built (`bcrypt.dll`, `libparsec_orbit_loader.dylib`) and writes `SHA256SUMS.txt`. The libraries come from `cargo build --release`; a missing one is named. |
| `registry check [--manifest DIR]` | Validates the sources offline, reporting each problem with its file. |
| `registry add --tag vX.Y.Z --assets DIR [--manifest DIR] [--repo owner/name]` | Appends a `[[release]]` to `plugins/<id>/releases.toml` for each plugin zip (URL `https://github.com/<repo>/releases/download/<tag>/<file>`, platform from the file name, `published` today) and to `host/releases.toml` for each loader with its installer. A host release needs both files; a loader alone adds nothing. A release already listed is refused, and nothing is written if any check fails. |
| `registry build [--manifest DIR] [--sign KEYFILE] [--dist DIR]` | Fetches every release, checks its size, sha256 and manifest (`id`, `version`, `kind` against the platform), and writes `index.json` and `host.json` in the format of this document. `serial` rises by one only when the content changes; an unchanged build leaves both files untouched. `--sign` writes `*.sig` over the exact bytes; `--dist DIR` reads each release from the file of its last URL name. A signature left from an older file is removed when the file changes unsigned. |
| `keygen`, `sign`, `verify`, `verify-any` | Forwarded to the `parsec-orbit-market` implementation in `parsec-orbit-registry`, so both tools sign and verify the same way. |
