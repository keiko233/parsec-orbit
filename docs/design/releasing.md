# Releasing

Releases are cut by the owner. Nothing in CI publishes to a registry on its own.

## GitHub release (loader and plugins)

Actions -> Publish -> Run workflow with the new version (for example `0.2.0`). The workflow bumps `[workspace.package] version` and the versions of the crates that name each other in `[workspace.dependencies]`, refreshes `Cargo.lock`, commits, tags `v0.2.0` and pushes; the tag triggers the Release workflow, which builds, verifies, packages and publishes the GitHub release.

`package.ps1` also publishes the market tool, a signed `index.json` and a
signed `release.json` for the [network installer](network-installer.md). Set
the `MARKET_SIGNING_KEY` action secret to the content of the `.key` file made
by `parsec-orbit-market keygen`, and put the matching public `ed25519 ...`
line into the `OfficialEd25519PublicKey` constant in
`scripts/orbit-manager.ps1` before the release is cut. Releases without the
secret still build, but their manifests are unsigned and the installer needs
`-AllowUnsigned`.

## crates.io (SDK)

After the GitHub release, from a clean checkout of the tag, in this order (each crate must be visible in the index before the next one is published):

```powershell
cargo publish -p parsec-orbit-plugin-abi
cargo publish -p parsec-orbit-plugin-sdk
cargo publish -p parsec-orbit-plugin-sdk-std
```

Before the first publish: `cargo package -p parsec-orbit-plugin-abi -p parsec-orbit-plugin-sdk -p parsec-orbit-plugin-sdk-std` (CI runs it on every push) and a read of each crate's README on the packaged tarball (`cargo package --list`). The names were free on 2026-10-12; check again before publishing.

An ABI change that bumps `ABI_VERSION` is a major bump of all three crates and is recorded in the [roadmap](roadmap.md).

## npm (JavaScript SDK)

`@parsec-orbit/sdk` is released the same way once it exists (phase 4); see [spec/js-sdk.md](../spec/js-sdk.md).
