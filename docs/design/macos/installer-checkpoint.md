# M1b transactional installer checkpoint

Date: 2026-10-14. Host: macOS 27.0 (26A428), arm64.
**M1b is implemented and scratch-measured on disposable toy bundles.** The
installer has never been run against a real vendor `Parsec.app`; original-path
authorization, full backup-metadata fidelity and the signing/distribution
gates remain with M1c. The [phase
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
   a regular file and is hashed; the pristine tree snapshot is taken. Nothing
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

## Explicitly not measured

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
