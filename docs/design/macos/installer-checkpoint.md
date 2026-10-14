# M1b transactional installer checkpoint

Date: 2026-10-14. Host: macOS 27.0 (26A428), arm64.
**M1b is implemented and scratch-measured.** The initial 2026-10-14
measurements used toy bundles. The 2026-10-15 follow-up below also applies
and restores a disposable real vendor copy; the original-path roundtrip is recorded separately in the
[signing checkpoint](signing-checkpoint.md). The [phase
exit gate](../macos-phases.md#m1b---transactional-applyrestore) asks for
scratch apply/re-apply/uninstall and failure/interruption recovery, an exact
pristine restore and refusals without mutation; the measured evidence below
covers that scope and nothing beyond it.

## Implemented boundary

- `crates/parsec-orbit-installer-macos/` (lib `parsec_orbit_installer`, binary
  `parsec-orbit-installer-macos`) owns the mutation side: classification,
  backup, staging, the Mach-O patch, signing, verification, swap, preseed,
  uninstall and the transaction journal. It never escalates privileges, never
  touches a path the caller did not select, and refuses a running app.
- `crates/parsec-orbit-macos-state/` hosts the M1a seed transaction that used
  to live in the loader crate. The loader re-exports `seed`/`SeedReport`
  unchanged; the installer depends on this rlib so it never links the loader
  `cdylib` (two crates of the same crate-type would collide on the output
  file name).
- Portable modules (classification, digest trees, journal, Mach-O parsing)
  build and unit-test on every host. `codesign`, `ditto`, `plutil` and
  `libproc` calls live in `platform.rs` and return errors on non-macOS hosts;
  the Windows job is a compile/unit cross-check, not runtime evidence.
- The shipped stub whitelist contains exactly the stub measured by the M1a
  acceptance runs (`785f985d...be76d`, Parsec 150.101a/104a arm64). An
  unknown stub refuses before any change, and a re-apply is accepted only
  when the stub matches the recorded applied hash. Widening the list means
  measuring a new layout and hash first.

## Transaction

`apply` runs these steps in order, journaling each transition:

1. **Recover (pre-inspect).** A recorded `Preparing`/`BackedUp` transaction
   may have moved the app away, so it is rolled back before the selected path
   can be hashed.
2. **Inspect.** Canonical, non-symlink bundle directory; absolute Orbit root;
   the stub hash must be whitelisted or match the recorded applied stub; the
   bundle id is read from `Info.plist`; `libproc` must show no running process
   whose executable path equals the canonical stub; the loader input must be
   a regular arm64 `MH_DYLIB` (bounded to 64 MiB) and is hashed; the pristine tree snapshot is taken. Nothing
   here writes.
3. **Recover.** A recorded `Installed` state is accepted only when the whole
   tree still matches the applied snapshot *and* strict signature
   verification passes (already-applied no-op); a stale tree refuses without
   touching the pristine backup; `Preparing`/`BackedUp` roll back.
4. **Backup.** Strict `codesign --verify --strict` on the selected bundle,
   private Orbit root/install directory, same-volume check, `Preparing`
   manifest written (private temp file, fsync, rename), then the bundle is
   renamed to `installs/<id>/pristine` (`app@<12-hex stub prefix>`) and the
   state becomes `BackedUp`.
5. **Stage.** `ditto` the pristine tree to `installs/<id>/staged` and verify
   the copy against the recorded snapshot.
6. **Patch.** Insert exactly one `LC_LOAD_DYLIB`
   (`@executable_path/../Frameworks/libparsec_orbit_loader.dylib`) into the
   zeroed header padding immediately before the trailing, unique
   `LC_CODE_SIGNATURE` command. The header counts are updated; no instruction
   or file-backed section byte is rewritten.
7. **Sign loader.** Copy the loader to `Contents/Frameworks`, require the
   input hash, set `0755`, sign it ad-hoc, then write the schema-1
   `Contents/Resources/orbit-install.json` record (canonical `executable`,
   `loader`, `orbit_root`, `uid`). The signed loader hash is recorded
   separately (`applied_loader_sha256`); signing changes the file, so staged
   and installed verification compare against the signed copy.
8. **Sign bundle.** Write the `disable-library-validation` entitlements next
   to the record and sign the staged bundle last (inside-out; no `--deep`).
9. **Verify staged.** Strict signature verification; the dependency parses;
   every recorded file-backed section digest is re-measured
   after signing; the loader and record match their recorded values; the tree
   differs from pristine only in the allowlist (`parsecd`,
   `Contents/Frameworks`, the loader, the record, `_CodeSignature`).
10. **Swap.** Re-check that the stub is not running, require the app path to
    be free, rename `staged` into place and fsync the parent.
11. **Verify installed.** Tree equality with the verified staged snapshot,
    strict signature, stub/loader hashes and the record.
12. **Preseed (best effort).** Seed `<orbit_root>/home/.parsec` from the
    bundle's verified `Contents/Resources/skel`. A failure is a recorded
    warning; an existing state directory is never replaced (`Skipped`).
13. **Commit.** Record `Installed` in the manifest and journal.

A failure at any mutating step rolls back: the pristine backup is renamed
back, an occupying patched bundle is kept as `applied-removed[-n]` (never
deleted), staged/entitlement scratch is removed, the restored tree is
compared against the pristine snapshot, and the state is recorded as
`RolledBack`. A restored bundle that fails strict verification is a recorded
warning, not a second mutation. Refusals before the first mutation
(`Inspect`/`Recover`) leave the bundle untouched.

## On-disk layout

```text
<orbit_root>/installs/<bundle-id>@<12-hex>/
  manifest.json      state, canonical paths, input/applied hashes, trees
  journal.jsonl      append-only JSON lines; never rewritten
  pristine/          the only copy the installer restores from
  staged/            transient; removed on rollback
  disable-library-validation.entitlements   transient
  applied-removed[-n]/   a patched bundle moved aside, kept for inspection
```

States: `Preparing`, `BackedUp`, `Installed`, `RolledBack`, `Uninstalled`.
Installer-created record files are `0600` and the install directories `0700`;
the manifest is replaced atomically. Classification is `Missing`, `Invalid`,
`Pristine`, `Applied`, `Stale` or `Unsupported`; an unrecorded directory is
`Pristine` only when its stub hash is whitelisted, and `Installed` accepts
only a tree that still matches the applied snapshot.

## Uninstall

`uninstall` requires a recorded `Installed` state whose tree still matches
the applied snapshot. The patched bundle is renamed to `applied-removed`, the
pristine backup is renamed back, the restored tree is compared, and the
state becomes `Uninstalled`. Orbit data (`home/`, `plugins/`, `data/`,
`market/`, logs) is kept unless `--purge-data` is passed; `installs/` is
always kept so the record survives. A failure after the patched bundle was
removed restores the pristine bundle again. A stale bundle (extra or changed
files) refuses without mutation, so a vendor update cannot consume the
pristine backup.

## CLI

```sh
# Read-only classification; nothing is created.
parsec-orbit-installer-macos inspect <app> [--orbit-root <folder>] [--json]

# Transactional apply (refuses a running app, unknown stub or missing loader).
parsec-orbit-installer-macos apply <app> --loader <dylib> [--orbit-root <folder>]

# Restore the matching pristine bundle; data survives unless purged.
parsec-orbit-installer-macos uninstall <app> [--purge-data] [--orbit-root <folder>]

# Print the recorded state for the selected path.
parsec-orbit-installer-macos status <app> [--orbit-root <folder>] [--json]
```

The default Orbit root is
`~/Library/Application Support/ParsecOrbit`. The tool never invokes `sudo`,
never rewrites Dock/Spotlight/URL/login-item state and never runs a vendor
preinstall.

## Measured evidence (2026-10-14)

`cargo test -p parsec-orbit-installer-macos --test scratch_apply` builds
disposable arm64 toy bundles with `clang` (linker header padding, toy loader,
toy client payload, ad-hoc signature) in the test scratch directory and runs
the real installer binary/library against them. All eight tests pass:

| Test | Measured |
| --- | --- |
| round trip | apply patches/signs/swaps; private modes; full journal event sequence; isolated state seeded from the verified skel; running the patched stub actually loads the inserted dependency (the toy loader's constructor writes a marker); re-apply is a verified no-op; uninstall restores a tree equal to the pre-mutation snapshot and strict `codesign --verify --strict` passes on the restored toy bundle; data survives uninstall; re-apply keeps existing state |
| injected failure | a fault at `VerifyStaged` rolls back; tree equals pristine; strict signature passes; no state was seeded; the next ordinary apply completes |
| crafted interruptions | `BackedUp`-with-half-staged and staged-already-swapped crash states are recovered by the next apply; the interrupted bundle is kept as `applied-removed` |
| refusals | unknown stub and missing app refuse at `Inspect` before creating any state; tree unchanged |
| stale app | an extra file makes re-apply refuse at `Recover` and uninstall refuse at `CheckApplied`; the pristine backup is untouched |
| running app | with the stub running (`TOY_STUB_SLEEP`), apply refuses at `Inspect`; after the identity-verified child exits, apply succeeds |
| preseed warning | a bundle without `skel` records a preseed warning instead of rolling back; uninstall still restores |
| uninstall failure | an injected fault at `VerifyRestored` restores the pristine bundle |

Surrounding validations on the same tree: `cargo test --workspace --locked`
(60 suites, exit 0, log `/tmp/parsec-orbit-m1b-workspace-tests.log`),
`cargo check --workspace --all-targets --locked`, `cargo fmt --all -- --
check` and clippy `-D warnings` for the installer/state/loader crates all
pass, as do the three macOS Python suites (`test-macos-bootstrap-audit.py`,
`test-macos-accept-bootstrap-audit.py`, `test-macos-keychain-canary.py`, no
Security calls), `py_compile` and `git diff --check`. A
`x86_64-pc-windows-msvc --all-targets` check for the same three crates passes
as a compile cross-check only. The release loader (no `harness`) rebuilt
after the seed move: **571,472 B**, SHA-256
`b278d7b8c4b68e720a03d45d54c7c55433e486addbf35e00d6a97ce4d377f5b0`
(571,504 B / `cbccb4f9...` before the move).

## Historical limits of the 2026-10-14 measurement

- No real vendor bundle was applied, and `/Applications/Parsec.app` was not
  selected or mutated. The whitelisted vendor stub hash is measured input
  recognition, not a measured apply.
- Restore equality covers bytes, entry set, kind, POSIX permission bits,
  size and symlink targets. Modification times, owners, ACLs, extended
  attributes and resource forks are not in the snapshot and `ditto`'s
  fidelity for them is unverified.
- Running-app detection compares `libproc` executable paths with the
  canonical stub; a process that exec'd a replaced copy or whose path
  changed is not covered. No unrelated process is ever signalled.
- The install path requires write permission the caller already has; the
  `/Applications` permission request, quarantine/Gatekeeper and signing
  identity route are M1c work. `spctl` rejection of ad-hoc copies is
  unchanged from the M1a record.
- Concurrent vendor updates during a transaction, disk-full behaviour, and
  whole-app replacement detection beyond the stale-tree refusal are not
  measured. The pre-swap re-check of the running stub is the only
  concurrency guard.
- The preseed removal of the measured empty-state first-launch exit 2 has
  not been re-measured against the real vendor client; the earlier M1a
  direct-exec run on empty state is still the last vendor observation.

## Reproduction

```sh
cargo test -p parsec-orbit-installer-macos --test scratch_apply
cargo test --workspace --locked

# On a disposable copy only, never /Applications during this phase:
target/debug/parsec-orbit-installer-macos inspect /path/to/copy/Parsec.app
target/debug/parsec-orbit-installer-macos apply /path/to/copy/Parsec.app \
  --loader target/release/libparsec_orbit_loader.dylib \
  --orbit-root /tmp/orbit-scratch
target/debug/parsec-orbit-installer-macos uninstall /path/to/copy/Parsec.app \
  --orbit-root /tmp/orbit-scratch
```

The installer refuses an unsupported stub, a running app, a stale tree or a
cross-volume Orbit root without touching the selected bundle; the refusal
messages name the failing step.

## Follow-up: official universal input and restore metadata (2026-10-15)

The whitelist is the hash of a **universal** official stub, whereas the
initial parser accepted only thin arm64. The installer now validates a
FAT32 slice table (extent, alignment, CPU/header agreement, uniqueness and
non-overlap), inserts the dependency only in arm64 and retains x86_64.
This is not x86_64 execution support. Ad-hoc `codesign` repacks the slices:
the official arm64 offset changed from 131072 to 114688 in the measurement.
Section digests are therefore CPU-bound and slice-relative; every recorded
file-backed section in both slices is rechecked after signing. FAT64 and
other unsupported layouts refuse.

Snapshots include the bundle root, Unix uid/gid and nanosecond mtime, macOS
ACL bytes and xattr name/value bytes (including resource forks and symlink
attributes without following the link). Old manifests compare only fields
they recorded. Restore checks are exact. `ditto` resets symlink mtimes on
this host; staging permits that transformation while preserving the pristine
rename backup. Post-signing checks permit mtimes only on the root,
`Contents`, `Contents/Resources`, and `Contents/MacOS` ancestor directories;
metadata changes elsewhere feed the existing changed-path allowlist.
Applied snapshots are exact after the swap. Birthtime, atime, inode numbers,
BSD flags and hardlink relationships are not claimed by this snapshot.

Apply and uninstall hold a private, no-follow `flock` sidecar in the app's
parent directory. It is keyed by the canonical selected app name, independent
of Orbit root, and kept after unlock to avoid inode-splitting races. Unknown
inputs still leave the app/state unchanged but may create this lock file.
The input tree is rechecked before backup. Recovery/rollback refuses to move
an occupying tree that does not match the recorded applied snapshot; both
that replacement and the pristine backup survive. These checks serialize
cooperating installers and detect the injected replacement cases; they do
not lock the vendor updater or prove absence of every filesystem race.

Measured tests include the prior eight scenarios, competing apply/uninstall
using another state root, an external replacement during crash recovery,
an external replacement before swap, and a real universal toy roundtrip.
The separate metadata test covers root/directory/file/symlink xattrs, ACLs,
nanosecond timestamps, `ditto`, rename restoration and tamper detection.

The formal installer also applied the official vendor copy at
`/private/tmp/orbit-m1c-i30pesbu/Parsec.app`, keeping `tv.parsec.www`, with
the unchanged shipped stub whitelist. Preseed succeeded. Its first direct
launch needed **one attempt**, showed a Parsec window, loaded config/Boa/
fixture plugins (3 loaded, 0 skipped, 0 failed), and reinitialized after
101a-to-104a self-exec. Launch Services subsequently selected the same copy
path. Uninstall restored the recorded pristine metadata tree and strict
signature with no warnings; Orbit data survived. The stricter post-signing
metadata comparator was then exercised by re-applying the restored copy.
These are disposable-copy measurements, not existing Dock/Spotlight/TCC or
the original `/Applications` entry acceptance.

Evidence: `/tmp/orbit-continue-installer-tests.log`,
`/tmp/orbit-m1c-strict-vendor-apply.json`,
`/tmp/orbit-m1c-uninstall.json`, and the direct/open acceptance JSON under
the scratch path. No vendor preinstall, root, Keychain transaction or real
account operation was performed.


## Follow-up: protected-source staging and loader validation (2026-10-15)

A root-owned official app copied by an ordinary user transfers ownership to
that user (the source group may remain), and macOS may attach new
`com.apple.provenance`. Staging now permits exactly those copy transformations,
plus the measured symlink mtime reset. Existing xattrs, ACLs, content and other
metadata remain checked; no attributes are deleted. The verified copy receives
its own pristine snapshot. Post-signing verification compares against that
snapshot, so signing does not inherit relaxed owner/provenance exceptions.
The rename backup and final restored original still require exact original
ownership and metadata. A read-only root-owned source copy preflight passed
(`/tmp/orbit-root-ownership-preflight.log`).

`codesign` can sign an ordinary data file using xattrs. Signature success alone
therefore cannot validate a loader. Inspect now requires a structurally valid
arm64 dynamic library before backup. A regression first signs a data file and
then confirms refusal without bundle/state mutation. Installer verification is
now 21 unit tests, 14 scratch tests and the separate metadata roundtrip.

The ordinary-user original-path apply attempt was refused at the backup
rename with `Permission denied (13)` and recorded `RolledBack`; the original
remained pristine. The separately authorized administrator experiment then
used a fully verified candidate prepared with the real patch/sign/metadata
primitives and privileged renames only. It launched as the ordinary user and
restored the exact original. This measures original-path activation, not a
privileged production CLI workflow. End-user privilege handling remains
explicitly deferred.
