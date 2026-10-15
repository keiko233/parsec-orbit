# Parsec Orbit documentation

| Directory | Holds | Audience |
| --- | --- | --- |
| [`spec/`](spec/) | Normative specifications: ABI, manifest, loading rules, host services, SDK APIs. A change in behaviour changes the spec in the same commit. | Plugin authors, SDK and loader implementers |
| [`design/`](design/) | Why things are the way they are: architecture, decision records, roadmap. Not normative. | Contributors |
| [`guides/`](guides/) | Task-oriented how-tos (write a Rust plugin, write a TypeScript plugin, ship a plugin). Written together with the SDKs they describe. | Plugin authors |
| [`development/`](development/) | How we work: workflow, commits, testing and Rust style, and the rules coding agents must follow. | Contributors and coding agents |

## Specifications

| Document | Status | Summary |
| --- | --- | --- |
| [spec/plugin-abi.md](spec/plugin-abi.md) | Implemented (ABI 1, host revision 2) | Exports, descriptor, host table, versioning rules |
| [spec/plugin-manifest.md](spec/plugin-manifest.md) | Implemented | `plugin.toml` |
| [spec/loading.md](spec/loading.md) | Implemented | Discovery, dependency ordering, failure semantics |
| [spec/services.md](spec/services.md) | Implemented | Host service registry, interface headers, kind handlers, core service definitions |
| [spec/rust-sdk.md](spec/rust-sdk.md) | Implemented | `parsec-orbit-plugin-abi`, `-sdk` and `-sdk-testing` crates, `freestanding!` runtime, size policy |
| [spec/js-sdk.md](spec/js-sdk.md) | Implemented | JS plugin model, `@parsec-orbit/sdk` TypeScript API, WebView operations |
| [spec/marketplace.md](spec/marketplace.md) | Implemented | Plugin index, sha256, signatures, safe installation, `parsec-orbit-market` |
| [spec/registry.md](spec/registry.md) | Partly implemented | Format-1 index additions (serial, yanked, mirrors, platform), cache, `host.json`, `trusted-keys.toml` |
| [spec/ui-service.md](spec/ui-service.md) | Draft | Declarative UI service (parsec-orbit.ui), winio companion window, schema, SDK bindings |
| [spec/macos-host.md](spec/macos-host.md) | Draft | macOS patched-app launch, loader, state isolation, signing and transactional installation |
| [spec/macos-platform.md](spec/macos-platform.md) | Draft | macOS paths, `dylib` plugins, per-service behaviour, marketplace |

"Draft" means agreed direction, not yet implemented; each draft spec lists its
open questions at the end.

## Design

* [design/architecture.md](design/architecture.md) - components, crate graph, size and concurrency policy
* [design/orbit-ui.md](design/orbit-ui.md) - evaluation of in-app tab hack vs winio external companion window, unified ABI, and activation model
* [design/roadmap.md](design/roadmap.md) - phases
* [design/macos/native-dependency-poc.md](design/macos/native-dependency-poc.md) - measured copy PoC and remaining signing/original-path gates
* [design/macos/bootstrap-checkpoint.md](design/macos/bootstrap-checkpoint.md) - M1a guarded activation: acceptance evidence and audit limits
* [design/macos/installer-checkpoint.md](design/macos/installer-checkpoint.md) - M1b transaction: apply/restore evidence and the unverified list
* [design/macos/extension-projects.md](design/macos/extension-projects.md) - Spicetify and alternative extension mechanisms
* [design/macos-phases.md](design/macos-phases.md) - macOS phases M0-M2: deliverables, exit criteria, dependencies, risks
* [design/size-report.md](design/size-report.md) - measured sizes: runtime spike, plugins, loader
* [design/releasing.md](design/releasing.md) - how releases and crate publishing are done
* [design/network-installer.md](design/network-installer.md) - signed release manifests and the install/upgrade/uninstall script
* [design/decisions/](design/decisions/) - decision records (`NNNN-title.md`)
* [design/macos/](design/macos/) - macOS support research: constraints, client internals, the loader-as-host design and its dynamic smoke test
* [guides/macos-console.md](guides/macos-console.md) - independent terminal log viewer
* [guides/macos-network-install.md](guides/macos-network-install.md) - verified release downloads and persistent macOS installation

## Related notes outside this repository

Reverse-engineering notes about the Parsec client (WebView bridge protocol,
sideloading evidence) live in the research repository next to this one
(`parsec/docs/webview-bridge.md`). The specs here only restate what a plugin
author needs.
