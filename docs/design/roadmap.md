# Roadmap

Supersedes the roadmap in the research repository. Status as of the SDK design stage.

| Phase | Content | State |
| --- | --- | --- |
| 0 | Loader, bcrypt proxy, `ParsecOrbit*` ABI v1, official `native` and `webview` plugins, hello sample, e2e tests | done (v0.1.0) |
| 1 | Workspace restructure: `crates/`, `-abi` / `-sdk` / `-core` / `-loader` split, `Platform` + `Logger` traits; official plugins renamed to `parsec-orbit-plugin-high-bitrate-*` | done |
| 2 | Host revision 2: per-plugin host table, service registry, `requires` ordering, kind handlers, `log_ex` | spec |
| 3 | Rust SDK on crates.io (`-abi`, `-sdk`), samples migrated, size spike and report | spec |
| 4 | `webview-bridge` plugin (DevTools client extracted), JS runtime plugin (Boa + ractor), `@parsec-orbit/sdk` on npm, JS/TS samples (`bandwidth.js` as the baseline) | design (`spec/js-sdk.md`) |
| 5 | Marketplace (index, sha256, signatures), hooks service, config storage service | later |

Each phase ends with `cargo test --workspace` green, docs updated in the same commits, and a recorded size report where relevant.
