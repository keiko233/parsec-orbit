# M1a bootstrap implementation checkpoint

Date: 2026-10-13. Host: macOS 27.0 (26A428), arm64.
**M1a is measured complete on disposable copies** (acceptance evidence below);
this is still not a usable patched client. The [phase exit
gate](../macos-phases.md#m1a---minimal-production-bootstrap) was met with
fresh-copy direct-exec and Launch Services launches: window, native
hello/registry, self-exec re-initialization, isolation-negative gates and the
recorded audit findings. Isolation remains a constructor-installed
custom-SBPL boundary with its documented limits, not a kernel-level
guarantee.

## Implemented boundary

- A minimal dependency constructor identifies the canonical executable and
  its own dylib. Ordinary hosts remain inactive without reading installation
  records, creating Orbit state/log files or starting Orbit workers.
- The sibling `Contents/MacOS/parsecd` requires a strict schema-1
  `Contents/Resources/orbit-install.json` containing canonical `executable`,
  `loader`, `orbit_root` and the current `uid`. Roots/home must be private,
  current-user-owned directories. Records reject symlinks, special files,
  unsafe permissions, unsupported schemas and unknown fields. Ancestors are
  validated; protected state/Keychain exclusions are lexical and do not read
  real account paths. These checks do not replace a kernel isolation backend
  or authenticate bundle provenance.
- NSS callbacks forward inactive calls through this image's original imports.
  Active original implementations are resolved only from Apple's NSS images,
  with bounded Mach-O export-trie parsing rather than recursive `dlsym`.
  `getpwuid_r` preserves NSS errors/UID misses and keeps the replacement in
  the caller's buffer; `ERANGE` sets the result to null. `getpwuid` copies
  every string into thread-local storage and preserves NSS's errno.
  Only the explicit `harness` feature can install a replacement home now.
- `seed(skel, home)` checks real SHA-256 against strictly decoded appdata,
  refuses unsafe source paths and nonprivate home, stages private files,
  syncs files/directories and commits with `RENAME_EXCL`. Concurrent attempts
  cannot replace an existing `.parsec`, including an empty directory or
  dangling symlink. Preexisting staging directories are never deleted.
  Manifest consistency is not authenticated vendor provenance; the M1b
  caller must verify the approved bundle/version/signature first.
- The seed layout is `home/.parsec/{appdata.json,parsecd-*.dylib}` as measured
  in the earlier smoke test, correcting the old static `.parsec/skel` claim.
- The readiness **policy** requires an isolated mapped image and a matching
  release announcement appended during the current launch. It has a polling
  deadline, stops on permanent failures and caps each sleep at the remaining
  time. Probe callbacks must be nonblocking. The policy is now wired to the
  live `dyld` image snapshot (`original.rs::mapped_images`) and to the real
  vendor log at `<orbit_root>/home/.parsec/log.txt` through `runtime.rs`; the
  product acceptance run below measures the binding at runtime.

- A private log observer now snapshots the pre-launch length/inode, pins the
  directory descriptor and opens the safe basename with `openat/O_NOFOLLOW`.
  It rejects hardlinks, replacement, truncation, unsafe permissions and byte
  overflow; reads are capped at 4 KiB per call and 64 KiB retained. Only new
  complete UTF-8 lines feed the readiness policy. Split lines remain buffered;
  malformed complete UTF-8 permanently fails. Nine scratch tests include stale
  release text, directory/inode replacement, split UTF-8 and partial records.
  Same-inode rewrites, unrelated writers and disk-I/O latency are not fully
  detected/bounded. The real vendor log source and deferred worker are
  connected (acceptance below). The measured vendor-created `log.txt` is
  `0644` inside the validated `0700` state root; the observer originally
  rejected any group/other bit on the file, which permanently failed
  readiness on every real launch, and now refuses only group/other *write*
  (`022`) while keeping the root, ownership, inode, hardlink and pinned-open
  checks.

The production constructor now **activates** a selected installation whose
record passes, in this order, with every step failing closed (exit **78**
before vendor main, message `Parsec Orbit: ...`): shared-persistent redirect
(`redirect.rs`), custom-SBPL deny profile with the sentinel gate
(`isolation.rs`), current-UID home replacement armed and verified in-process
against both the interposed and the real NSS entry (`runtime.rs`), then the
single deferred worker starts. Only then does the constructor return and
vendor main run. A launch that fails any step still exits 78 pre-main and
names the failing step. The constructor does not set `HOME` and does not seed
state: the vendor stub seeds its own isolated state, as measured below. The
earlier core/SDK non-Parsec harness remains available for M0 tests.

- `isolation.rs` builds the measured profile: `(version 1)`, `(allow
  default)`, `file-read*`/`file-write*` subpath denies for the four protected
  roots, a `network-outbound` unix-socket deny under each root (the measured
  addition after a connect under a denied path succeeded), and a literal
  `file-read*` deny on a per-launch sentinel. Roots are validated absolute,
  deduplicated and SBPL-quoted. `install` creates the sentinel with
  `O_EXCL`/`0600`, calls the raw `sandbox_init(profile, 0)`, then requires a
  post-install sentinel open to fail with `EPERM`/`EACCES`; any other outcome
  removes the sentinel and fails the gate. Mach stays `(allow default)` for
  the measured reason below.
- `redirect.rs` interposes `open`, `openat`, `fopen`, `stat`, `lstat`,
  `access`, `mkdir`, `rename` and `opendir` with `__DATA,__interpose` and
  rewrites only the measured shared-persistent prefix
  (`/Users/Shared/.parsec-persistent`) to
  `<orbit_root>/home/.parsec-persistent`, at component boundaries and without
  `..` components. Everything else falls through to the real path, where the
  profile denies it; the redirect is not the security boundary. Darwin arm64
  passes the variadic `open`/`openat` mode on the stack, so those two use
  `global_asm!` trampolines that load it into `x2`/`x3` before tail-branching
  to the Rust implementations; a scratch probe measured the wrong mode before
  that fix.

## Dependency policy and measured checks

Use a **strong `LC_LOAD_DYLIB`** dependency. A weak missing-loader fallback
could enter the vendor client without isolation and use real state, which
violates the host contract. The earlier strong/weak matrix is recorded in
[native-dependency-poc.md](native-dependency-poc.md#fault-matrix-strong-and-weak-dependencies).
Neither policy promises recovery from invalid signatures, constructor faults
or native plugin crashes.

The checked-in `bootstrap_e2e.rs` compiles disposable C images, signs them
ad-hoc with Hardened Runtime and only disable-LV, verifies the signatures,
and exercises inactive forwarding, cross-image NSS replacement, buffer errors,
UID-miss semantics, selected-host rejection and missing/corrupt strong
dependencies. The tests execute no vendor client or account state.

Checkpoint validation: the macOS loader library's **94 tests** pass,
`cargo test --workspace --locked` and `cargo check --workspace --all-targets
--locked` pass, formatting/diff checks pass, and the loader-only Clippy check
with `--all-targets --no-deps -D warnings` passes. The new dependency/NSS test
and the existing std/no_std registry e2e pass, and the new owned
`redirect_e2e` fixture verifies the interposed prefix rewrite in both the
installed and control variants. The loader's Windows target
check passes; a whole-workspace Windows cross-check is blocked by the host's
missing MSVC C headers (`ring`: `assert.h` not found). Unmodified core/devtools
lints also block workspace-wide strict Clippy. None of these cross-checks
replaces the retained Windows CI runtime job.

**Pre-flip negative gate (historical).** A locally prepared copy of the
verified `785f985d...be76d` vendor stub was re-run while the product still
refused valid records, and exited 78 with `M1a isolation backend is not
validated; refusing client execution (guard installed and verified)`. The
sentinel-gate success branch therefore ran: the custom profile installed, the
post-install sentinel probe failed as required, and the sentinel file was
removed again. The copy's outer strict signature passed, the bundled official
client retained its hash (`9cce...cdd5`), the scratch Orbit root contained only
the empty `home/` directory afterwards, and the controlled process was bound
to PID, microsecond start identity and canonical executable path
(`orbit-m1a-audit-rl09m3av`, `run-ymmaqff9`, PID 68271). This measures the
product constructor's negative gate with the real guard, not a login window,
plugin activation or a syscall-complete isolation proof (the root file-trace
permissions remain spent and were not reused).

### Guarded activation acceptance (2026-10-14)

The constructor flip was accepted with two ordinary-user runs on disposable
copies of the verified `785f985d...be76d` stub, staged by
`scripts/macos-stage-bootstrap-audit.py --acceptance` and driven by
`scripts/macos-accept-bootstrap-audit.py`. Both plans staged the owned
`com.example.mac-e2e` fixture plugin (`abecdddf...`, `kind = "dylib"`) whose
script registers `com.example.mac-e2e` and queries `parsec-orbit.client`; the
runner re-validates plan hashes, compiles a `libproc`/CoreGraphics helper,
launches only that copy, waits for readiness/plugin evidence, stops only the
launched PID after re-verifying its microsecond start identity, and compares
the pristine inputs and real account metadata before/after.

- **Direct exec, empty isolated state** (`orbit-m1a-audit-usbtwa7s`,
  `acceptance-3eee5cee`, loader `37cd6b9d...`, staged executable
  `5123462f...`):
  - attempt 1 (PID 90324) exited **2** without reaching the client - the
    empty-state first-launch quirk already measured in the PoC. The stub had
    seeded `home/.parsec` from the bundle skel (`9cce8505...`).
  - attempt 2 (PID 90347, start identity `1791903867 78152`) reached vendor
    main and the loader log shows `client 150-101a detected`, the fixture
    `register com.example.mac-e2e -> 0`, `query parsec-orbit.client -> 1.0
    (16 bytes)` and `ready: 1 loaded, 0 skipped, 0 failed`. The client then
    self-updated, re-executed the same stub and the fresh constructor
    re-initialized: a second `client 150-104a detected`, a second fixture
    registration/query and a second ready line, all under PID 90347.
  - the vendor log recorded `Parsec release (150-101a, ..., Loader: 14)` and
    `(150-104a, ..., Loader: 14)`; the updater left
    `parsecd-150-104a.dylib` (`eed68639...`) in the isolated state.
  - the runner stopped the owned PID with an identity-verified SIGTERM
    (`-15`); a CGWindowList snapshot attributed an on-screen window named
    `Parsec` to that process.
- **Launch Services `open -n`, preseeded verified skel**
  (`orbit-m1a-audit-rx7nlkr3`, `acceptance-bf88eb83`, staged executable
  `433ba3ea...`): one launch, discovered by unique executable path (exactly
  one match), PID 90477, start identity `1791903892 186859`. The first launch
  reached the client, produced both 101a and 104a detections with both plugin
  scans, an on-screen `Parsec` window, and was stopped by identity-verified
  SIGTERM.
- **Isolation observations from both runs**: all vendor state was created
  inside `<orbit_root>/home/.parsec` (0700) with the client dylib,
  `appdata.json`, `config.json`, `hotkey.json`, `cc0.txt` and `log.txt`; the
  redirected shared-persistent target
  `<orbit_root>/home/.parsec-persistent` received the vendor's `devid.bin` and
  `status.json`; `os.lstat` metadata of real `~/.parsec`,
  `~/.parsec-persistent`, `/Users/Shared/.parsec` and
  `/Users/Shared/.parsec-persistent` was byte-identical before/after, and the
  staged payload hashes and the `/Applications` stub hash were unchanged.
  This is runtime observation of the redirect and the private state, not a
  syscall-complete read/write audit (the root file-trace permissions remain
  spent).
- **Readiness permission fix, measured**: the first post-flip run
  (`orbit-m1a-audit-zk4tfb_5`, `acceptance-920333a4`) measured that the
  vendor creates `log.txt` as `0644` inside the `0700` state root, and the
  observer's owner-only bit rule permanently failed readiness
  (`log must be current-user-owned and private`) so no plugin scan ran even
  though the client itself ran and updated. The rule now refuses only
  group/other write (`022`); the reruns above are the post-fix measurements.

## Reproduce the controlled startup audit

These scripts are development fixtures, **not the M1b installer**:

```sh
cargo build -p parsec-orbit-loader-macos --release --locked
python3 scripts/macos-stage-bootstrap-audit.py \
  --bundle /Applications/Parsec.app \
  --loader target/release/libparsec_orbit_loader.dylib
python3 scripts/macos-run-bootstrap-audit.py /absolute/path/to/audit-plan.json
```

The guarded acceptance contract has its own staging mode and neutral runner
(no sudo, ordinary user only). `--preseed-skel` copies the verified skel into
the private state first, which the PoC measured as the shape whose *first*
launch reaches the client:

```sh
python3 scripts/macos-stage-bootstrap-audit.py \
  --bundle /Applications/Parsec.app \
  --loader target/release/libparsec_orbit_loader.dylib \
  --acceptance --preseed-skel \
  --plugin target/debug/libparsec_orbit_fixture_plugin.dylib
python3 scripts/macos-accept-bootstrap-audit.py \
  /absolute/path/to/audit-plan.json --run --launch open
```

`--launch direct` (the default) starts the staged executable itself;
`--launch open` goes through Launch Services and discovers the launched PID by
the unique staged executable path. The runner refuses root, non-acceptance
plans, escaping payloads and digest mismatches, and it reports
`unmeasured`/`failures` separately in `acceptance-result.json`.

The preparer accepts only the recorded pristine stub hash/layout, creates a
fresh private copy with a distinct bundle id, preserves vendor instruction
sections and client bytes, retains quarantine, adds the loader/record and
signs inside-out without `--deep`. It never launches or changes the source
installation. The runner accepts only the prepared fail-closed payload hashes,
uses a private executable gate before exec, records PID/start/path, has bounded
cleanup and never signals by process name. Each run gets a separate evidence
directory, so a failed trace cannot reuse a previous successful report.

For the explicitly authorized **five-second PID-only file trace**, authenticate
in the same ordinary-user terminal, then run:

```sh
sudo -v
python3 scripts/macos-run-bootstrap-audit.py /absolute/path/to/audit-plan.json --trace
```

Only `fs_usage` runs as root. The controller and vendor copy remain ordinary
user processes. If noninteractive sudo authentication fails, the runner
withholds exec and stops only its owned gate process. The initial tool-side
attempt returned `sudo: a password is required`. A subsequent authorized
terminal run (`run-a49jtlil`, PID 77231, start identity `1791884052 576492`)
observed exit 78 and the expected refusal, but its `fs-usage.log` contains
**only one exit event**. Thus the negative gate passed; startup file capture
did not establish usable evidence. The old `root_trace: true` field merely
recorded that tracing was requested, not that capture succeeded. No password
belongs in repository files, chat or command arguments. No protection bypass
or broader privileged service is proposed.

The revised runner requires a known `open`/path event for a unique private
probe read by the same owned PID **before exec**. It retries that probe for at
most three seconds within the five-second trace. If capture is not observed,
exec remains withheld. After exec, a path event for `orbit-install.json` must
also be present, the tracer must exit successfully, and no recognized loss
diagnostic may occur. An incomplete trace receives a separate `file_trace`
assessment and nonzero runner exit even when the negative gate passes.
This fixes the exit-only false-positive; it is not proof that every syscall
was captured, and it cannot explain the earlier missing events without a
new measurement. The revised tool-side attempt (`run-h_fi94kj`) encountered
the same sudo authentication failure and recorded `exec_released: false`;
the revised root capture remains unmeasured until terminal authentication.

Eight ordinary-user regression tests (`python3 scripts/test-macos-bootstrap-audit.py`)
cover exit-only evidence, both sides of exec, stale probes, trace diagnostics,
the compiled gate's EOF/probe failure/symlink/explicit-go behavior, and
post-exec suspension before a constructor runs plus cancellation/resumption. A new
no-trace disposable launch (`run-k6ekfwe6`) again exited 78 as expected.
Apple's [fs_usage source](https://github.com/apple-oss-distributions/system_cmds/blob/main/fs_usage/fs_usage.c)
configures a numeric PID filter, flushes live event output, and reports buffer
overruns; observing the probe is stronger than an arbitrary attach sleep.
File traces also do not prove
Keychain/Mach/socket IPC isolation. This first audit keeps client execution
blocked; later client probes need a validated guard and separate safe runtime
procedure.

### Observed loss across exec and post-exec calibration

The authorized revised terminal run (`run-bxm4d9jp`, PID 88081, start identity
`1791886137 119391`) captured repeated probe opens/reads/closes in the helper.
After exec it captured only the exit, while stderr confirmed the production
refusal and exit 78. `file_trace.status` correctly remained `incomplete`:
`pre_exec_probe_observed: true`, `installation_record_observed: false`.
Thus tracer attachment works; capture of the selected executable does not.

Apple's published [kdebug process filtering](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kdebug.c)
checks `proc.p_kdebug`, while [fork/exec process allocation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_fork.c)
creates a zeroed object and retains PID identifiers for exec. The marker is
outside the copied `p_forkcopy` group in
[proc_internal.h](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_internal.h).
This is a source-derived explanation consistent with the observations, not a
measured kernel root cause on this host's xnu-13432.1.9 build.

`scripts/macos-run-suspended-bootstrap-audit.py` provides a separate calibration
route: `POSIX_SPAWN_START_SUSPENDED` installs the actual executable before user
code, the controller verifies its PID/start/path and stopped state, and only
then starts the same five-second PID-only tracer and resumes that child. It
never enables vendor client execution. An ordinary-user run
(`suspended-run-0b_9o2x1`) observed the usual exit 78. Its compiled regression
fixture confirms the target constructor does not run before resume, and
cancellation also leaves its constructor marker absent.

The authorized terminal run `suspended-run-zbu3r5u4` (PID 94572, start identity
`1791887480 573004`) captured **231 events**, including dyld/system-library
accesses, loader loading, installation-record open/read, private root checks,
and the final exit 78. Every emitted row was attributed to the target thread.
The tracer exited 0 with no recognized event-loss diagnostic and the report
correctly records `startup-paths-observed`. No listed real `.parsec`, shared
`.parsec-persistent`, or Keychain pathname occurred in this captured prefix.
Home-directory metadata and global Apple logging preferences were observed;
the latter are not Parsec account state. This measures recovery of capture
across the launcher transition, not complete isolation or client runtime.

This route has an explicit attach warm-up rather than a same-PID pre-exec
probe, because exec has already completed. It requires the actual installation
record event after resume, rejects failed/lossy traces, and never claims full
capture or complete isolation. Every additional privileged run still requires
its concrete authorization; no protection bypass or process-name filter is
proposed.

```sh
python3 scripts/macos-run-suspended-bootstrap-audit.py \
  /absolute/path/to/audit-plan.json --trace
```

## Remaining acceptance work

Audit and enforce shared `.parsec-persistent`/shared `.parsec`, descriptor and
relative-path APIs (`open/openat`, rename/remove, directory/lock operations),
symlink/mount aliases, Library/Keychain access and IPC. The old partial mapper
is not shipped as this backend, and a constructor-installed development sandbox
does not cover earlier dyld initialization. The SDK's `sandbox.h` explicitly
marks the historical API unsupported; choosing a production mechanism remains
an evidence-backed decision.

The [isolation mechanism assessment](isolation-mechanism-assessment.md)
separates the observed constructor sandbox from SDK support warnings and the
App Sandbox/Endpoint Security alternatives. Its 2026-10-13 extended scratch
measurements resolved the direction and the remaining owned-fixture items: the
66-probe custom-SBPL fixture measured 36 EPERM denials (including
`stat`/`lstat`/`access`, mutation, alias and the now-denied unix-socket connect
probes), 23 successes, six dependent skips and one `ELOOP`; its baseline
control measured 65 successes and the same `ELOOP`; a sentinel gate verified
`sandbox_init_rc=0` with a post-install sentinel `EPERM`; and an earlier
dependency constructor read a protected scratch file before the guard
installed the policy. The `--ipc allowlist` run measured that the allowlist
form permits an owned `CFMessagePort` name while a blanket `mach-lookup` deny
also blocks a standard system service (`kern_return` 1100), so the shipped
profile keeps `(allow default)` for Mach. The extended 40-probe App Sandbox
fixture measured 17 EPERM denials but still allows
`stat`/`lstat`/`access(F_OK)`/`realpath` metadata, and a nested custom profile
is refused. The custom SBPL profile with the sentinel gate is selected for
`isolation.rs` and implemented, fail-closed, and the acceptance runs below
closed the two remaining runtime-observation items measured so far: the
shared-persistent redirect against the real client and the client/service
startup path under the constructor-installed profile. The vendor's required
Mach service set remains an observation item for M2, with `(allow default)`
as the measured interim. No new backend is enabled by this checkpoint.

The [Keychain canary](keychain-canary-plan.md) then measured two clients in
two separate transactions: the earlier App Sandbox client, and a custom-SBPL
client with the sentinel gate verified. Both returned `-25293` for the two
owned legacy dummy items, including the ones explicitly trusting the client.
The controller verified both fake payloads, deleted both items, and confirmed
absence with exact queries. This does not establish a complete Keychain
boundary or enable vendor execution. Five mocked cleanup/protocol tests pass
without Security API calls.

The redirect, the isolation gate, the guarded constructor and the deferred
runtime ran against the vendor client in the two acceptance runs above; the
M1a exit gate (fresh-copy LS first-launch window, native hello/registry,
self-exec re-initialization, isolation-negative gates, recorded audit
findings) is measured on disposable copies. Remaining for later phases: M1b
transactional apply/restore and the atomic preseed that removes the measured
empty-state first-launch exit 2 (both implemented afterwards; see the
[installer checkpoint](installer-checkpoint.md), which also lists what stayed
unmeasured, including the vendor-side preseed effect); M1c signing, original
path/id and Finder/Dock/Spotlight entries; M2 test-account sessions, the vendor Mach
service set, Keychain/data-protection behavior and verified-build policy
(`parsec-orbit.client.status` stays `not-verified` for every build). No
original-path launch, Gatekeeper acceptance or Windows runtime success is
inferred from this checkpoint.
