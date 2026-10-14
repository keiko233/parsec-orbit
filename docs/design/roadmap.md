# Roadmap

Supersedes the roadmap in the research repository. Status as of the SDK design stage.

| Phase | Content | State |
| --- | --- | --- |
| 0 | Loader, bcrypt proxy, `ParsecOrbit*` ABI v1, official `native` and `webview` plugins, hello sample, e2e tests | done (v0.1.0) |
| 1 | Workspace restructure: `crates/`, `-abi` / `-sdk` / `-core` / `-loader` split, `Platform` + `Logger` traits; official plugins renamed to `parsec-orbit-plugin-high-bitrate-*` | done |
| 2 | Host revision 2: per-plugin host table, service registry, `requires` ordering, kind handlers, `log_ex` | done |
| 3 | Rust SDK on crates.io (`-abi`, `-sdk`, `-sdk-std`), samples migrated, size spike and report | done (publishing itself is a manual step) |
| 4 | `webview-bridge` plugin (DevTools client extracted), JS runtime plugin (Boa), `@parsec-orbit/sdk` for npm, JS/TS samples (`bandwidth.js` as the baseline) | done (npm publishing is a manual step) |
| 5 | Marketplace (index, sha256, signatures), hooks service, config storage service | done (no hosted service; the maintainer creates the signing key) |
| 6 | UI service plugin (winio external companion window, `parsec-orbit.ui`), declarative UI schema, Rust and JS SDK bindings, hotkey and webview trigger | implemented on `feat/orbit-ui`, awaiting merge |
| 7 | UI follow-up ([orbit-ui.md](orbit-ui.md#6-planned-settings-the-window-should-grow-into) section 6): editable bandwidth ladder, STUN latency table with pinning and custom servers, Overview actions (debug console, restart), restart-and-apply, Log tab | planned |

Each phase ends with `cargo test --workspace` green, docs updated in the same commits, and a recorded size report where relevant.

## macOS

Separate track, planned in [macos-phases.md](macos-phases.md); specified in [spec/macos-host.md](../spec/macos-host.md) and [spec/macos-platform.md](../spec/macos-platform.md). Apple silicon, client mode only (hosting is out of scope).

| Phase | Content | State |
| --- | --- | --- |
| M0 | Cross-platform workspace: target gates, native kinds `dll`/`dylib`, macOS `Platform`/`Logger`, `freestanding!` for `aarch64-apple-darwin`, macOS CI | done (`50c74e7`, `020ee7f`, `6f38677`) |
| M1 | Dependency bootstrap, transactional patched-app apply/restore, isolated state, signing and original-path launch gates, services/market | M1a done (`a45d215`); M1b scratch-measured (`8abf13a`); M1c planned |
| M2 | Real client sessions, measured UI/import hooks, native bitrate compatibility and update/recovery matrix | planned; UI surface must be measured |

Current direction: [ADR 0014](decisions/0014-macos-patched-bundle.md). [Native dependency PoC](macos/native-dependency-poc.md) passed Finder/LS, real core/SDK hello and client re-exec in a disposable copy. M0 is done: the whole workspace checks on `aarch64-apple-darwin`, macOS arm64 CI runs the portable suite, and non-Parsec-host fixture loading is measured. It does not complete M1; ad-hoc Gatekeeper assessment rejected, and original installed path/id remain unverified. Each M0/M1/M2 subphase ends with its own validated local commit.
