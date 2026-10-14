# Local macOS arm64 installation

Quit Parsec and run `/bin/bash scripts/install-macos.sh` from this repository
as your ordinary signed-in user. The default target is `/Applications/Parsec.app`;
installation persists until explicitly uninstalled. Rust/Cargo and Apple's
command-line tools are required for the build. `--no-build` reuses prepared
`target/release` artifacts.

The wrapper builds only the Mac installer/loader and Config, Hooks, JS runtime.
It stages each Mac manifest and dylib, signs ad hoc and strictly verifies it
before replacing a plugin directory. Existing managed directories move to a
private `plugin-backups/` run folder; unrelated plugins are kept. An apply
failure rolls back the plugin swaps. A per-root wrapper lock serializes plugin
operations; the Rust per-app lock also serializes bundle transactions.

The script prompts for sudo for the installer binary, passing the invoking
UID and explicit user state root. The Rust process selects that user's groups
and effective UID before inspecting or creating state. The install record and
seeded private home belong to that user. External tools permanently discard
root real/effective/saved IDs before exec; ad-hoc signing needs no identity.
App/state/transaction parents are pinned with directory descriptors and
no-follow traversal; elevated renames resolve relative to those descriptors.
Only no-clobber bundle renames between the selected app and its private install
transaction, and creation of that selected app's lock file, may briefly regain
root. The untouched pristine backup retains its original ownership and metadata.
Never run the wrapper with sudo, and do not invoke root apply without
`--sudo-user` and an explicit `--orbit-root`.

Open Parsec normally after successful installation. It uses independent client
state under `~/Library/Application Support/ParsecOrbit/home`; no existing
credentials are copied, so you must log in yourself if needed. Logs are
`parsec-orbit.log` and `install-macos.log` under that Orbit root. Config/Boa/owned
import hooks have local measurements; account/session behavior and native
bitrate/WebView plugins remain unsupported or unverified as documented in the
[client checkpoint](../design/macos/client-checkpoint.md).

Restore preserves the original contents, ownership, modes, timestamps, ACLs
and all original attributes. macOS may add `com.apple.provenance` to the
root-owned backup. Only that addition on the bundle root is retained and
reported; it is not raw metadata equality, and no quarantine/provenance marker
is deleted. Existing-attribute changes and all other differences refuse.

To restore the recorded pristine bundle while keeping Orbit data:

```bash
/bin/bash scripts/install-macos.sh --uninstall
```

Use the same `--app` and `--orbit-root` selections for custom paths. Do not delete
`installs/`, move backups by hand or reinstall the vendor app over a patched
bundle and then assume the old backup is current. Unknown/stale/externally
changed bundles refuse; preserve both the current app and pristine backup for
inspection. The Rust journal records interruption and restore attempts.

A wrapper killed before cleanup may leave `.install-macos-lock`; check that no
installer is running before removing that empty lock directory and its `pid`
file. Do not remove an active lock. An interrupted plugin swap may leave the
old managed plugin in `plugin-backups/`; retain it and inspect `install-macos.log`
rather than deleting recovery files. The wrapper's plugin swaps are rollback
on ordinary failure, not a crash-atomic plugin journal.

Quarantine attributes are retained; no system protections or default URI/Dock
settings are changed. A strict local ad-hoc signature does not establish a
notarized download or Gatekeeper acceptance. Current signing, original-path
and remaining delivery evidence are in the
[signing checkpoint](../design/macos/signing-checkpoint.md).
