# Install Parsec Orbit from a macOS release

The network manager installs the published macOS arm64 release into the local
Parsec application. It requires macOS arm64, Python 3.9 or newer, and an
ordinary user account. The manager downloads and validates the complete release
before it calls the release's `scripts/install-macos.sh` wrapper. That wrapper
is the only activation path and prompts for `sudo` when its installer needs it.
The manager does not launch or stop Parsec.

Download `orbit-manager-macos.py` from the chosen GitHub release and run it
directly with Python; the shell wrapper is optional and expects that file beside
it. The bootstrap script itself is trusted through HTTPS, as on Windows. For a
published release containing the macOS assets:

```sh
curl --fail --location --proto '=https' --proto-redir '=https' \
  --output orbit-manager-macos.py \
  https://github.com/keiko233/parsec-orbit/releases/latest/download/orbit-manager-macos.py
python3 orbit-manager-macos.py install
```

Mac assets will become available only after a release built with this workflow
is published. From a repository checkout, use the shell entry point instead:

```sh
scripts/orbit-manager-macos.sh install
scripts/orbit-manager-macos.sh upgrade
scripts/orbit-manager-macos.sh uninstall
```

The default app is `/Applications/Parsec.app`; the default Orbit root is
`~/Library/Application Support/ParsecOrbit`. Pass `--app PATH` or
`--orbit-root PATH` to select another absolute path. Install refuses a managed
Orbit app, and upgrade requires one. Uninstall is offline: it validates the
cached release files, invokes that cached wrapper, and keeps Orbit plugins and
user data.

If Orbit was installed with the local `install-macos.sh` wrapper, use `upgrade`
to adopt that existing applied installation into network-manager state. A
restored or stale app is checked through the Rust installer's classification;
the presence of an old transaction record alone does not mean it is installed.

The default manifest is the latest GitHub release's `release-macos.json`.
GitHub's latest URL must redirect to a versioned release URL before the manager
resolves the manifest's verifier and bundle filenames. HTTPS redirects are
checked before following them. GitHub releases may redirect assets to
`release-assets.githubusercontent.com` or `objects.githubusercontent.com`; no
other cross-origin host is accepted. Every artifact remains pinned to the
versioned manifest URL selected at the start of the operation.

The manager uses the embedded official Ed25519 release public key. To use a
different trust set, pass `--keys /absolute/path/to/trusted-keys.txt`. The file
must be a regular, single-link file owned by the current user and contain
`ed25519 <base64 public key>` entries. The manager copies the key list to a
private temporary snapshot for verification and never edits the supplied file.

On the first signed install, the manager downloads `parsec-orbit-market` from
the pinned HTTPS release origin, checks its size and SHA-256 against the
manifest, and then runs its signature verifier. This first verifier is a
bootstrap trust boundary: before a signature can be checked, its authenticity
rests on HTTPS and the manifest's self-declared digest. It is cached as trusted
only after it verifies a signed release and the install or upgrade succeeds.
Later signed operations use that cached verified verifier to authenticate the
manifest before considering a replacement. An unsigned operation never
promotes a verifier into trusted cache state.

`--allow-unsigned` is an explicit install or upgrade escape hatch. It warns
that authenticity then relies only on HTTPS and the manifest's own size and
SHA-256 values. It also permits an unavailable detached signature. The
operation does not cache or trust a verifier from an
unsigned manifest. A later signed operation must bootstrap and verify a signed
verifier again if no previously signed verifier is cached.

The manager keeps private, versioned release caches and its state under
`.network-install-manager` inside the Orbit root. A cache is retained only
after the wrapper succeeds. Uninstall checks the cached metadata digest and
every required file digest before using the cached wrapper. Failed download,
signature, digest, ZIP-layout, inspection, or wrapper checks leave the prior
manager state and cache pointer intact. If the wrapper succeeds but a later
cache or state write fails, the manager preserves the release staging area and
`pending.json`, reports its path, and refuses another operation. If wrapper
completion is interrupted or unknown, it records that status and preserves the
same recovery data. Inspect the pending record, the app classification, and
the installer's transaction journal before taking manual recovery action; the
manager deliberately does not guess whether an interrupted wrapper completed.

The release ZIP must contain exactly the wrapper and console helpers, the
installer, loader, three official plugin libraries, and three macOS plugin
manifests. Extraction rejects traversal, duplicate names, links, special files,
and excessive compressed or expanded sizes. The manager's installer tests use
temporary directories, a loopback-only injected fetch fixture, and mocked
installer/wrapper processes. They do not modify `/Applications/Parsec.app`,
invoke `sudo`, or launch the Parsec vendor application.

The package uses ad-hoc code signing as part of installation. Ad-hoc signing
does not provide a Developer ID identity, notarization, or Gatekeeper approval.
