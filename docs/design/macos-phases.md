# macOS phase plan

Status: **M0 and M1a/M1b implemented; M1c local ad-hoc/original-path activation measured; M2 independent code and local plugin scenarios complete. Session, distribution and internals-dependent gates remain open (2026-10-15).** Specifications: [macos-host.md](../spec/macos-host.md), [macos-platform.md](../spec/macos-platform.md). Current decision: [ADR 0014](decisions/0014-macos-patched-bundle.md). Evidence: [native dependency PoC](macos/native-dependency-poc.md), [extension-project research](macos/extension-projects.md), and earlier [investigations](macos/README.md).

Scope: arm64/client only. The target is ordinary launch of the locally patched installed `Parsec.app`, without an extra launcher. A copy passing Finder launch is not original-path or release validation. M0-M2 are independent of the completed Windows phases. Every implementation phase ends with appropriate checks, updated docs and an atomic local commit; do not collect the entire port into one final commit.

```text
M0  portable workspace + formal native macOS core/SDK loading
 |
M1  minimal bootstrap -> transactional patch/restore -> signing/original-path gates
 |
M2  client-session validation + measured hooks/UI/plugin compatibility
```

## M0 - portable workspace and native platform

Owner: implementation session; no installed-client mutation.

**Deliverables**

- ~~Target-gate Windows loader/hooks/native scanners/verification/e2e code; check the whole workspace on `aarch64-apple-darwin` without hand-excluding crates. Preserve Windows behavior and loader size policy.~~ Done in **M0a** (`50c74e7`).
- ~~Formal `dylib` kind, platform defaults/entry suffix, native-kind mismatch skips, macOS `:` path list. Do not carry the PoC `kind="dll"` convention into production.~~ Done in **M0a**/**M0b** (`50c74e7`, `020ee7f`). The index `platform` field, per-version filtering and the market's macOS paths/signing are specified in [macos-platform.md](../spec/macos-platform.md#9-marketplace-on-macos) and are M1b work; M0 only made the market's path handling host-independent (`d3294a2`).
- ~~Separate macOS loader crate with real dlopen/dlsym/dlerror Platform and append Logger; real core lifecycle, registry and permanent mapped-module ownership.~~ Done in **M0b** (`020ee7f`).
- ~~`freestanding!` for arm64 Mach-O, std/no_std SDK fixtures, portable ABI alignment test fix and size report.~~ Done in **M0b**/**M0c**.
- ~~macOS arm64 CI and portable/core fixture tests; retain Windows CI checks.~~ Done in **M0c**.

**Exit gate:** formal manifest discovery and Query/Init/registry succeed for std/no_std fixtures in a non-Parsec host `[measured]` (`crates/parsec-orbit-loader-macos/tests/fixture_e2e.rs`); foreign native kind skips correctly `[measured]`; `cargo check --workspace --all-targets --locked`, `cargo test --workspace --locked` and the pnpm suite pass on `aarch64-apple-darwin` `[measured]` and on Windows CI. A cross-check does not substitute for Windows runtime tests: the Windows job in `.github/workflows/test.yml` remains the runtime evidence, and the portable subset runs on `macos-26`.

Suggested commits: **M0a** target gates/manifest/alignment; **M0b** MacPlatform/Logger/fixtures/SDK; **M0c** CI/size/docs, if the changes warrant separate review.

## M1 - patched launch and installation

Owner: implementation session. All initial runtime work uses disposable copies and fresh isolated state. Original-path validation belongs in a recoverable isolated user/test machine; do not patch the developer's live installation implicitly.

### M1a - minimal production bootstrap

**Accepted on disposable copies (2026-10-14, `a45d215`).** The [implementation
checkpoint](macos/bootstrap-checkpoint.md) provides canonical installation
binding, a fail-closed dependency constructor, NSS callbacks/export
resolution, exclusive verified seeding, the constructor-installed isolation
guard and a bounded readiness policy. A valid selected host now arms the
guard and the verified home replacement, starts the single deferred worker
and returns to vendor main; any failed step still exits 78 pre-main. Two
ordinary-user acceptance runs (direct exec on empty state and Launch Services
`open -n` on a preseeded copy) measured the window, the native
hello/registry evidence through the `parsec-orbit.client` service and the
self-exec re-initialization, with real-state and payload hashes unchanged.
The measured limits that remain open are listed in the checkpoint: the
empty-state first-launch exit 2 (M1b preseed), the vendor Mach service set,
Keychain/data-protection behavior and the verified-build policy (M2).

- Mach-O constructor/dependency path; canonical installation-bound activation gate; inactive forwarding and no side effects in other processes.
- Current-UID home handling with original NSS semantics, validated private roots, separate-image probe and fail-closed isolation errors. Audit and cover shared `.parsec-persistent`, open/openat and other observed APIs; do not generalize fopen/stat coverage.
- Verified atomic seed for new isolated state, no real credential/state migration and no overwrite of existing appdata.
- Deferred initialization based on bounded client readiness, no fixed PoC sleep; core discovery, version/hash/token logging and the `parsec-orbit.client` policy. Unknown client internals disable affected hooks; unknown stub refuses apply.
- HR + disable LV only. Test strong/weak missing/corrupt/constructor boundaries and record the selected policy. Do not claim recovery from native plugin crashes.

**Exit gate:** fresh copy first launch through LS/Finder displays a window and real native hello/registry evidence; self-exec reinitializes; isolation-negative cases stop before client execution; complete audit findings are recorded. Copy PoC results reduce uncertainty but do not replace product tests.

### M1b - transactional apply/restore

**Implemented and scratch-measured (`8abf13a`, `af92789`, `3af8910`).**
The [installer checkpoint](macos/installer-checkpoint.md) records the
transaction, universal official-stub support, arm64 loader preflight, full
recorded restore metadata, root-owned staging transformations, per-app lock,
external-replacement refusals and 14 scratch scenarios. Formal apply, first
launch, re-apply and exact uninstall passed on the disposable official bundle.
The normal-user `/Applications` attempt was denied at backup rename and rolled
back; a separate administrator-assisted original-path candidate roundtrip
passed, as recorded in M1c. The subsequent formal sudo wrapper regression
and persistent actual-path installation also passed; see the installer checkpoint.
Market platform filtering, arm64 preflight and staged ad-hoc signing are
implemented separately in `37dc0c7`, with a real signed-index HTTP integration.

- Version/hash/layout whitelist and idempotent pristine/applied/stale/invalid state machine.
- Full pristine bundle/metadata backup, provenance and transaction journal; supported header-space patch only, no vendor instruction edits or client re-signing.
- Inside-out signing, strict verification, staged install, rollback and process identity checks. Preserve original path/id. Inspect permissions early; request only concrete necessary authorization after staging validation.
- Uninstall restores matching-version pristine app and keeps Orbit data by default; stale backup must not overwrite a vendor update.
- Client update versus full app/stub replacement detection and explicit re-apply. No vendor package preinstall invocation.
- Release archive contains only Orbit-owned artifacts. No Dock preference rewriting or default URI/login-item changes.

**Exit gate:** scratch apply/re-apply/uninstall and failure/interruption cases are recoverable; pristine restore matches hashes/signature/metadata; unknown and inconsistent inputs refuse without mutation.

### M1c - signing, original-path and service gates

**Independent local route and explicit original-path activation measured
(`70da8d3`, `06f5ead`).** The maintainer selected ad-hoc for development and
user installation; Developer ID/notarization is not a missing credential gate.
The requested local wrapper now handles persistent original-path installation; packaged distribution remains unverified. Strict codesign passes separately
from the measured ad-hoc spctl rejection (exit 3). Assessment preserves
quarantine; a synthetic attribute test does not establish downloaded delivery.
The [signing checkpoint](macos/signing-checkpoint.md) records one authorized
administrator-assisted `/Applications` Launch Services run with fresh private
state, ordinary-user execution, 6 successful plugins through 101a→104a,
identity-verified stop and exact original restore. Existing Dock/Spotlight/URI
entries, TCC/credentials and quarantined delivery are still unmeasured.

- Clean downloaded/quarantined payload, codesign versus spctl outcomes, available signing identities and supported distribution behavior. The selected delivery route is local ad-hoc; no Developer ID/notarization is required. No silent quarantine removal or global protection weakening.
- Same path/id, existing Finder/Dock/Spotlight/URI entries, foreground/window, identity/TCC/Keychain/updater behavior; test in an isolated environment.
- `config`, market signing/staging and interpreted JS/Boa if compatible with retained HR. Measure executable-memory requirements before considering additional exceptions.
- Installation/launch/rollback/uninstall guide stating exactly which gates passed.

**Exit gate:** documented supported signing/install route and original-entry experience, with no unresolved blocker hidden behind ad-hoc copy success. If distribution needs external credentials or OS permissions, complete independent work, record the blocker and ask for the concrete missing input. Do not mark M1 release-ready while Gatekeeper behavior remains unsupported.

## M2 - client behavior and plugin parity

**Independent code and local scenarios completed (`35d20c9`, `56bc6b2`,
`06f5ead`, `b038686`); full M2 exit gate remains open.** The
[client checkpoint](macos/client-checkpoint.md) records shared client ABI/SDK,
bounded classic already-bound import rebinding under HR, official config/Boa
Mac manifests, JS kind, actual 105c readiness and update handling, signed-out
native UI inventory, static bitrate tooling and owned memory-protection
canaries. Disposable 105c Launch Services acceptance has 7 loaded / 0 skipped /
0 failed. Client builds remain `not-verified`; no internals-dependent native
bitrate or WebView macOS package is offered. The maintainer currently has no
dedicated account/remote endpoint and explicitly requested local independent
work first. Account/session evidence cannot be substituted by scratch tests.

Owner: implementation session plus maintainer-supplied test account/environment where required.

- Real login/connect/disconnect/reconnect, input/audio and client self-update scenarios. Test-account credentials must be provided/entered explicitly; do not reuse real state.
- UI discovery by actual view/URL/DOM evidence. The PoC login view was native, with no WKWebView. Ship a WebView backend only if a usable runtime surface is measured; otherwise record it unavailable and preserve service failure semantics.
- Import-rebind hooks and `__DATA_CONST`/protection gates under the retained HR policy. No unmeasured inline code patch/JIT assumptions.
- Native bitrate layout/use-site validation, per verified build; unknown builds refuse internals-dependent changes.
- Config/JS/market/plugin scenarios, inspector aid where supported, update/rollback regression matrix, verified-build documentation and size checks.

**Exit gate:** each shipped macOS plugin passes its scenario on explicitly verified builds; unknown-build policy is exercised; connection/session and recovery checks pass in the supported environment. Missing UI/account/platform evidence must remain an explicit limitation, not a fabricated success.

Suggested commits: **M2a** client/session harness and measured surfaces; **M2b** hooks/UI/native plugins separately as justified; **M2c** update matrix/release guide after gates pass.

## Verification matrix

| ID | Required evidence | Owner / phase |
| --- | --- | --- |
| V1 | pristine input hashes/signature/metadata; version-bound backup and exact restore | M1b |
| V2 | run-owned PID + start identity + executable path; no unrelated process signals | M1a/M1b |
| V3 | full observed file/read/write/IPC audit; no real Parsec credential/state access | M1a, repeat M2 |
| V4 | client provenance and real core/plugin ready counts | M0b/M1a |
| V5 | fresh first launch, client update/re-exec and full-app replacement/re-apply | M1a/M1b |
| V6 | inactive gate, root/NSS/symlink failures, loader missing/corrupt/constructor faults | M1a/M1b |
| V7 | strict signature and separate Gatekeeper/quarantine assessment | M1c |
| V8 | original path/id and existing Finder/Dock/Spotlight entries | M1c |
| V9 | client test-account session, services/plugins and unknown-build behavior | M2 |
| V10 | interrupted apply/signing, disk/permission faults, stale restore/concurrent update | M1b/M2 |

## Evidence status and remaining questions

| Phase / gate | Current evidence | Remaining |
| --- | --- | --- |
| M0 | Workspace/platform/SDK/owned fixtures implemented and locally tested | Windows runtime CI remains separate evidence |
| M1a | Guarded copy first launch, re-exec and negative gates measured | Full syscall/IPC and credential audit is not claimed |
| M1b | Formal toy/official-copy apply/re-apply/exact restore, metadata and injected replacement refusals | Root-owned restore permits/reports only OS-added root provenance; real full-app concurrency/disk-full scenarios |
| M1c | Local ad-hoc route, separate strict codesign/spctl, original-path explicit LS launch/exact restore | Quarantined delivery, existing Dock/Spotlight/URI and TCC/credentials |
| M2 independent | Shared client SDK, bounded owned import hooks, config/Boa/JS kind, market, 105c update, bounded diagnostics | Generic/chained/lazy coverage is outside the shipped hooks boundary |
| M2 account/session | Unverified; no supplied dedicated account/endpoint | Login/connect/reconnect/input/audio and coexistence |
| M2 native/UI | Static bitrate candidate/xrefs; signed-out native Matoya view, no WKWebView; owned canaries only | Runtime table/menu/use-sites/rollback, usable WebView URL/DOM |

All independent checks passed on the final code: workspace test/check,
formatting, targeted no-deps Clippy, Python and pnpm suites. See the client
checkpoint for the pre-existing transitive/test lint warnings and local
Windows C-toolchain limitations. Size measurements are recorded in
[size-report.md](size-report.md), not hidden behind an unmeasured budget claim.
The earlier original-entry trial and disposable test copy were restored.
The subsequently requested formal local wrapper is now persistently installed
at `/Applications/Parsec.app`; its recorded original backup and Orbit evidence
are retained. No push, PR or release was performed.

Hosting, x86_64 execution and out-of-process plugins remain excluded. The requested local ad-hoc wrapper is implemented; packaged delivery remains unverified; no
Developer ID credential request is pending. Complete the remaining measured
behavior gates before claiming all phases or release readiness.
