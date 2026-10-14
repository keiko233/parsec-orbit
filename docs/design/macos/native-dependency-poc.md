# Native dependency PoC: normal launch with Orbit inside Parsec

Date: 2026-10-13. Host: macOS 27.0 (26A428), arm64. Scope: client-only, disposable app copies. Four `gpt-6-luna` high/max side tasks supplied the dyld/fault matrices, window observer, real core/SDK fixture and static safety review. No repository code, installed vendor app or global security setting was changed; no commit was made during the experiment.

## Result

**The native entry path passed in a disposable Parsec bundle:** an added `LC_LOAD_DYLIB` loaded the Orbit loader on a genuine Finder double-click; the official client displayed its login window, and the real Orbit core activated a Rust SDK hello plugin and verified its registered interface. A visible AppKit label read `Orbit PoC · core + hello loaded`.

The host kept Hardened Runtime with `disable-library-validation`, used no DYLD injection environment variable and no additional launcher. SIP remained enabled. This supports the [patched-bundle direction](../decisions/0014-macos-patched-bundle.md), not a claim that the original installed path or distribution works. The original vendor outer signature is replaced. `codesign --verify --strict` passed; `spctl --assess` rejected the ad-hoc app.

Spicetify's transferable mechanism is patch/apply/version/restore, not its SPA injection. The login window view walk found `__mty_app_view_0` and no WKWebView; this observation covers that window only.

## Measurements

| Check | Result | Boundary |
| --- | --- | --- |
| Static dependency + plain ad-hoc | passed | toy |
| HR without loading exception | dependency rejected, abort 134 | toy, library validation |
| HR + disable LV | constructor and interpose passed | toy and Parsec copy |
| Main and later dlopen-image home interpose | passed | separate toy probe |
| HR + disable LV + allow-dyld-env | passed | toy; extra exception unnecessary |
| Launch Services `open -n`, no `--env` | loader/core/login window passed | preseeded fresh copy, PID 99592 |
| Genuine Finder double-click | visible core/hello label, AX and screenshot observed | copy, PID 99748 |
| Real core/SDK activation | loaded=1 skipped=0 failed=0, registry verified | external MacPlatform fixture |
| Client update/self-exec | 101a to 104a, constructor reran, core later passed | client dylib update, not full app update |
| Home-interpose negative gate | exit 78 before client | intentionally removed home tuples |
| Complete pristine-backup restore | seven files matched backup; strict vendor signature valid | scratch copy only |
| Patched signature | flags 0x10002 (adhoc,runtime), strict verification passed | outer copy, no Team id |
| Gatekeeper assessment | rejected, exit 3 | ad-hoc copy; no bypass |
| Original path/id and existing Dock/Spotlight | not tested | separate id/path used |
| Login/remote session/input/audio | not tested | no credentials entered |
| Developer ID/notarization/other OS releases | not tested | do not extrapolate |

## Implementation chain

```text
Finder / Launch Services
  -> disposable app bootstrap
  -> dyld dependency: ../Frameworks/libparsec_orbit_loader.dylib
  -> constructor: executable gate, separate home probe, experimental sandbox
  -> official stub: appdata/hash validation, dlopen client, wx_main
  -> client updater: exec same stub, dependency loads again
  -> loader worker: external Rust PoC cdylib
  -> real core scan -> MacPlatform dlopen/dlsym -> SDK Query/Init -> registry
  -> main queue: visible AppKit label
```

The installed pristine stub SHA256 was `785f985d7f0cf6cffda8966b776a61b6c6cb0c79e582a8e3f35cda36305be76d`. Its arm64 slice had 112 zero bytes after existing commands and before the first section. The new dependency command needed 88 bytes; ncmds changed from 33 to 34. The patcher inserted into padding, updated ncmds/sizeofcmds, kept the signature command last and moved no instruction sections. Future versions may lack this space and must be rejected rather than blindly patched.

Dependency path: `@executable_path/../Frameworks/libparsec_orbit_loader.dylib`. The copy retained the executable name and LSBackgroundOnly setting, but used a distinct bundle id/display name for safety. Owned dylibs were signed before the outer bundle; the official client dylib was not deep re-signed.

Seed client SHA256: `9cce85059c7f95f1a8e578b3c9f7200cb364520e557b5c85294802c8c596cdd5` (`parsecd-150-101a.dylib`). Its bytes/vendor signature were retained. The client updated to 104a and execed the same stub/PID; dependency initialization ran again.

The external Rust fixture links the current core, ABI and SDK crates. It supplies MacPlatform with dlopen/dlsym, scans a hello plugin and validates `com.example.orbit-poc`. It temporarily uses `kind="dll"` with a `.dylib` entry because that is the current core native branch. **This is not implementation of the normative dylib kind, full M0, config, JS runtime/JIT, unload or UI-hook parity.** The visible badge was added by the loader, not an existing production plugin UI API.

## Cold-state and shared-path findings

### First empty-state launch

Both plain and HR+LV runs copied the seed but exited 2 before client execution. A second launch succeeded. A fresh home preseeded with verified vendor skel succeeded on its first LS launch.

Static disassembly provides a candidate explanation: the earlier appdata parse failure is saved in w26; the successful post-seed parse does not clear that saved state; `0x41c0: cbnz w26` reaches the exit-2 branch at `0x461c`. No register/branch trace was captured, so this is not a runtime-confirmed root cause. Installation should atomically preseed new isolated state and preserve existing appdata, not require two user launches.

### Shared persistent state

The first successful run read real `/Users/Shared/.parsec-persistent/devid.bin`. A write to shared status.json was denied by the scratch-only write sandbox. This read was reported during the experiment; do not describe that run as complete read isolation.

Later builds redirect fopen/stat for that prefix to `<scratch home>/.parsec-persistent` and deny reads to the original path. Logs showed scratch status writes and missing scratch devid, with no real-path reads through these APIs. That mapper is partial; open/openat, rename/unlink, directory/lock APIs, Keychain, Library reads and IPC were not fully audited.

The experiment allowed default reads/network, denied writes outside its native scratch root and denied reads of the listed real Parsec roots. LS/Finder applied sandbox inside the loader constructor, not before all dyld initialization. This is an experimental protection, not the production isolation policy. Client networking included updates and LAN/UPNP discovery.

## Fault matrix: strong and weak dependencies

All toy fault cases used HR + disable LV with no payload symbol references.

| Payload state | Dependency | Observation | Exit |
| --- | --- | --- | --- |
| missing | strong | main not reached | 134 |
| missing | weak | main reached | 0 |
| not a Mach-O | weak | image skipped, main reached | 0 |
| invalid code signature | weak | killed before main/constructor marker | 137 |
| constructor abort | weak | constructor ran, main not reached | 134 |

Weak loading only recovered selected missing/unusable-image cases. It does not provide universal fail-open behavior. Keep the production constructor minimal and record the strong/weak choice separately; recoverable plugin Init errors do not imply native crash containment.

## Product implications and next gates

- Re-signing changes vendor seal, Team identity/designated requirement and possibly Keychain/TCC/updater behavior. Keeping path/id does not prove identity compatibility.
- HR + disable LV is the measured exception. It relaxes code-loading policy for the entire application process; no allow-dyld-env/JIT/debug entitlement was necessary for this experiment.
- Ad-hoc Finder success in local scratch does not solve downloaded/quarantined distribution. Gatekeeper/signing is a release blocker; do not clear quarantine or disable protections as a solution.
- Client-dylib updates passed; whole-app/stub replacement, unknown layout, interrupted patch/signing, rollback and concurrent updates did not. Back up the complete pristine bundle and refuse stale-version restore.
- Keep independent state/sign-in. Preserve arm64/client scope; no Hosting or Intel work is implied.
- Replace fixed sleeps/hardcoded scratch paths, partial file redirection and fixture-only manifest handling with production contracts before release.
- Validate original path/id and existing launch entries in an isolated macOS user/test machine with recoverable app/state backups. Remote sessions require an explicitly supplied test account.

[The phase plan](../macos-phases.md) assigns these gates to M0 native platform/SDK, M1 patched launch/install/signing and M2 verified client/plugin behavior. The PoC does not complete any whole milestone.

## Evidence archive and reproduction

Original author-local artifacts: `/tmp/parsec-orbit-poc-20261013/{native,core,toy,observe,review}/`. Key files: native/hard-lv-core/{patch.json,ls-result.json,finder-core-result.json,loader.log,loader.log.core.log,verify.log,spctl-result.json}, native/hard-bad-gate/direct-result.json, native/restore-case/result.json, native/final-verification.json, toy/{summary.md,run.log}, toy/weak-boundary/{summary.md,run.log}. These paths are not repository dependencies.

A source/log ZIP was preserved outside the repository in the author's Codex artifacts directory as `parsec-orbit-macos-poc-evidence.zip`; it excludes official binaries/dylibs. Core Cargo paths and constants are local fixture paths, not a standalone release package. This document preserves the durable findings; if local artifacts expire, repeat the scoped experiment rather than treating missing logs as new evidence.

Reproduction: use a fresh private scratch root, read-only pristine inputs and distinct app id. Record hashes/signatures; first run toy signing/fault matrices. Require zero header padding and a verified stub, sign inside-out and check strict codesign/spctl separately. Validate canonical executable/home paths, separate-image home probe and fail-closed behavior; deny non-scratch writes and real-state reads, including shared persistent. Preseed verified skel, then perform LS and genuine Finder launches without injection env. Record PID/start/path, windows, client/loader images, ABI output and visible marker. Stop only run-owned identities and compare pristine inputs afterward. Never execute the vendor package preinstall for this test.

Final audit: 17 baseline files retained hash/size/mtime/mode, and Git status was unchanged. Shared persistent was not in the initial baseline, so this comparison does not prove its before/after identity. All experimental Parsec processes were stopped after exact executable-path checks. The cleanup harness did not bind process start serial, so production cleanup still needs that safeguard.

The independent review was static, with runtime findings supplied by the main controller rather than separately reproduced. It confirmed the candidate cold-seed branch and identified incomplete API auditing, constructor-time sandbox boundary and reused-build-directory risks. Use new directories for cold runs; builder reuse can retain old home/log state.
