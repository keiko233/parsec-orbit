# macOS phase plan

Status: **planned implementation; disposable-copy native dependency PoC passed**. Specifications: [macos-host.md](../spec/macos-host.md), [macos-platform.md](../spec/macos-platform.md). Current decision: [ADR 0014](decisions/0014-macos-patched-bundle.md). Evidence: [native dependency PoC](macos/native-dependency-poc.md), [extension-project research](macos/extension-projects.md), and earlier [investigations](macos/README.md).

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

- Mach-O constructor/dependency path; canonical installation-bound activation gate; inactive forwarding and no side effects in other processes.
- Current-UID home handling with original NSS semantics, validated private roots, separate-image probe and fail-closed isolation errors. Audit and cover shared `.parsec-persistent`, open/openat and other observed APIs; do not generalize fopen/stat coverage.
- Verified atomic seed for new isolated state, no real credential/state migration and no overwrite of existing appdata.
- Deferred initialization based on bounded client readiness, no fixed PoC sleep; core discovery, version/hash/token logging and the `parsec-orbit.client` policy. Unknown client internals disable affected hooks; unknown stub refuses apply.
- HR + disable LV only. Test strong/weak missing/corrupt/constructor boundaries and record the selected policy. Do not claim recovery from native plugin crashes.

**Exit gate:** fresh copy first launch through LS/Finder displays a window and real native hello/registry evidence; self-exec reinitializes; isolation-negative cases stop before client execution; complete audit findings are recorded. Copy PoC results reduce uncertainty but do not replace product tests.

### M1b - transactional apply/restore

- Version/hash/layout whitelist and idempotent pristine/applied/stale/invalid state machine.
- Full pristine bundle/metadata backup, provenance and transaction journal; supported header-space patch only, no vendor instruction edits or client re-signing.
- Inside-out signing, strict verification, staged install, rollback and process identity checks. Preserve original path/id. Inspect permissions early; request only concrete necessary authorization after staging validation.
- Uninstall restores matching-version pristine app and keeps Orbit data by default; stale backup must not overwrite a vendor update.
- Client update versus full app/stub replacement detection and explicit re-apply. No vendor package preinstall invocation.
- Release archive contains only Orbit-owned artifacts. No Dock preference rewriting or default URI/login-item changes.

**Exit gate:** scratch apply/re-apply/uninstall and failure/interruption cases are recoverable; pristine restore matches hashes/signature/metadata; unknown and inconsistent inputs refuse without mutation.

### M1c - signing, original-path and service gates

- Clean downloaded/quarantined payload, codesign versus spctl outcomes, available signing identities and supported distribution behavior. Developer ID/notarization is a research route, not assumed success. No silent quarantine removal or global protection weakening.
- Same path/id, existing Finder/Dock/Spotlight/URI entries, foreground/window, identity/TCC/Keychain/updater behavior; test in an isolated environment.
- `config`, market signing/staging and interpreted JS/Boa if compatible with retained HR. Measure executable-memory requirements before considering additional exceptions.
- Installation/launch/rollback/uninstall guide stating exactly which gates passed.

**Exit gate:** documented supported signing/install route and original-entry experience, with no unresolved blocker hidden behind ad-hoc copy success. If distribution needs external credentials or OS permissions, complete independent work, record the blocker and ask for the concrete missing input. Do not mark M1 release-ready while Gatekeeper behavior remains unsupported.

## M2 - client behavior and plugin parity

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

Copy-only window/bundle/self-exec/native ABI gates passed in the PoC; formal platform code, full isolation and production gates remain open. The cold-seed branch is a static candidate; preseeded first launch passed. Ad-hoc spctl rejection is measured. Loader-as-Host remains optional development/fallback, with its historical limitations documented.

Remaining: clean-machine tooling and size budget (M0/M1), alternate LOADER token (informational M1), Keychain/TCC/camera/microphone and account overlap (M1c/M2), whole-app updates/backup metadata (M1b), UI rollout/DOM and rebinding under HR (M2). Hosting, x86_64 and out-of-process plugin execution remain excluded. Signing/distribution research is no longer excluded: it is a mandatory gate.
