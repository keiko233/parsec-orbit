# macOS release tools

The macOS release is a separate arm64 distribution alongside the Windows assets.
It uses the existing transactional installer and local ad-hoc signatures; it
does not install Windows-only plugins or change Parsec's UI mode. The supported
plugin set is Config, Hooks and JS Runtime. Account, WebView and native bitrate
acceptance gates remain as recorded in [macos-phases.md](macos-phases.md).

## Packaging

Run `python3 scripts/package-macos.py` on macOS arm64. It builds only the loader,
installer, market verifier and the three supported plugins. `--no-build` uses
existing release artifacts; `--out` selects the output directory (default
`dist/macos`). Repository-owned executable copies are checked for arm64,
ad-hoc signed and strictly verified before archiving. Cargo artifacts and the
installed Parsec app are left untouched.

`--signing-key /absolute/path/release.key` uses the marketplace Ed25519 signing
format to sign `release-macos.json`. The manager embeds the same official public key as the Windows manager;
`--keys` selects a different trust file. No private key is shipped to users.
The Release workflow uses the existing `MARKET_SIGNING_KEY` secret when supplied.
An unsigned distribution is usable only through the manager's explicit
`--allow-unsigned` mode; a checksum is not a signature.

The output comprises:

- `release-macos.json` and its optional `.sig` signature;
- `parsec-orbit-market`, the native manifest verifier;
- `parsec-orbit-macos-arm64.zip`, with the installer, loader, supported plugins,
  macOS manifests, local installation wrapper and terminal viewer;
- `orbit-manager-macos.sh` and `orbit-manager-macos.py`, the bootstrap entry point;
- `console-macos.sh` and `console-macos.py`, standalone terminal viewer entry points;
- `SHA256SUMS-macos.txt`, for manual artifact checks.

The zip preserves a minimal checkout layout so the existing ordinary-user
`install-macos.sh --no-build` remains the single installation entry point.
The manager owns download, manifest verification and archive extraction; the
wrapper owns plugin staging/signing and delegates protected bundle operations
to the Rust installer. A failure must not silently convert to an unsigned
installation. Download/cache verification does not replace installer validation
of the selected vendor bundle.

## Terminal viewer

The console is an independent terminal process, not a window injected into
Parsec. It follows only the selected Orbit root and isolated client's logs;
launching or terminating the viewer does not launch or stop Parsec. It requires
Python 3, as does the network bootstrap. No compiler is needed on user machines.

## Delivery evidence

Owned fixtures validate log following and network deployment failures. Local
packaging validates the actual arm64 release artifacts and the signed manifest.
This is separate from a published GitHub release download and from Gatekeeper
acceptance of a quarantined ad-hoc bundle; those remain unverified until a real
distribution is published and tested.
