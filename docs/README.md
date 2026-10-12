# Parsec Orbit documentation

| Directory | Holds | Audience |
| --- | --- | --- |
| [`spec/`](spec/) | Normative specifications: ABI, manifest, loading rules, host services, SDK APIs. A change in behaviour changes the spec in the same commit. | Plugin authors, SDK and loader implementers |
| [`design/`](design/) | Why things are the way they are: architecture, decision records, roadmap. Not normative. | Contributors |
| [`guides/`](guides/) | Task-oriented how-tos (write a Rust plugin, write a TypeScript plugin, ship a plugin). Written together with the SDKs they describe. | Plugin authors |

## Specifications

| Document | Status | Summary |
| --- | --- | --- |
| [spec/plugin-abi.md](spec/plugin-abi.md) | Implemented (ABI 1, host revision 2) | Exports, descriptor, host table, versioning rules |
| [spec/plugin-manifest.md](spec/plugin-manifest.md) | Implemented | `plugin.toml` |
| [spec/loading.md](spec/loading.md) | Implemented | Discovery, dependency ordering, failure semantics |
| [spec/services.md](spec/services.md) | Registry and kind handlers implemented; core service tables draft | Host service registry, interface headers, kind handlers, core service definitions |
| [spec/rust-sdk.md](spec/rust-sdk.md) | Implemented | `parsec-orbit-plugin-abi`, `-sdk` and `-sdk-std` crates, `freestanding!` runtime, size policy |
| [spec/js-sdk.md](spec/js-sdk.md) | Implemented | JS plugin model, `@parsec-orbit/sdk` TypeScript API, WebView operations |

"Draft" means agreed direction, not yet implemented; each draft spec lists its
open questions at the end.

## Design

* [design/architecture.md](design/architecture.md) - components, crate graph, size and concurrency policy
* [design/roadmap.md](design/roadmap.md) - phases
* [design/size-report.md](design/size-report.md) - measured sizes: runtime spike, plugins, loader
* [design/releasing.md](design/releasing.md) - how releases and crate publishing are done
* [design/decisions/](design/decisions/) - decision records (`NNNN-title.md`)

## Related notes outside this repository

Reverse-engineering notes about the Parsec client (WebView bridge protocol,
sideloading evidence) live in the research repository next to this one
(`parsec/docs/webview-bridge.md`). The specs here only restate what a plugin
author needs.
