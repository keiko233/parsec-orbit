# Restructure: crate roles, multi-OS layering, market plugin, installer, no_std

Status: **implementation started** on branch `refactor/restructure`; the
owner's decisions are applied ([section 10](#10-decisions)). Progress, open
work and the Windows hand-off are in
[restructure-handoff.md](restructure-handoff.md). Accepted parts become ADRs 0017 onward
([section 9](#9-records-to-write)); existing ADRs are not rewritten.

Scope:

1. Separate `lib` from `plugin` and define crate responsibilities.
2. One architecture for several operating systems.
3. Fold the stand-alone market into a plugin; one spec for remote data, data
   format and interface; the official registry lives in `manifest/` on `main`.
4. Untangle build, packaging and installation; the installer becomes Rust.
5. Evaluate `no_std` per crate and migrate only where it pays.
6. One format policy: TOML for what people edit, JSON for what machines
   exchange, one source per fact ([section 7](#7-formats-and-single-sources)).

The break from the current layout happens once, at **0.2.0**: no
compatibility shims for the 0.1.x scripts and release assets. User data stays
compatible ([section 8](#compatibility-summary)).

Everything below is based on the tree at `6fc5c26`.

## 1. Findings

### 1.1 Classification

`crates/` holds 26 workspace members in one flat directory. Ten share the
`parsec-orbit-plugin-` prefix but are two different things:

| Crate | Actual role |
| --- | --- |
| `-plugin-config`, `-hooks`, `-js-runtime`, `-webview-bridge`, `-ui` | Provide a `parsec-orbit.*` service or kind handler. No user-facing feature of their own. Others `require` them. |
| `-plugin-high-bitrate-native`, `-high-bitrate-webview`, `-stun-swap` | User-facing features. Consume services. |

Nothing in the repository records the difference. The installer's idea of
"required / default / group" is a hand-written table in `scripts/package.ps1`
(`$pluginPolicy`), away from the manifests. The plugin lists are repeated in
`install.ps1`, `package.ps1`, `package-macos.py` (`PLUGINS`) and
`orbit-manager-macos.py`.

Non-plugin crates are also mixed in: loaders (`-loader`, `-loader-macos`),
shared Rust libraries (`-core`, `-devtools`, `-macos-state`, `-market`) and
executables (`-installer-macos`, `-market` bin, `-verify-*`).

### 1.2 Multi-OS

macOS was added by copying the Windows shape and suffixing `-macos`. A third OS
would copy it again.

| Duplicated thing | Windows | macOS |
| --- | --- | --- |
| Env to paths (plugin dirs, log candidates, data root, dedupe, split) | `loader/src/config.rs` (416 lines) | `loader-macos/src/config.rs` (304) |
| Same paths again | `orbit-manager.ps1`, `install.ps1` | `installer-macos/cli.rs`, `orbit-manager-macos.py`, `console-macos.py`, `market/cli.rs` |
| Log line format | `loader/src/log.rs` | `loader-macos/src/log.rs` (format kept equal by copy) |
| Logger sink, `remember(Loaded)` store, scan wrapper, "ready: N loaded" line | `loader/src/plugin.rs` | `loader-macos/src/lib.rs` |
| Per-plugin manifest | `plugin.toml` | `plugin.macos.toml` (config, hooks, js-runtime; differs only in `kind`/`entry`) |
| Release manifest | `release.json` | `release-macos.json` (different schema) |
| Installer | PowerShell, no journal | Rust transactional installer + sh wrapper + Python manager |

Other defects:

* `plugin.macos.toml` is unnecessary. The loader already defaults `kind` to the
  platform's native kind and `entry` to the first `*.dll`/`*.dylib`
  ([plugin-manifest.md](../spec/plugin-manifest.md)). But `market`'s
  `parse_manifest` defaults `kind` to `"dll"` instead, so the two parsers
  disagree about a kind-less manifest.
* `market::index::Platform` is a closed two-value enum and `current()` returns
  Windows for every target that is not macOS arm64. A Linux build would
  silently claim Windows.
* A plugin's version is written three times (`Cargo.toml`,
  `plugin.toml`, `declare_plugin!`). The Publish workflow bumps only
  `Cargo.toml`, `Cargo.lock` and `javascript-sdk/package.json`, so the next
  release publishes plugins whose manifests still say `0.1.1`.
* Process-global state in `parsec_orbit_core::host` (registry, logger) and the
  loaders (`OWNED`, `SINK`, `REGISTRY`). The C host table has no user pointer,
  so this is inherent today. Not in scope; recorded so it is not mistaken for a
  precedent.

### 1.3 Market

* `parsec-orbit-market` is a library plus a 911-line CLI, shipped as
  `parsec-orbit-market.exe` and in the macOS bundle, used by both managers
  purely as a signature verifier and zip installer. The reserved
  `parsec-orbit.market` interface has no provider.
* Remote data is one `index.json` per GitHub release
  (`.../releases/download/<tag>/index.json`). A plugin cannot ship a fix
  between loader releases, and a third-party source is a bare URL with no
  registry concept.
* `index.json` lacks what a browser needs (role, category, summary, homepage,
  publish date). Selection policy lives in a second file, `release.json`.
* No rollback protection: an old, validly signed `index.json` is accepted.
  No way to withdraw a bad release.
* `parsec-orbit.ui` already exists but nothing in it manages plugins; the
  Plugins tab is only planned ([orbit-ui.md](orbit-ui.md#4-the-three-pillar-realization-logic)).

### 1.4 Installer / build / packaging

| Script | Lines | Does | Problem |
| --- | --- | --- | --- |
| `install.ps1` | 183 | Install from a checkout | Duplicates `orbit-manager.ps1`: stop service, back up, copy, restart. Hard-coded plugin list; folders named by package, while the market uses the id. |
| `orbit-manager.ps1` | 1445 | Download, verify, select, install, state, WebView toggle, `ui` (Win32 P/Invoke) | Re-implements URL rules, semver, sha256, key handling already in Rust. `ui` is not an installer concern. |
| `set-web-ui.ps1` | 47 | `client_web_ui` toggle | Same logic exists in `orbit-manager.ps1` (`Set-WebViewSetting`). |
| `package.ps1` | 256 | Build, stage, zip, index, `release.json`, sign | Build, packaging and release policy in one script. Windows only. |
| `package-macos.py` | 135 | Same for macOS | Different output shape. |
| `install-macos.sh` | 282 | Build **and** install | Mixes build with install. |
| `orbit-manager-macos.py` | 1211 | Download, verify, extract zip | Third copy of manifest validation, zip safety, atomic write. |
| `console-macos.py` | 254 | Log viewer | Re-implements `core::console::Follower`. |
| `macos-*canary/audit` (10), `test-*` (8 in CI) | ~4000 + tests | Research and their tests | Not product automation; live next to it. |

Security-critical logic (manifest validation, URL pinning, zip extraction,
digest check, trusted keys) exists three times: Rust (`market`), PowerShell,
Python. The official public key is embedded in both managers. macOS end users
need Python 3; Windows users need PowerShell 5.1.

## 2. Target structure

### 2.1 Roles

| Role | Definition | Loaded in client | Dir |
| --- | --- | --- | --- |
| `sdk` | Contract for authors. Published. | no | `crates/` (+ `javascript-sdk/`) |
| `host` | Runs in or as the loader. | yes (`bcrypt.dll`, `.dylib`) | `crates/` |
| `shared` | Rust library linked by more than one component. | no | `crates/` |
| `lib` | A module that **provides a service or kind handler** and has no user-facing feature. Installed as a dependency. Still an ordinary ABI plugin at runtime. | yes | `libs/` |
| `plugin` | A **feature**. Uses services, may provide none. | yes | `plugins/` |
| `tool` | Executable for the host machine. | no | `tools/` |

`lib` is a role, not a new loader concept. The ABI, `plugin.toml` fields and
loading rules do not change; the loader still sees modules. Rust crates that
are libraries are `shared`, never `lib`.

Manifest: one optional field, `role = "lib" | "plugin"` (default `plugin`).
The loader ignores unknown keys. Consumers: the market UI hides libs behind
"show libraries", install resolves `requires` and installs libs as
dependencies, remove warns when a lib is still required.

### 2.2 Crates

| New package | Old | Role | Responsibility |
| --- | --- | --- | --- |
| `parsec-orbit-plugin-abi` | same | sdk | `repr(C)` contract. no_std, zero deps. Published, name fixed. |
| `parsec-orbit-plugin-sdk` | same | sdk | Safe Rust API, freestanding runtime. One package for `std` and `no_std` plugins, no features ([6.7](#67-the-sdk-as-one-package-adr-0002-re-evaluated)). Published. |
| `parsec-orbit-plugin-sdk-testing` | `-sdk-std` (only `MockHost` survives) | sdk | `MockHost`, a `std` test double. Dev-dependency only. Published. |
| `parsec-orbit-core` | same | host | OS-neutral loading: manifest, scan, plan, host tables, services, console follower. No serde/async. |
| `parsec-orbit-layout` | **new** | shared | Where things live: `Os`, platform id, Orbit root, plugin dirs, data, log candidates, backups, state, keys. Pure functions over an injected env lookup. std only, zero deps. |
| `parsec-orbit-loader-windows` | `-loader` | host | `bcrypt.dll` proxy: thunks, `DllMain`, Win32 `Platform`/log/console. Output name stays `bcrypt`. |
| `parsec-orbit-loader-macos` | same | host | Dylib entry, activation, isolation, `dlopen` `Platform`. |
| `parsec-orbit-macos-state` | same | shared | Atomic isolated-state seed (loader + installer). |
| `parsec-orbit-registry` | `-market` (lib) | shared | Index and host manifest model, sha256/ed25519, package reading, safe install, `Fetch`, cache. One implementation for installer, release tool and market plugin. |
| `parsec-orbit-lib-config` | `-plugin-config` | lib | `parsec-orbit.config` |
| `parsec-orbit-lib-hooks` | `-plugin-hooks` | lib | `parsec-orbit.hooks` (backends per OS) |
| `parsec-orbit-lib-js-runtime` | `-plugin-js-runtime` | lib | `kind.js`, `parsec-orbit.js` |
| `parsec-orbit-lib-webview-bridge` | `-plugin-webview-bridge` + `-devtools` | lib | `parsec-orbit.webview`; the DevTools client is a module of it, reachable by the verify tool through the `rlib` |
| `parsec-orbit-lib-ui` | `-plugin-ui` | lib | `parsec-orbit.ui` (companion window) |
| `parsec-orbit-plugin-high-bitrate-native` | same | plugin | Native UI bandwidth patch |
| `parsec-orbit-plugin-high-bitrate-webview` | same | plugin | WebView UI bandwidth patch (first `no_std` plugin, [section 6](#6-no_std-evaluation)) |
| `parsec-orbit-plugin-stun-swap` | same | plugin | STUN redirect |
| `parsec-orbit-plugin-market` | **new** | plugin | Browse/install/update from registries ([section 4](#4-market-as-a-plugin)) |
| `parsec-orbit-installer` | `-installer-macos` + scripts | tool | bin `orbit`: install, upgrade, uninstall, status, plugins, ui, logs |
| `parsec-orbit-release` | `package*.{ps1,py}` + market publisher CLI | tool | Package, build/sign the registry, keygen, verify (CI/publisher only) |
| `parsec-orbit-verify` | `-verify-callpaths`, `-verify-roundtrip`, `-verify-webview-inject` | tool | Loader and WebView-patch verification, three `[[bin]]`s with the old names |

`examples/` and `tests/` are unchanged. `hello` and `hello-nostd` are
plugins by role.

### 2.3 Directory tree

```text
crates/      abi  sdk  sdk-testing  core  layout  loader-windows  loader-macos
             macos-state  registry
libs/        parsec-orbit-lib-{config,hooks,js-runtime,webview-bridge,ui}
plugins/     parsec-orbit-plugin-{high-bitrate-native,high-bitrate-webview,stun-swap,market}
tools/       parsec-orbit-installer  parsec-orbit-release  parsec-orbit-verify
manifest/    the official registry (section 4.3)
examples/    unchanged        tests/   unchanged        javascript-sdk/  unchanged
research/    macos/ (the canary and audit scripts and their tests, not product)
```

Top-level `libs/` and `plugins/` rather than nesting inside `crates/`:
existing plugin path dependencies are written `../../crates/<name>`, which stays
valid one level down, and the crate directory still equals the package name.

### 2.4 Dependency law

| From | May depend on | Never |
| --- | --- | --- |
| `abi` | nothing | anything |
| `sdk` | `abi` | `std` (ADR 0002) |
| `core` | `abi`, `layout` | serde, async, `sdk`, `registry` (ADR 0004) |
| `loader-<os>` | `core`, `layout`, `abi`, OS crate | `registry`, `sdk` |
| `libs/*` | `sdk` (+ `sdk-testing` as dev-dependency), `shared` | other `libs/*`, `plugins/*`, `core`, loaders |
| `plugins/*` | `sdk` (+ `sdk-testing` as dev-dependency); `plugin-market` also `registry`, `layout` | `libs/*`, other plugins, `core`, loaders |
| `tools/*` | `registry`, `layout`, `macos-state`, `core` (pure helpers only) | `sdk`, loaders |

Libs and plugins meet only through services at run time, declared with
`requires` in `plugin.toml`. A `cdylib + rlib` pair is allowed only so tests
and verify tools can reach the code, and **never for a `no_std` plugin**: its
`freestanding!` panic handler and allocator would clash with `std` in the
consumer ([section 6.5](#65-costs-and-constraints)).

Enforcement: a check that reads `cargo metadata` and fails on a forbidden
edge, run from `scripts/verify.ps1` and CI (about 40 lines). It is the only new
automation in this item.

### 2.5 When a crate is split

A crate boundary is kept only when one of these holds; otherwise the code is
one package with modules.

1. **Forced by the toolchain.** Cargo or rustc cannot express it in one
   package: a different output file name per OS, a `#[panic_handler]` that
   cannot coexist with `std` in a build, a `std`-only test double that a
   `no_std` package cannot contain.
2. **Different consumers who must not depend on each other's half.** The host
   side must not link the author-facing API; a plugin must not link loader
   logic; a user-facing binary must not carry publisher code.

Review of the current splits:

| Split | Verdict | Reason |
| --- | --- | --- |
| `sdk` / `sdk-std` | **merged** (`sdk-std` becomes `sdk-testing`) | A feature cannot do it (6.7), but almost nothing in `sdk-std` needs `std` once the five call sites are inlined. Only the test double does. |
| `devtools` / `webview-bridge` | **merged** | Two consumers, one of which (the verify tool) can use the bridge's `rlib`; no boundary of kind 1 or 2. |
| `verify-callpaths` / `-roundtrip` / `-webview-inject` | **merged** into `parsec-orbit-verify` | Three tiny binaries of one purpose; `[[bin]]` names keep `verify.ps1` unchanged. |
| `market` lib+bin / `installer-macos` lib+bin | **one package each**, binary removed or merged into the installer | The library is the product. |
| `abi` / `sdk` | kept | Kind 2: `core` and the loaders depend on the contract only, never on the author API, the allocator or the assembly runtime. |
| `loader-windows` / `loader-macos` | kept | Kind 1: the DLL must be named `bcrypt`, the dylib is not, and Cargo cannot vary `[lib] name` by target. |
| `macos-state` | kept | Kind 2: the installer and the loader both need it and neither may depend on the other (one is a `cdylib`, one a binary). |
| `layout` | kept | Kind 2: plugins and tools need the paths without linking `core`'s host state. |
| `installer` / `release` | kept | Kind 2: the user binary does not carry key generation or registry building. |
| `hello` / `hello-nostd`, `fixture-plugin` / `fixture-nostd` | kept | They exist to cover the two flavours. |
| `high-bitrate-native` / `-webview` | kept | Two ids, two audiences, different `requires`; separately installable. |

## 3. Multi-OS architecture

### 3.1 Principle

Common logic is OS-free and tested with plain values and doubles. Each OS
implements a small, named set of ports. No `#[cfg(windows)]` or
`#[cfg(target_os)]` in common code; only in `os/` and `backend/` modules,
Cargo `[target]` tables and the single `pub use` that selects a backend.

### 3.2 Ports

| Port | Lives in | Windows | macOS |
| --- | --- | --- | --- |
| `Platform` (map library, native kind) | `core` (exists) | `LoadLibrary` | `dlopen` |
| `Logger`, `console::Console` | `core` (exist) | file + console window | file + stderr |
| `Layout` (paths) | `layout` (new) | `%APPDATA%\ParsecOrbit`, `<client>\plugins` | `~/Library/Application Support/ParsecOrbit` only |
| `Fetch` | `registry` (exists) | `ureq` | `ureq` |
| `DylibSigner` | `registry` (exists) | none | `codesign` ad hoc |
| `ClientIntegration` | `installer` (new trait) | swap `bcrypt.dll`, stop/start service, `client_web_ui` | patched bundle, transaction journal (existing code) |
| Service backends | inside one lib/plugin crate | `backend/windows.rs` | `backend/macos.rs` |

New in `core`, replacing the copies in both loaders:

* `log::format_line(level, tag, process, pid, message)`. The sinks stay
  OS-specific (raw `WriteFile` / `open(O_APPEND)`); the format is shared.
* `Session`: owns the `Vec<Loaded>` (today `REGISTRY`/`OWNED`), wraps `scan`,
  emits the summary line, carries `loader_version`.
* `staging::promote` ([section 4.5](#45-installing-from-inside-a-running-client)).

All are plain functions and a struct; no new dependency; the Windows loader
size is re-measured against the budget (ADR 0004, size policy) in the commit
that adds each.

### 3.3 `layout`

```text
Os::{Windows, MacOs}                 Os::current() by cfg; native_kind(), lib_ext()
PlatformId("windows-x86_64" | "macos-arm64" | ...)   open string, <os>-<arch>
Layout::resolve(os, env_lookup, home, loader_dir) -> Layout
  .root  .plugin_dirs()  .data_root()  .log_candidates()  .backups()
  .state_file()  .trusted_keys()  .market_cache()  .pending()
```

`resolve` takes the environment as a closure, like `Config::from_lookup` today,
so the unit tests already written for both configs move over unchanged. The
`PARSEC_ORBIT_*` variables and their precedence are preserved byte for byte:
`PARSEC_ORBIT_PLUGIN_DIR`, `PARSEC_ORBIT_LOG`, `PARSEC_ORBIT_HOME`,
`PARSEC_ORBIT_CONSOLE`, `PARSEC_ORBIT_DEVTOOLS*`, `PARSEC_ORBIT_HOST`.

Loader-only settings (devtools port, console target, Parsec's own logs on
Windows; activation and isolation on macOS) stay in each loader's `Config` and
read the shared `Layout`.

### 3.4 Plugins on several OSes

One crate, one `plugin.toml`, backends:

```text
libs/parsec-orbit-lib-hooks/
  plugin.toml                 role, id, requires; no kind, no entry
  src/lib.rs                  common logic + service table (no cfg)
  src/backend/windows.rs
  src/backend/macos.rs
  src/backend/unsupported.rs  compiles everywhere; service returns -4
```

`hooks` already follows this. `stun-swap`, `high-bitrate-native` and
`lib-ui` adopt it: their Windows code moves under `backend/windows.rs`, and an
`unsupported` backend keeps `cargo check --workspace` green on every developer
OS (the current "compiles empty" convention, made explicit). `lib-ui` selects
`winio` features per target in `[target.'cfg(...)'.dependencies]`.

`plugin.macos.toml` is deleted. The source manifest omits `kind` and `entry`;
`parsec-orbit-release package` writes the resolved `kind` and `entry` into the
staged manifest for the target platform. The loader's defaults already cover a
hand-copied install. `registry` aligns its default `kind` with the loader.

### 3.5 Adding an OS (checklist)

`layout` OS variant and platform id; `loader-<os>`; installer `ClientIntegration`;
`backend/<os>.rs` per plugin that supports it (others get `unsupported`);
`[build].platforms` entries in `manifest/`; one CI job. Nothing in `core`,
`abi`, `sdk` or `registry` changes. Linux is not planned; this list is the
check that the design does not block it.

## 4. Market as a plugin

### 4.1 Decision

The stand-alone `parsec-orbit-market` tool is retired. Its code becomes the
`registry` library, consumed by three clients with no second implementation:

| Client | Use |
| --- | --- |
| `plugins/parsec-orbit-plugin-market` | In-client browse, install, update, remove |
| `tools/parsec-orbit-installer` | `orbit plugins ...`, install/upgrade selection |
| `tools/parsec-orbit-release` | build and sign the registry, package, verify |

The reserved `parsec-orbit.market` interface is **not** implemented. No plugin
needs it; `market.json` stays the install record. It remains reserved.

### 4.2 Plugin

* Id `com.parsec-orbit.market`, `role = "plugin"`, `requires =
  ["com.parsec-orbit.ui"]`, `optional = ["com.parsec-orbit.config"]`.
* Built on `sdk` (a `std` plugin: TLS, zip and threads; [section 6](#6-no_std-evaluation)).
  One actor thread owns the registry client and a command channel (the pattern
  of the webview bridge and JS runtime, ADR 0004 amendment). UI action
  callbacks only enqueue; they never fetch or block.
* Registers one `parsec-orbit.ui` section. No new UI item type is added for the
  market (see 4.4).
* Heavy dependencies (HTTP/TLS, zip, ed25519) live in this separate download;
  the loader is unaffected. Plugin size is measured and recorded in the size
  report before release.
* Only the client instance the user started (the one that owns the companion
  window) runs the actor, as `lib-ui` already does; the service instance does
  nothing.

### 4.3 The official registry: `manifest/` on `main`

**Principle.** The registry is a catalogue, not a file store. `manifest/` in
the `main` branch holds small text files: what exists, where its package is,
which sha256 it must have. Packages stay where they are built (GitHub release
assets, or any HTTPS URL for third parties). Git history is the audit log;
a pull request is how an entry is added. The signature, not the hosting,
makes it trustworthy.

#### Layout

```text
manifest/
  README.md                     rules for submitters
  host/
    releases.toml               loader + installer artifacts, one [[release]] per version and platform (appended by the release job)
  plugins/<id>/
    entry.toml                  identity and presentation (hand-written)
    releases.toml               one [[release]] per package (appended by the release job; PR for third parties)
  index.json   index.json.sig   GENERATED, signed: all plugins and libs
  host.json    host.json.sig    GENERATED, signed: loader and installer
```

`<id>` is the plugin id, used as the folder name exactly as the loader and
`orbit` do.

#### Source files

`plugins/<id>/entry.toml`; everything but `id` is optional because the rest is
derived from the package's own `plugin.toml` when the registry is built:

```toml
id = "com.parsec-orbit.webview"
default = true                 # pre-selected by `orbit install`
group = "UI patches"           # grouping in the installer prompt and market UI
category = "UI patches"        # overrides the derived value
summary = "Adds bandwidth limits above 50 Mbps to the WebView UI"   # <= 120 chars
homepage = "https://github.com/keiko233/parsec-orbit"

[build]                        # official modules only; read by `parsec-orbit-release package`
platforms = ["windows-x86_64"]
```

The crate behind an official entry is found by scanning the workspace for the
`plugin.toml` whose `id` matches (next to the `Cargo.toml` that names the
package); it is not written down a second time.

`plugins/<id>/releases.toml`:

```toml
[[release]]
version = "0.2.0"              # must equal the version in the package's plugin.toml
platform = "windows-x86_64"
url = "https://github.com/keiko233/parsec-orbit/releases/download/v0.2.0/parsec-orbit-plugin-high-bitrate-webview-windows-x86_64.zip"
mirrors = []                   # optional, tried in order after `url`
sha256 = "<64 lowercase hex>"
published = "2026-11-01"
yanked = false                 # true: hidden from selection, warned about when installed
```

`host/releases.toml`:

```toml
[[release]]
version = "0.2.0"
api = 1
platform = "windows-x86_64"
loader    = { url = "https://github.com/.../bcrypt.dll", sha256 = "...", size = 374272 }
installer = { url = "https://github.com/.../orbit-windows-x86_64.exe", sha256 = "...", size = 0 }
```

Appending a block is a plain text append, so the release job never rewrites
hand-edited content. `yanked` is a human edit.

#### Generated files

`parsec-orbit-release registry build` reads `manifest/`, downloads every
package it has not seen in the previous `index.json`, checks sha256, reads the
package's `plugin.toml`, and writes the materialised, sorted, stable
`index.json` (format `1`, fields as in [marketplace.md](../spec/marketplace.md)
plus):

| Level | Field | Source |
| --- | --- | --- |
| index | `serial` (previous + 1, only when content changed), `generated` | build |
| plugin | `role`, `summary`, `category`, `homepage`, `default`, `group`, `official` | package manifest, `entry.toml`; `official` = id starts with `com.parsec-orbit.` |
| plugin | `required` | derived: a `lib` in the `requires` closure of any `default` plugin |
| release | `published`, `yanked`, `mirrors`, `size`, `api`, `requires`, `platform` (open `<os>-<arch>` string) | `releases.toml`, package |

`host.json` has `format`, `serial`, `generated`, `latest` and the
`releases` of `host/releases.toml` in the same materialised form. It replaces
`release.json` and `release-macos.json`; `distribution.toml` and
`manifest.json` from the earlier draft are not needed.

The generated files are the only thing clients read. Readers ignore unknown
keys, so the format number stays `1` for `index.json`.

#### Build, sign, publish

* `parsec-orbit-release registry check` (pull requests): schema, id rules,
  reserved prefix (`com.parsec-orbit.` only for entries with a `[build]`
  section), HTTPS URLs, download + sha256 + package validation with the
  `registry` library, version equals the package's, id unique, size caps.
* `parsec-orbit-release registry build --sign <key>` (push to `main` that
  touches `manifest/plugins/**` or `manifest/host/**`, and the release job):
  writes and signs the two generated files, commits them as the bot with
  `[skip ci]`. Concurrency group `registry` serialises it; `serial` comes from
  the committed index, so it is monotonic. The bot already pushes to `main`
  for the Publish workflow, so branch protection already allows it.
* The signing key is a secret of the release and registry jobs only.

#### Fetch and update (clients)

Source list: `<root>/market/sources.toml`; absent file = the official source:

```toml
[[source]]
name = "official"
urls = [
  "https://raw.githubusercontent.com/keiko233/parsec-orbit/main/manifest/",
  # optional mirror; safe because every byte is signature-checked
  "https://cdn.jsdelivr.net/gh/keiko233/parsec-orbit@main/manifest/",
]
```

1. `GET index.json.sig` with `If-None-Match`. The signature is 88 bytes and
   changes whenever the index does, so an unchanged `ETag` or content means
   nothing to do.
2. Otherwise `GET index.json`, verify the signature against `trusted-keys.toml`
   plus the official key built into `orbit` and the market plugin, and refuse
   an index whose `serial` is lower than the cached verified one. If the two
   files come from different commits (CDN skew) verification fails: retry up to
   three times 10 s apart, then keep the cache and report "registry is updating,
   try again".
3. Store the verified `index.json`, `.sig` and `ETag` in
   `<root>/market/cache/<source>/`. Nothing unverified is cached.
4. A package is fetched from `url`, then each mirror; size cap first, then
   sha256 against the index; the first match wins.
5. `host.json` follows the same steps; only `orbit upgrade` reads it.

Cadence: on `orbit plugins ...`, on the market's Refresh button, and once when
the companion window opens if the cache is older than 24 h. No background
polling. Offline, the cache is shown read-only and marked stale; installs need
the network anyway.

Trust, unchanged in model: a package is trusted by sha256 and size from a
signed index; the index by a key the user trusts. For the official registry the
signature means "the maintainers listed this", including third-party packages
hosted elsewhere; the UI shows `official` separately from the author. A source
without a trusted key is shown as unverified and not installable unless the
user overrides it. A leaked key is handled by changing the key list; `yanked`
withdraws a release without one.

### 4.4 UI interface (reuse of `parsec-orbit.ui`)

Section id `com.parsec-orbit.market`, built only from existing item types:

| key | type | Role |
| --- | --- | --- |
| `source` | `select` | Registry source |
| `filter` | `select` | All / Installed / Updates / Official |
| `show_libs` | `toggle` | Include `role = "lib"` |
| `plugin` | `select` | Picker, "Name vX.Y (state)" |
| `details` | `readonly_text` | Summary, author, license, requires, size, source, trust state, yanked |
| `install` / `update` / `remove` | `button` | Act on `plugin` |
| `refresh` | `button` | Re-fetch |
| `status` | `readonly_text` | Progress, errors, "restart Parsec to apply" |

Action events are the existing `{section, key, type, value}`.

Known limit: the dropdown's options are fixed at registration, so a changed
list means re-registering the section, which resets the user's place in the
panel. Planned phase 7 ABI work already needs dynamic options and a multiline
`text_view` ([orbit-ui.md](orbit-ui.md#61-what-the-abi-is-missing)) for STUN
and the Log tab; the market adopts those when they land and adds nothing of its
own. A table/list item is not added for the market alone.

### 4.5 Installing from inside a running client

* **New plugin or lib:** staged next to the target, swapped in, loads at the next
  start. No loader involvement.
* **Update of a loaded module** (decided): Windows will not replace a mapped DLL
  or rename its folder. The plugin writes to `<root>/pending/<id>/`; at the
  start of `scan`, `core` runs `staging::promote(plugins_dir, pending_dir)`:
  rename the existing folder to `<id>.old`, rename the pending one in, delete
  `<id>.old` on the next start; roll the old one back if the second rename
  fails. `std::fs` only, no new dependency, unit tested with temp dirs. It is
  the only loader-side change in this design. The commit that adds it records
  the `bcrypt.dll` size change in the size report (ADR 0004); a failed promote
  is logged and the old module loads.
* The UI plugin's own update goes the same way.

## 5. Build, package, install

### 5.1 Responsibilities

| Stage | Owner | Input | Output | Runs |
| --- | --- | --- | --- | --- |
| Build | `cargo build --release --locked` | source | `target/release/*` | dev, CI |
| Verify | `scripts/verify.ps1`, tests | build | pass/fail | dev, CI |
| Package | `parsec-orbit-release package` | build + `manifest/plugins/*/entry.toml` `[build]` | `dist/<platform>/` | CI per OS, dev |
| Publish | `parsec-orbit-release registry add` + `build --sign` | `dist/`, uploaded assets | `manifest/` commit | release job |
| Install | `orbit` | registry (network) or `--from <dist>` | client changed | user |

Rules: build never packages, packaging never installs, install never builds.
`install-macos.sh --no-build` and the "build inside install" mode disappear:
developers run `scripts/package.ps1` then `orbit install --from dist`
(`package` also writes a local, dev-signed `dist/index.json` and `host.json`
so the whole path is testable without the network).

### 5.2 Release assets

```text
orbit-<platform>(.exe)                       installer, one per platform
loader: bcrypt.dll | libparsec_orbit_loader.dylib
parsec-orbit-<role>-<name>-<platform>.zip    plugin and lib packages
SHA256SUMS.txt
get-orbit.ps1  get-orbit.sh                  bootstrap (<60 lines each)
```

No JSON manifest ships in the release: `manifest/` is the single place that
describes releases, and `host.json` points at these assets. A plugin
package's inner `plugin.toml` has `kind`/`entry` written by `package` for the
target platform.

### 5.3 Bootstrap and trust

`get-orbit.{ps1,sh}` download `orbit-<platform>` over TLS from the pinned
repository's latest release, check it against `SHA256SUMS.txt`, and run
`orbit install`. That first fetch is TLS-only, as in ADR 0006; from then on
`orbit` verifies `host.json` and `index.json` with the Rust ed25519 code. The
official public key is a constant in `orbit` and in the market plugin (one
place each, not two scripts). `orbit` verifies a newer `orbit` through
`host.json` before replacing itself.

### 5.4 Installer

`tools/parsec-orbit-installer`, bin `orbit`:

```text
src/main.rs  cli.rs
src/common/  registry.rs (sources, cache, verify)  select.rs  state.rs  plan.rs
src/os/mod.rs            trait ClientIntegration { inspect, apply, restore, status, restart_hint }
src/os/windows.rs        service stop/start, bcrypt backup+swap with retry, client_web_ui toggle, `ui` activation
src/os/macos/            apply, macho, journal, lock, privilege, ... (moved unchanged from installer-macos)
```

Commands: `install`, `upgrade`, `uninstall`, `status`, `plugins
list|install|remove|update`, `ui` (post the activation message), `logs
[--follow]` (uses `core::console::Follower`; replaces the Python viewer),
`webview-ui on|off|keep` (replaces `set-web-ui.ps1`). Flags map one to one from
the PowerShell options (`-Plugin` to `--plugin`, `-DryRun` to `--dry-run`, ...);
the table goes in the migration guide.

State is preserved: `%APPDATA%\ParsecOrbit\install-state.json` format 1 is read
and rewritten with additive fields (`platform`); the macOS journal and state
files are untouched. `upgrade` also migrates legacy plugin folders
(`plugins\parsec-orbit-plugin-*\` created by `install.ps1`, no `market.json`)
whose manifest id is official into `plugins\<id>\`; today both naming
schemes can coexist and the loader then skips the second as a duplicate.

Tests replace the Python and shell tests with Rust: `Fetch` double, scratch
Orbit roots and a fake `parsecd.exe` / scratch bundle (the existing
`scratch_apply.rs` approach). No test touches a real client.

### 5.5 CI

* `test.yml`: as today plus the dependency-law check; Python tests go with
  their scripts.
* `release.yml`: per OS build, `verify`, `parsec-orbit-release package
  --platform X`, upload to the release; then one `publish` job (ubuntu) that
  runs `registry add` for the uploaded assets, `registry build --sign`, and
  commits `manifest/`.
* `registry.yml` (new): `registry check` on pull requests that touch
  `manifest/`; `registry build --sign` on push to `main`.
* `scripts/package.ps1` stays as the thin entry (`cargo build` then the tool),
  on every OS through `pwsh`. `research/macos/` keeps the evidence scripts,
  ungated except the tests CI runs today (path change only).

## 6. no_std evaluation

`no_std` is a means. The question per crate is whether dropping `std` buys
something real for its cost.

### 6.1 Method

* `std` API families counted per crate outside `#[cfg(test)]`; dependency
  sources inspected for `#![no_std]` / feature support.
* Three throwaway crates in a scratch directory outside the repository,
  built with the workspace release profile (`opt-level = "z"`, fat LTO,
  `panic = "abort"`, stripped) on `aarch64-apple-darwin`, path-depending on the
  SDK: the real `high-bitrate-webview` logic as `no_std`, and one JSON
  workload (parse, modify, serialize with `serde_json`) as `std` and as
  `no_std + alloc`. Nothing in the repository was changed.

### 6.2 What is already `no_std`

* `abi`: `#![no_std]`, no `alloc`, zero dependencies.
* `sdk`: `#![no_std]`, `core` only. It ships `freestanding!`: a process-heap
  global allocator, a logging panic handler and (Windows x86_64) the C
  runtime symbols as assembly. A `no_std` plugin can therefore use `alloc`
  today; `fixture-nostd` exercises it. It has no `std` feature and cannot
  have one ([6.7](#67-the-sdk-as-one-package-adr-0002-re-evaluated)).
* The build is its own guard: `hello-nostd` and `fixture-nostd` define the
  panic handler, so `std` anywhere in their dependency graph is a duplicate
  lang item error.

### 6.3 Where `std` is actually used

Outside tests, by what it is used for:

| Crate | `std` used for | Without `std` |
| --- | --- | --- |
| `high-bitrate-webview` | one `std::env::var` | `GetEnvironmentVariableW` / `getenv`, about 20 lines |
| `high-bitrate-native` | `env::var`, `thread::sleep`, `Instant`/`Duration`; `serde_json`; raw patching via `windows-sys` | three Win32 calls; `serde_json` and `windows-sys` are `no_std` capable |
| `lib-config` | `fs` (read, write, rename, create_dir), `OnceLock`, `HashMap`, `Path` | raw file I/O and atomic rename per OS |
| `lib-hooks` | `retour` (needs `std`), `Mutex`, `HashMap` | not possible with `retour`; macOS backend is libc-heavy |
| `lib-webview-bridge` | TCP sockets (its `devtools` module), threads, `mpsc`, time | raw sockets and threads per OS |
| `lib-js-runtime` | Boa (needs `std`), threads, `fs` | not possible |
| `lib-ui` | `winio` (needs `std`), `RwLock`, `HashMap` | not possible |
| `plugin-stun-swap` | `UdpSocket`, threads, `fs`, `env`, time | raw sockets and threads per OS |
| `plugin-market` (new) | TLS (`ureq`/`rustls`), `zip`, threads | not possible |
| `core` | `Path`/`PathBuf` in `Platform::load` and everywhere paths are passed; `fs::read_dir`/`read_to_string` (`scan`, `manifest`); `Mutex`/`OnceLock`/`HashSet` (`host`); thread, `SystemTime`, `Duration` (`console`); `toml` with its `std` feature | pure parts (`plan`, `kind`, `Level`/`Logger`, manifest model) are `alloc`-only; `scan`, `host`, `console` are not |
| loaders | `Path` (about 30 references Windows, 110 macOS), `fs`, `sync`, `env`, `process`; macOS also 50+ FFI sites | rewrite of paths and file access over Win32 / libc |
| `registry`, `installer`, `release`, `macos-state`, `layout` | `fs`, `net`, `process`, `zip`, TLS, `PathBuf` | not meaningful; no `no_std` consumer exists |

Dependencies: `serde_json` (`alloc`), `sha2`, `ed25519-dalek`, `semver`,
`base64`, `tracing-core` and `windows-sys` work without `std`; `toml` parses
without `std` (`default-features = false` and no `std` feature); `retour`,
Boa, `winio`, `ureq`/`rustls` and `zip` require `std`.

### 6.4 Measured benefit

Bytes of the loaded library, release profile.

| Build | `std` | `no_std` | Saved |
| --- | ---: | ---: | ---: |
| `hello` / `hello-nostd`, macOS arm64 | 285,472 | 50,272 | 235,200 |
| `high-bitrate-webview` logic, macOS arm64 | 302,208 | 50,336 | 251,872 (83 %) |
| JSON workload with `serde_json`, macOS arm64 | 302,000 | 83,776 | 218,224 (72 %) |
| `hello` / `hello-nostd`, Windows x64 (existing [size report](size-report.md)) | 210,432 | 10,752 | 199,680 |

* The `std` cost is a constant of 200 to 250 KB per module, not proportional to
  the code. A `no_std` module with a real dependency (`serde_json`) is still
  about a quarter of its `std` twin.
* The macOS `no_std` floor (50 KB) is Mach-O and libSystem overhead; Windows
  has none.
* Applied to the default install: `high-bitrate-webview` 273,920 B drops to
  roughly 15 to 20 KB on Windows; `high-bitrate-native` 257,536 B to roughly 60
  to 90 KB (estimate from the JSON row, not measured on Windows). About 0.4 MB
  less out of a default set of roughly 3 to 4 MB.
* Performance: no effect. Every candidate does its work in `init`, once; none
  is on a hot path.
* Memory: each `std` module carries its own copy of `std` statics and a static
  CRT; both are small next to the client process.
* Not measured: the loader without `std` (no Windows linker here). From the
  table, up to 100 to 200 KB of the 374 KB is the `std` plus CRT floor, but
  `core::fmt`, `alloc` and `toml` come back, so the saving is an estimate.

### 6.5 Costs and constraints

* **Per-OS facilities.** Without `std` a module calls the OS itself:
  environment, sleep, clock, files, threads, sockets. That is the portability
  `std` provides for free, which the multi-OS design relies on. It is cheap for
  env, sleep and clock (three small functions) and expensive for files,
  threads and sockets.
* **Not linkable from `std` crates.** A `no_std` plugin expands
  `freestanding!` in its own crate. Compiled as an `rlib` into a `std` binary
  it would clash with the host's panic handler and allocator. Today
  `verify-webview-inject` uses `high-bitrate-webview` as an `rlib`
  (`BITRATE_SCRIPT`); the converted plugin is `cdylib` only and that tool reads
  `assets/bandwidth.js` itself.
* **Every dependency must be `no_std` capable**, and the plugin keeps
  `panic = "abort"` in every profile (already workspace policy).
* **Windows freestanding is `x86_64` only** (assembly runtime), the platform
  we ship.
* **Tests** keep working: `MockHost` from `sdk-testing` as a dev-dependency,
  as `hello-nostd` does.
* **Two flavours to maintain.** Mitigated by keeping the set small and the
  recipe in [rust-plugin.md](../guides/rust-plugin.md).

### 6.6 Decision

| Group | Verdict | Reason |
| --- | --- | --- |
| `sdk` (`abi`, `sdk`) | stay `no_std`; one `sdk` package serves both flavours | Already true; build-enforced. |
| `sdk-testing` | `std` | A test double; dev-dependency only. |
| `plugin`: `high-bitrate-webview` | **convert** | One `env::var` is the whole dependency; saves about 250 KB of a default-selected plugin for roughly a day of work. |
| `plugin`: `high-bitrate-native` | **convert second**, after the first lands | Three small OS calls; saves an estimated 150 to 190 KB; Windows-only so no cross-OS cost. Measure on Windows first; skip if the saving is under 100 KB. |
| `plugin`: `stun-swap`, `market` | stay `std` | Sockets, threads, TLS, zip. |
| `lib`: all five | stay `std` | `retour`, Boa, `winio` cannot; `config` could (about 200 KB) but needs raw file I/O and atomic rename on two OSes, which is the cost `std` exists to avoid; the saving does not justify it. |
| `shared`, `tool` | stay `std` | I/O, network and archives; no `no_std` consumer. |
| `core`, loaders | **defer**, with a spike gate | See below. |

**Loader gate.** The loader is the one place where `std` removal could be
large, and it is already over its size budget (374,272 B against 368,640 B).
The `toml` parser stays as it is: it is not replaced, rewritten or trimmed for
size (owner's decision). If the loader is still over budget after Phase 2,
run a spike on a branch: build
`loader-windows` and the `core` subset it needs without `std` and CRT, behind
the existing `Platform`/`Logger` traits. Go only if it saves at least 100 KB
and stays within a week; otherwise stop. This is not part of 0.2.0, and nothing
in phases 1 to 5 prevents it: `layout` and the registry stay `std`, and
`core` gains no `std`-only code in `plan`, `kind` or the logger traits.

**Feature flags.** None on the SDK ([6.7](#67-the-sdk-as-one-package-adr-0002-re-evaluated)).
A module is `no_std` or `std` for its whole life; where a `std`-only part must
sit next to a `no_std` one, it is a separate crate (`sdk-testing`).

**Support added.** If and when the second plugin converts, `sdk` gains a small
`os` module (`env_var`, `sleep_ms`, `tick_ms`: kernel32 through `raw-dylib` on
Windows, libSystem on macOS, the same technique `ProcessHeap` uses), so the
two plugins do not each carry their own copy. A semver-minor addition to a
published crate; not added for a single user.

### 6.7 The SDK as one package (ADR 0002 re-evaluated)

Question: can `no_std` vs `std` be a feature of one SDK package?

**Not as a feature that links `std`.** Reproduced in a scratch workspace
(`sdk` copy with `std = []` that does `extern crate std`, one `no_std` plugin
using `freestanding!`, one plugin enabling the feature, resolver 2):

| Command | Result |
| --- | --- |
| `cargo build -p nostd` | builds |
| `cargo build --workspace` | `error[E0152]: found duplicate lang item panic_impl` in `nostd` |
| `cargo build -p nostd -p stdp` | the same error |

Cargo unifies a package's features across everything built in one invocation,
so one `std` plugin turns `std` on for every `no_std` plugin in the workspace,
and `std` anywhere in a graph forbids the plugin's `#[panic_handler]`.
Dev-dependencies unify the same way under `cargo test --workspace` and
`cargo check --all-targets`. No `cfg`, macro or resolver setting can undo it;
the only workarounds (a second workspace for `no_std` plugins, or building each
plugin with `-p` and never testing the workspace) put correctness in the build
invocation. ADR 0002's amendment was right and stays right.

**But the crate does not have to exist.** `sdk-std` held five things:

| Item | Users | Outcome |
| --- | --- | --- |
| `prelude` / re-export of the SDK | 13 files | A `std` plugin imports `parsec_orbit_plugin_sdk` directly, as a `no_std` plugin does. `declare_plugin!` is the same macro. |
| `ContextExt::spawn` (a thread named `<id>:<name>`) | 5 calls | Inlined: `thread::Builder::new().name(format!("{}:{}", cx.plugin_id(), name))`. The trait is deleted. |
| `ContextExt::install_panic_hook` | 1 call (`stun-swap`) | Inlined there (`std::panic::set_hook`). |
| `TracingExt` / `tracing` feature | 0 outside its own tests | Removed. Its `tracing-core` dependency turns `std` on through `once_cell` when anything else in the build enables `tracing`'s `std`, which is the same unification trap. Re-added as its own crate if a plugin needs it. |
| `testing::MockHost` | 9 test modules | Stays `std` (a `thread_local` state, `CString`), so it is the one thing that cannot join the `no_std` package. It becomes `parsec-orbit-plugin-sdk-testing`. |

Result: one SDK package with no features for both flavours, and a small
test-only crate. A feature switch is not needed because there is nothing left
for it to switch.

## 7. Formats and single sources

### 7.1 Rule

| Use | Format | Why |
| --- | --- | --- |
| Written or reviewed by a person: manifests, source metadata, user settings, key lists, policy | **TOML** | Comments, readable diffs, native dates, already the format of `Cargo.toml` and `plugin.toml`. |
| Exchanged between programs or generated: indexes, install records, caches, stored settings, API-like documents | **JSON** (JSON Lines for append-only records) | Strict, universal, no ambiguity, `serde_json` is already everywhere. |
| Required by an external tool or specification | whatever it requires | Not changed. |

A file is never both: a generated file is JSON and is not edited; an edited
file is TOML and a program only appends a block to it (`releases.toml`,
`trusted-keys.toml`) or converts it once from an old format; it never
re-serialises a file a person owns. `releases.toml` is the deliberate mixture:
appended by the release job, edited by hand only to set `yanked`.

Conventions for every JSON document we define:

* an integer `format` (the schema version) at the top level, never
  `schema_version`, `version` or `$schema`;
* `snake_case` keys; enum values `kebab-case` where they already are;
* timestamps RFC 3339 UTC strings (TOML files use native TOML dates and
  datetimes); sizes integer bytes; hashes lowercase hex sha256; signatures,
  keys base64;
* URLs absolute `https`, or relative to the document with no `..`;
* unknown keys are ignored by readers (forward compatible), unknown `format`
  is refused.

The Rust types in `parsec-orbit-registry` are the schema of every document the
registry, installer and release tool exchange; the specs quote examples that a
test parses, so a spec and its type cannot drift.

### 7.2 Inventory and decision

| File | Today | Role | Decision |
| --- | --- | --- | --- |
| `Cargo.toml`, `.cargo/config.toml`, `cliff.toml` | TOML | build, tooling | keep (required) |
| `package.json`, `tsconfig*.json`, `renovate.json` | JSON | npm, TypeScript, Renovate | keep (required) |
| `.github/workflows/*.yml`, `pnpm-workspace.yaml`, `pnpm-lock.yaml` | YAML | GitHub, pnpm | keep (required); the only YAML in the repository, and CI logic moves into the Rust tools so the YAML stays thin |
| `plugin.toml` | TOML | plugin identity and loading | keep; source tree omits `version` and `kind`/`entry` (7.3) |
| `plugin.macos.toml` | TOML | duplicate of the above | delete ([3.4](#34-plugins-on-several-oses)) |
| `manifest/**/entry.toml`, `releases.toml` | new | registry source | TOML ([4.3](#43-the-official-registry-manifest-on-main)) |
| `manifest/index.json`, `host.json` | new | registry, generated | JSON |
| `index.json` (release asset) | JSON | same data | replaced by `manifest/index.json` |
| `release.json`, `release-macos.json` | JSON, two schemas | release description | replaced by `host.json`, one schema |
| `trusted-keys.txt` | custom lines `ed25519 <key> [comment]` | user-edited key list | **TOML** `trusted-keys.toml`: `[[key]]` with `algorithm`, `public`, `comment`; the installer reads the old file once and rewrites it; a `.txt` is never written again |
| `servers.txt` (stun-swap) | custom lines `host@port` | user-edited list | **TOML** `servers.toml`: `servers = ["stun.qq.com@3478"]`; the plugin reads the old file if the new one is absent. The grammar of an entry is unchanged |
| `sources.toml` (new) | TOML | user-edited registry list | TOML |
| `install-state.json` | JSON, `camelCase` keys, `format` 1 | install record | JSON, `snake_case`, `format` 2; reader accepts format 1 |
| `market.json` | JSON, `installed_unix` | per-plugin install record | JSON; adds `format`; times as RFC 3339 |
| `<data>/config.../<owner>.json` | JSON | config service store | keep (machine state) |
| macOS `manifest.json`, `journal.jsonl`, state | JSON, `schema_version` | transaction record | keep as is: installed state, moved unchanged, versioned by its own field |
| `*.sig` | base64 of the raw signature | detached signature | keep; not a configuration |
| `SHA256SUMS.txt` | `sha256sum` format | bootstrap check | keep: the standard format, and the bootstrap scripts have no JSON parser |
| `fixture.cfg` | custom `key=value` lines | e2e fixture script | keep, test-only and not shipped |
| Loader log, Parsec log | text lines | logs | keep; not configuration |
| Parsec `config.json` (`[comment, settings]`) | JSON, external | client settings | keep; the installer edits it in place and preserves key order and the rest |
| `PARSEC_ORBIT_*` | environment | loader switches | keep; `layout` is the one place that lists and reads them |

Nothing else in the tree is YAML or a private format. No new loader settings
file is introduced: the loader keeps reading the environment (a settings file
would be a feature, not a unification).

### 7.3 One source per fact

| Fact | Single source | Derived into |
| --- | --- | --- |
| Version of a module | `[workspace.package] version` in `Cargo.toml` (Rust) or `package.json` (TypeScript) | the staged `plugin.toml` (stamped by `package`), the descriptor (`declare_plugin!` takes it from `CARGO_PKG_VERSION` if the macro allows it, else a test fails on a mismatch), `index.json`, `host.json`. The source `plugin.toml` has no `version`. |
| Identity: `id`, `name`, `description`, `author`, `license`, `requires`, `provides`, `role`, `permissions`, `api` | the module's `plugin.toml` | `index.json` (read from the package when the registry is built); the descriptor (checked against it by one shared test helper in `sdk-testing`, replacing the per-plugin copies) |
| `kind`, `entry` | the build: platform and artifact | the staged `plugin.toml` written by `package`; the loader's defaults for a hand-copied install |
| Which crate builds a module | discovered by scanning for `plugin.toml` next to a `Cargo.toml` | never written in `manifest/` |
| Platforms, default selection, group | `manifest/plugins/<id>/entry.toml` | `index.json`, installer prompt, market UI |
| Required libs | the `requires` closure of the default plugins | `index.json` (`required`) |
| Package URL, sha256 | `manifest/plugins/<id>/releases.toml` | `index.json`; size and metadata re-derived from the package |
| Loader and installer artifacts | `manifest/host/releases.toml` | `host.json` |
| Release-asset checksums | computed once by `package` | `SHA256SUMS.txt`, `releases.toml`, `host/releases.toml` |
| Paths and platform id | `parsec-orbit-layout` | loaders, installer, market plugin, tools |
| Official public key | one constant in `registry`, used by `orbit` and the market plugin | nowhere else (scripts no longer embed it) |
| JSON schema | Rust types in `registry` | specs (tested examples), `dist/` output |

Considered and rejected: generating `plugin.toml` from
`[package.metadata]` in `Cargo.toml`. It would remove the hand-written file for
Rust plugins only; JavaScript plugins have no Cargo manifest, so there would be
two sources of `plugin.toml` instead of one. Also rejected: a `build.rs` to
copy the version into the descriptor; the repository has none on purpose.

### 7.4 Consistency between components

* **Registry, installer, release tool, market plugin** read and write the same
  Rust types; none has a private parser. The `registry` crate owns TOML
  (`entry`, `releases`, `sources`, `trusted-keys`) and JSON (`index`, `host`,
  `market`, `install-state`) readers and writers: `toml` (parse-only, as in
  `core`) for reading, and appending a block is plain text, so no TOML writer
  or editor dependency is added.
* **Plugin and loader:** `plugin.toml` stays parsed by `core` (parse-only
  `toml`) and by `registry`; both must agree on defaults. One shared table of
  field names and defaults in the spec, one test fixture directory of
  manifests that both parsers are run against.
* **CI** calls the tools; it never parses these files in YAML or shell
  (`Select-String` on `Cargo.toml` in the release workflow is replaced by a
  `parsec-orbit-release version` command).
* **Docs and specs** show examples in the format of the file they describe,
  and the examples are tests (7.1).

## 8. Migration

No release is cut between phases: `main` carries the old scripts and the new
tools side by side until the cutover, so every commit builds and every test
passes. 0.2.0 is tagged after phase 7. Each phase is a set of atomic commits;
docs and specs change in the same commit as behaviour; `cargo fmt`,
`cargo test --workspace --locked`, the pnpm tests and
`cargo check --workspace --all-targets` on macOS pass at every step. Work runs
in an isolated worktree with the target-clone policy of
[workflow.md](../development/workflow.md#worktrees-and-resource-reuse).

| # | Phase | Contents | Verification |
| --- | --- | --- | --- |
| 1 | Classify | Merge `sdk-std` into `sdk` + `sdk-testing`, `devtools` into the bridge, the three `verify-*` into one package ([2.5](#25-when-a-crate-is-split)); add `libs/`, `plugins/`, `tools/`, `research/`; `git mv` and rename packages (`-plugin-{config,hooks,js-runtime,webview-bridge,ui}` to `-lib-*`, `-loader` to `-loader-windows`); `role` in manifests; update members, path deps, scripts, CI, docs; dependency-law check; single-source versions and identity ([7.3](#73-one-source-per-fact)): drop `version` from source `plugin.toml`, take it from `CARGO_PKG_VERSION` in `declare_plugin!` if the macro allows it (else a test that fails on a mismatch), one shared manifest-vs-descriptor test helper in `sdk-testing`. | Full test suite; `package.ps1` still produces working zips; output `bcrypt.dll` unchanged; plugin ids and ABI unchanged. |
| 2 | Layout and core | `parsec-orbit-layout`; both loader `Config`s built on it; `format_line` and `Session` in `core`; unit tests moved. | Environment variables and path precedence identical (tests assert it). Re-measure `bcrypt.dll` and record it in the size report (the `toml` parser is not touched). |
| 3 | Registry and release tool | Rename `market` lib to `registry`; open platform id; align default `kind`; additive index fields, `serial` check, `yanked`, mirrors; `host.json`; `parsec-orbit-release` with `package`, `registry add/check/build`, `keygen`, `sign`, `verify`; create `manifest/` with an entry for every official module; drop `plugin.macos.toml` (package writes `kind`/`entry` and `version`); JSON conventions of [7.1](#71-rule) in the `registry` types; `trusted-keys.toml` reader with one-time conversion of `trusted-keys.txt`; `registry.yml` in check-only mode. Old scripts untouched. | Generated `dist/index.json` installs through the existing market CLI; `registry check` passes on the committed `manifest/`. |
| 4 | Installer | `parsec-orbit-installer` with `common/`, `os/macos` (moved from `installer-macos`, tests included), `os/windows` (ported from PowerShell); `get-orbit.*`; migration of legacy plugin folders; `install-state.json` format 2 (reader keeps format 1); `market.json` gets `format`; reads `manifest/` sources and `--from dist`. | Windows adapter against a fake client dir and a service double; macOS against the scratch bundle; a manual run on a real Windows machine recorded. |
| 5 | Market plugin | `docs/spec/registry.md`; `plugins/parsec-orbit-plugin-market`; `sources.toml`; `servers.toml` for stun-swap with the old file as fallback; the section on `parsec-orbit.ui`; `core::staging::promote` as its own commit with its size measurement. | Plugin tests with a `Fetch` double and `MockHost`; promote tests on temp dirs. |
| 6 | `no_std` conversion | `high-bitrate-webview` to `no_std` (`cdylib` only, `verify-webview-inject` reads the asset); then measure and decide `high-bitrate-native`; guide updated. | Size recorded in the size report; e2e suite loads the converted plugin. Independent of phases 3 to 5. |
| 7 | Cutover (0.2.0) | Release workflow publishes through `parsec-orbit-release`; enable the signing secret and bot commits; delete `install.ps1`, `orbit-manager*`, `set-web-ui.ps1`, `package*.{ps1,py}` internals, `install-macos.sh`, `console-macos.*` and their tests; move research scripts; remove `parsec-orbit-market` from the release; replace the `Select-String` version reads in the workflows with `parsec-orbit-release version`; update `AGENTS.md` §8 and the guides below; mark ADR 0006 superseded; README install section. | Fresh install and in-place upgrade from a 0.1.x install on both OSes before tagging. |

Phases 4, 5 and 6 depend only on 3 (6 only on 1) and may run in parallel.
Phase 1 is the largest diff and the lowest risk (moves and renames); it goes
first so every later change lands in the final layout.

### Compatibility summary

Kept (user data and installed state): plugin ids, ABI 1, `plugin.toml` fields,
the `plugins/<id>/` folder layout, `index.json` format 1 and the ed25519 trust
model, the contents of `trusted-keys.txt` (read once and rewritten as
`trusted-keys.toml`, [section 7](#7-formats-and-single-sources)), the data of
`install-state.json` (read; rewritten as format 2), the macOS state and journal,
all `PARSEC_ORBIT_*` variables, the `bcrypt.dll` name and the macOS dylib name
the installer writes into the stub.

Changed at 0.2.0, no shim: release assets (no `release.json`, no
`release-macos.json`, no `index.json` asset, no `parsec-orbit-market`
executable), the installer entry point (scripts to `orbit`), DLL file names of
renamed crates (hidden behind `entry`), the update path of a loaded module
(staged and promoted at next start). Users who kept a 0.1.x `orbit-manager`
script re-bootstrap with `get-orbit`; the release notes say so.

## 9. Records to write

| ADR | Title | Supersedes |
| --- | --- | --- |
| 0017 | Crate roles and directory layout (`lib`, `plugin`, `tool`, dependency law) | none |
| 0018 | OS ports, `layout` crate and per-plugin backends | none |
| 0019 | The market is a plugin over the `registry` library | marketplace spec sections on the CLI |
| 0020 | The installer and release tooling are Rust | 0006; `AGENTS.md` §8 |
| 0021 | The official registry is `manifest/` on `main`, signed by CI | marketplace spec "Publishing" |
| 0022 | Pending-update promotion in `core` | none |
| 0023 | One SDK package, no features; `sdk-std` dissolved into `sdk` + `sdk-testing`; `no_std` plugins by measurement; crate-split criteria | amends 0002 |
| 0024 | File formats: TOML for edited files, JSON for exchanged ones, one source per fact | none |

Documents touched when each lands:

* `AGENTS.md` §8, rewritten:
  *"Product automation (install, upgrade, packaging, registry generation and
  signing) is Rust, in `tools/parsec-orbit-installer` and
  `tools/parsec-orbit-release`; logic and security-sensitive code never lives
  in shell or PowerShell. `scripts/` holds thin PowerShell entry points
  (`kebab-case.ps1`, usage header, `[CmdletBinding()] param(...)` first,
  `$ErrorActionPreference = 'Stop'`, `-LiteralPath`) that build and delegate;
  CI calls the same scripts a developer runs. The bootstrap scripts shipped to
  users (`get-orbit.ps1`, `get-orbit.sh`) only download, check
  `SHA256SUMS.txt` and run `orbit`. One-off research scripts live in
  `research/` and are not product automation."*
  §5 gains the dependency law by reference; §0 sync rule applies.
* New `docs/development/formats.md` (the rule, conventions and inventory of
  [section 7](#7-formats-and-single-sources)), routed from
  `docs/development/README.md` and restated in `AGENTS.md` in one line.
* `docs/development/workflow.md` ("Repository scripts"), `rust.md` (error
  types: `market` becomes `registry`; crate groups), `testing.md` (doubles:
  `Fetch`, scratch Orbit root).
* Specs: `plugin-manifest.md` (`role`), `marketplace.md` replaced by
  `registry.md`, `services.md` (market stays reserved), `macos-platform.md`
  section 9, `loading.md` (promote step), `rust-sdk.md` (`no_std` plugin
  recipe, `os` module if added).
* Design and guides: `architecture.md` crate graph, `network-installer.md`
  superseded, `size-report.md`, `rust-plugin.md`, README layout and install.

## 10. Decisions

Settled by the owner:

1. **Registry:** the official registry is `manifest/` on `main`; layout, data
   format and fetch/update are designed in [4.3](#43-the-official-registry-manifest-on-main).
2. **Core:** `staging::promote` is added ([4.5](#45-installing-from-inside-a-running-client)).
3. **Installer:** PowerShell to Rust; `AGENTS.md` §8 is updated
   ([section 9](#9-records-to-write)).
4. **Compatibility:** direct move to the new architecture at 0.2.0, no
   compatibility window.
5. **Naming:** `lib` stays.
6. **SDK:** a `std` feature was evaluated and is not viable ([6.7](#67-the-sdk-as-one-package-adr-0002-re-evaluated)); the SDK becomes one package without features and `sdk-std` is dissolved. Other splits are reviewed by the criteria of [2.5](#25-when-a-crate-is-split).
7. **TOML:** the existing parser is kept and not touched, and is not replaced for size.
8. **Implementation** starts now: macOS-verifiable work first, Windows-only work left to a Windows session ([hand-off](restructure-handoff.md)).

Taken in this revision, easy to reverse:

* `role` is an explicit manifest field rather than derived from `provides`
  (`lib-ui` has a window; `stun-swap` may one day provide a service).
* The registry carries metadata only; packages stay on release assets or
  third-party HTTPS URLs. Binaries are not committed to git.
* `required` is derived from `requires`, not written by hand.
* Key lists and the stun server list move from private text formats to TOML;
  `install-state.json` moves to `snake_case` (format 2) with a reader for
  format 1. The macOS installer records keep their own versioned schema.
* `jsDelivr` appears as a mirror example because the signature makes any
  mirror safe; it is data in `sources.toml`, not a dependency.

Still open (no blocker for phases 1 to 3):

* The bot's commits to `main` need branch protection that lets it push, as the
  Publish workflow already does; confirm that stays acceptable.
* Whether the `high-bitrate-native` conversion goes ahead depends on a Windows
  measurement ([6.6](#66-decision)).
* The loader `no_std` spike runs only if the size budget is still missed after
  Phase 2.

Out of scope: Linux, removing process-global state from `core::host`,
changing the plugin ABI, unloading plugins.
