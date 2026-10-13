# macOS host and loader

Status: **draft**; the native dependency entry path has been measured in a disposable app, not implemented as a product. [ADR 0014](../design/decisions/0014-macos-patched-bundle.md) replaces the earlier separate-host default. Paths, plugin libraries and services are in [macos-platform.md](macos-platform.md); evidence is in [design/macos/](../design/macos/README.md).

The target is normal launch of the installed official `Parsec.app` after applying Orbit locally. A patched bootstrap loads the Orbit dylib as a Mach-O dependency; users do not need a separate launcher. The untouched vendor signature cannot be retained for the modified app.

## 0. Conventions

| Tag | Meaning |
| --- | --- |
| `[measured]` | observed on the recorded real or toy process; scope is in the linked experiment |
| `[static]` | inferred from source, binary or signature metadata |
| `[unverified]` | requires a runtime gate; not a shipping claim |

Untagged statements are requirements, not observations. A passing disposable-copy launch does not verify the original path, signing identity or distribution.

## 1. Scope and terms

macOS `aarch64`, Parsec 150.x, client mode only. Hosting, x86_64 and an out-of-process plugin companion remain out of scope. Signing/distribution research is required by ADR 0014; Developer ID/notarization success is not assumed.

| Term | Meaning |
| --- | --- |
| Vendor app | pristine installed `Parsec.app`, normally `/Applications/Parsec.app`, id `tv.parsec.www` |
| Patched app | that installation after a supported local Orbit patch and re-signing |
| Bootstrap stub | `Contents/MacOS/parsecd`, resolves state, validates and loads the client dylib |
| Client dylib | versioned `parsecd-<build>.dylib`, entry `wx_main` |
| Host | the process running the patched stub and official client; a separate host copy is optional fallback |
| Loader dylib | `Contents/Frameworks/libparsec_orbit_loader.dylib`, loaded through a dependency command |
| Orbit root | real home plus `Library/Application Support/ParsecOrbit` |
| Isolated home/state | `<Orbit root>/home`, with `.parsec` and `.parsec-persistent` beneath it |

## 2. Invariants

1. Only an explicitly selected, supported bundle may be patched. Record full pristine backup, source version/hash/signature and transaction status before mutation. Preserve the installed path, executable name and bundle id; no Dock substitution is required.
2. Do not read, copy, merge, write or delete real Parsec state/credentials. Audit `~/.parsec`, `/Users/Shared/.parsec`, `/Users/Shared/.parsec-persistent`, Library paths, Keychain and IPC. This is a product requirement; the PoC's partial redirection did not prove it universally.
3. Never terminate an unrelated process by name. Refuse mutation while the selected app is running; any test cleanup binds run-owned PID, start identity and canonical executable path.
4. No global DYLD environment, SIP/AMFI/Gatekeeper change, silent quarantine removal or permanent privileged service. Local installation permissions may be needed for the selected bundle; report and request them explicitly when necessary.
5. Release no vendor executable, dylib, icon or pristine backup. Obtain vendor inputs from the user's installation.
6. Preserve vendor client dylib bytes/signatures and bootstrap instruction semantics/hash validation. Patch supported load-command space only; reject unknown or inconsistent inputs.
7. Preserve Hardened Runtime with the measured disable-LV exception, and no unrelated signing exceptions. Document the resulting application-level security and identity changes.

Native plugins execute in the client process and can crash or compromise it. These invariants bind Orbit-owned components; arbitrary native code is not sandboxed by the plugin ABI.

## 3. Components and layout

```text
/Applications/Parsec.app/                     selected installation, locally patched
  Contents/Info.plist                        vendor path/id retained
  Contents/MacOS/parsecd                     supported arm64 stub + load command
  Contents/Frameworks/libparsec_orbit_loader.dylib

~/Library/Application Support/ParsecOrbit/
  home/.parsec/                             independent client state
  home/.parsec-persistent/                   independent shared-persistent replacement
  plugins/  data/  market/  parsec-orbit.log
  installs/<installation-id>/                provenance, transaction journal, pristine backup
```

Backup layout is installer-owned, private and version-bound; it is not plugin discovery space. A patch changes the app's signing identity even when id/path are preserved. Existing Dock/Spotlight/TCC behavior is `[unverified]`.

## 4. Launch sequence

Install time: verify the input stub and full bundle, stage loader/dependency patch, preseed fresh isolated state, sign/verify and commit the transaction.

1. Finder/Dock/Launch Services executes the same installed stub. No environment-delivery launcher is required.
2. dyld loads the sibling loader via `LC_LOAD_DYLIB` `[measured]` on a disposable copy. The production strong/weak policy remains a fault-test decision; weak dependencies do not guarantee recovery from signature or constructor failures.
3. The loader gates activation and validates isolation before client execution.
4. The stub verifies the official client hash, `dlopen`s it and calls `wx_main` `[measured]` on the copy.
5. Deferred loader initialization discovers the client build and runs core/plugin activation. Replace the PoC's fixed sleep with a bounded readiness contract; no complex plugin work in the constructor.
6. Client self-exec reloads the dependency and initializes a new runtime `[measured]` for 101a to 104a on the copy.

Fresh isolated state must be seeded atomically from verified official skel without overwriting existing appdata. Empty-state launches exited 2 in the experiment; preseeded first launch succeeded. The retained parse-result branch is a `[static]` candidate explanation, not a traced runtime root cause.

## 5. Host contract (what the bootstrap does)

Vendor instruction bytes and behavior remain unchanged. Earlier static mapping is in [client-internals.md](../design/macos/client-internals.md); historical host-copy behavior is in [smoke-test.md](../design/macos/smoke-test.md).

| Dependency | Evidence |
| --- | --- |
| Current UID home determines `.parsec`; `useProgramData` selects shared state | home `[measured]`; shared selector `[static]` |
| skel seed, appdata SHA256 validation, state locks | seed and matching hashes `[measured]`; exact branches `[static]` |
| client `dlopen`, `wx_main` with four arguments and `LOADER_V14` token | call shape `[static]`; client launch `[measured]` |
| return 1 re-execs the same stub | `[measured]` with client update |
| updater writes the isolated state and replaces the client build | `[measured]` for 101a to 104a |
| client accesses shared `.parsec-persistent` independently of home | `[measured]`; requires additional isolation |

No promise is made for future stubs, alternate loader tokens or full app updates.

## 6. Loader dylib

### 6.1 Activation gate

Resolve the loader and main executable canonically. Activate only for the installed sibling stub bound to the installation record; all other processes get no Orbit threads/files and interposed calls forward unchanged. The PoC used an exact compiled scratch-path gate; production must not hardcode it. Gate and interposer state must not race during initialization.

M1a is measured complete on disposable copies. The dependency constructor
validates a schema-1 `Contents/Resources/orbit-install.json` binding canonical
`executable`, `loader`, `orbit_root` and `uid`. Unknown fields, unsafe roots,
nonprivate state and invalid records fail closed. Unrelated hosts do not read
the record, initialize logging or start Orbit workers. For a valid selected
host the constructor installs the shared-persistent redirect, the custom-SBPL
profile with its sentinel gate and the verified current-UID home replacement,
starts the single deferred worker, and only then returns to vendor main; any
failed step still exits 78 pre-main and names the step. See the
[checkpoint and audit procedure](../design/macos/bootstrap-checkpoint.md).

### 6.2 Interposition

Resolve the real current-UID passwd entry through the original implementation. Preserve other fields/UIDs and NSS error semantics. Validate private root ownership, canonical containment, symlinks, permissions, lengths and buffer/error behavior. A separate-image probe must confirm home interception before entering client code. Fail closed on isolation errors; never fall back to real state.

The home hooks are `getpwuid/getpwuid_r`. Shared persistent access also needs coverage; the PoC redirected only fopen/stat and denied the original path via its experimental sandbox. Audit other APIs and establish the production mechanism before claiming complete isolation. A constructor-installed sandbox does not cover earlier dyld initialization and is not implicitly the shipping policy.

The 2026-10-13 [mechanism resolution](../design/macos/isolation-mechanism-assessment.md#mechanism-resolution-2026-10-13) selects the constructor-installed custom SBPL profile with a fail-closed sentinel gate as the only measured mechanism that denies the required protected metadata surface. The profile and gate are implemented in `isolation.rs`, and the measured shared-persistent redirect for the nine path-taking APIs is implemented in `redirect.rs`; both have unit and owned-fixture tests. Vendor execution stays disabled until the remaining runtime-observation items listed there (the vendor Mach service set and the redirect against the real client) are measured through a controlled disposable-copy client run.

The two 2026-10-14 guarded acceptance runs (direct exec and Launch Services)
measured the redirect against the real client (its `devid.bin`/`status.json`
landed in the private
`<orbit_root>/home/.parsec-persistent`, not in `/Users/Shared`) and the client
startup path under the constructor-installed profile. The vendor's required
Mach service set remains an M2 observation item; the shipped profile keeps
`(allow default)` for Mach for the measured reason above. Also measured: the
vendor creates `log.txt` as `0644` inside the private state root, and the
readiness observer accepts that file while still refusing group/other write,
hardlinks, replacement or a nonprivate root.

The NSS implementation preserves inactive forwarding and other-UID/error
semantics. Reentrant replacement strings stay inside the caller's buffer;
insufficient capacity returns `ERANGE` with a null result. Active legacy calls
use a thread-local owned copy. Original active implementations are resolved
from the system NSS export trie to avoid recursive interposed `dlsym` results.
The `harness` build can install a replacement home for fixtures; the
production constructor arms the same replacement only for a valid activation
record, verifies it in-process against both the interposed and the real NSS
entry, and fails closed (exit 78 pre-main) if either lookup differs.

### 6.3 Lifecycle

Keep constructor work minimal and nonblocking except bounded safety checks. Initialize Logger/Platform and plugins off the hot path after validated client readiness. `exec` starts a fresh runtime. No unload: registry pointers require modules to remain mapped. Report plugin initialization failures; do not claim native crashes are recoverable by catching an Init return value.

### 6.4 Loader size

[ADR 0004](../design/decisions/0004-loader-stays-light.md) applies. Measure the macOS loader budget before adding broad dependencies; keep UI experiments and fixture-only helpers out of the shipping loader.

## 7. State isolation

Create Orbit root privately (0700). Sign in separately; no credential migration. Client settings/plugins act only on Orbit state. The first PoC successful run read real shared devid while writes were denied; later redirection/read denial fixed that observed access. This is not evidence for universal isolation. Library/Keychain/network/IPC behavior and simultaneous signed-in clients remain `[unverified]`.

## 8. Signing and entitlements

- Development PoC: ad-hoc outer signature with runtime and only `com.apple.security.cs.disable-library-validation`; owned dylibs signed first, outer bundle last. Both static dependencies and later real-core plugin loading passed `[measured]`.
- This removes team validation for this process and replaces the original vendor identity/seal. It does not disable SIP globally or preserve vendor notarization. No `--deep` blanket re-signing of the vendor client.
- `codesign --verify --strict` passed, but `spctl --assess --type execute` rejected the PoC `[measured]`. Distribution, downloaded/quarantined payloads and any Developer ID/notarization strategy are release gates.
- Do not silently remove quarantine, bypass warnings or disable protections. Report supported signing behavior accurately.
- HR remains enabled: earlier plain-host RX/JIT observations cannot be applied to the new host without measurement. JS/Boa behavior, executable-memory requirements and rebinding feasibility require targeted tests, not speculative extra entitlements.
- Hosting and profile-gated capture remain out of scope. Client microphone/camera/TCC/Keychain effects of re-signing are `[unverified]`.

## 9. Client versions and compatibility

Identify loaded client dylib name/hash and `LOADER_V` token. Log version/provenance before plugins. Distinguish stub patch support from client-hook support: unknown stub/layout refuses apply; an unknown client may load portable plugins with a warning, but internals-dependent hooks must refuse unless explicitly verified. No measured build is declared fully supported solely because its login window worked.

Client-dylib updates and whole-app replacement are separate. The former re-exec path passed in the copy; the latter can erase the patch and needs detection/explicit re-apply. Do not silently patch an unknown replacement or refresh a pristine backup from modified files.

## 10. Installation

Installer responsibilities (`install.sh`/`uninstall.sh`/`package.sh` or equivalent tools):

1. Inspect architecture, selected bundle version/hash/signature, active process identity and permissions. Explain planned files and signing impact; stage before requesting any necessary installation privilege.
2. Maintain version-bound full pristine backup and a transaction journal. Classify pristine/applied/stale/invalid states. Never overwrite the only pristine backup with patched content.
3. Validate supported Mach-O header padding and command layout, insert one loader dependency without changing instruction bytes, install owned payloads and preseed new isolated state after hash validation.
4. Sign inside-out, verify signature and provenance, then atomically install or roll back. Idempotent apply validates an existing matching install; interrupted/stale/partial state must not be accepted as pristine.
5. Preserve path/id; no Dock rewrite, default URL handler or login-item registration. Test existing launch routes in an isolated user/test machine before claiming compatibility.
6. Uninstall restores the corresponding pristine bundle, including signature/metadata. If vendor updated, refuse a stale restore and offer matching-version recovery. Keep Orbit data unless purge is explicitly requested.
7. Package only Orbit-owned artifacts/checksums. Signing/Gatekeeper validation is mandatory before claiming a supported release.

Scratch complete-backup restoration passed `[measured]`; /Applications authorization, full metadata/ACL/xattr fidelity, atomic failure recovery and concurrent updates remain `[unverified]`.

## 11. Compatibility with the Windows documents

Shared ABI stays ABI 1/host revision 2. Platform differences are in [macos-platform.md](macos-platform.md). PoC `kind="dll"` with a dylib was only a fixture compatibility convention; the formal macOS `dylib` kind was implemented in M0. No Windows loader behavior change is authorized by this platform design.

## 12. Open questions

| Gate | Remaining validation |
| --- | --- |
| E1/E2 | Copy window, bundle and self-exec passed; original-path foreground/UI and full client session still required |
| E3 | Static dependency solved copy environment delivery; production strong/weak fault policy remains |
| E4 | Complete isolation, shared persistent/API coverage, Library/Keychain/IPC and skel behavior |
| E5 | Future/alternate `LOADER_V` acceptance |
| E6 | Real ad-hoc SDK fixture passed; formal dylib kind, no_std and unsigned/corrupt rejection remain |
| E7 | Original id/path, existing Dock/Spotlight/URL and TCC/Keychain identity after re-signing |
| E8 | Other-process inactive gate, child behavior and PID-safe cleanup |
| E9/E10 | Independent test-account connections, overlap and microphone/camera behavior |
| E11 | Gatekeeper/quarantine/signing/distribution; spctl currently rejects |
| E12/E13 | Clean-machine tool availability and macOS size budget |
| E14/E15 | Full-app replacement/re-apply/restore; URI/login behavior, without default handler changes |

Owners and phase assignments are in [macos-phases.md](../design/macos-phases.md).
