# 0006 - Orbit Manager: PowerShell orchestration, Rust verification

## Context

Users need to install and update the loader and official plugins from a
release, without a checkout. The existing `scripts/install.ps1` works only
after `cargo build`, and the marketplace already has a tested trust model:
sha256 entries inside an ed25519-signed index, verified by
`parsec-orbit-market`.

The installer has to be a script users can run on stock Windows. Windows
PowerShell 5.1 has no Ed25519 API, so the script cannot verify signatures by
itself without reimplementing curve25519.

## Decision

One PowerShell script orchestrates download, selection, service handling and
file placement, and delegates every cryptographic check to the existing Rust
market tool. The release carries two signed files: the standard `index.json`
for plugins and a small `release.json` for the loader, the market tool and the
plugin-selection policy. A new `verify-any` market command checks
`release.json.sig` without trying to parse the manifest as an index.

The market tool itself is downloaded from the same pinned HTTPS URL and its
sha256 is checked against the manifest before it runs. On a first install that
check is TLS-only; once the tool has verified the manifests it is cached, and
every later upgrade is verified by the previously trusted copy before the new
copy replaces it. The official public key is embedded in the script so a new
user gets one obvious trust root.

## Consequences

* Security-sensitive code stays in Rust where Ed25519 and the download rules
  already exist and are tested; PowerShell stays small and auditable.
* First-run trust is honest: it is TLS to a pinned GitHub URL, upgraded to
  Ed25519 as soon as the manifest verifies.
* `parsec-orbit-market.exe`, `index.json` and `release.json` become release
  artifacts, and releases are unsigned until the maintainer creates the
  signing key.
* Official plugin management reuses the marketplace code path rather than a
  second, less-tested installer.
