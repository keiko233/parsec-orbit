# Size report

## Restructure Windows checkpoint (2026-10-15)

Native `x86_64-pc-windows-msvc`, `rustc 1.100.0-nightly (a36d05efa
2026-09-09)`, workspace release profile and static CRT. The default loader
was rebuilt without `diagnostics` after forwarding checks.

| Artifact | Bytes |
| --- | ---: |
| loader `bcrypt.dll` | 400,896 |
| WebView patches (`no_std`) | 17,920 |
| UI service `parsec_orbit_lib_ui.dll` | 563,200 |
| WinUI renderer `parsec-orbit-ui-companion.exe` | 541,184 |
| market plugin | 1,968,640 |

The loader exceeds 360 KiB by 32,256 B and the previous console-enabled
381,952 B checkpoint by 18,944 B. This is the restructured loader with shared
layout/session/logging and pending-update promotion, not a controlled comparison
against the old toolchain; no precise per-change attribution is claimed.
The UI merge adds no loader dependency, and the TOML parser is unchanged.
Direct PE table inspection confirms the WebView DLL exports only the three
plugin ABI functions and imports only `kernel32.dll`.

Measured on `x86_64-pc-windows-msvc`, release profile (`opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`), static CRT where a CRT is linked at all. Sizes are bytes of the DLL. The [macOS arm64](#macos-arm64-m0) numbers below were taken with the same profile on `aarch64-apple-darwin`.

## Plugin runtime spike (phase 3)

The question of [ADR 0002](decisions/0002-sdk-is-no-std-by-default.md): how small can a plugin be, and is a C-runtime-free build practical? The same plugin (log one formatted line through the host) was built across the variants below.

| Variant | Size | Notes |
| --- | ---: | --- |
| `std` hello (phase 2 sample) | 210,432 | what the ADR called "~210 KB" |
| `no_std` + static CRT, one `log` call | 103,424 | the CRT start-up and its dependencies dominate |
| `no_std` + static CRT, `core::fmt` incl. floats | 129,024 | |
| `no_std`, `/NODEFAULTLIB`, one `log` call | 3,584 | compiler emits no `mem*` calls here |
| `no_std`, `/NODEFAULTLIB`, `core::fmt` incl. floats, `memcpy`/`memset`/`strlen` | 28,672 | float formatting is most of it |
| `hello-nostd` with the SDK (`freestanding!`), release | 10,752 | no imports at all, exports only the three ABI functions |
| `hello-nostd` with the SDK, debug | 33,280 | debug-info size depends on the build path |

**Result: the CRT-free variant is practical and becomes the default recipe.** What it takes is small and lives in `freestanding!`:

* the functions compiled code calls into: `memcpy`, `memmove`, `memset`, `memcmp`/`bcmp`, `strlen`, `__chkstk` (frames over a page), `_fltused`, a stub for the exception personality `__CxxFrameHandler3` (unoptimised builds still name it), and the DLL entry point `_DllMainCRTStartup`;
* all of that as `global_asm!`. As Rust `#[no_mangle]` functions they would be exported from the DLL, and the optimiser can turn the loops back into calls to themselves; assembly has neither problem;
* `/NODEFAULTLIB` as a linker directive in the object file (`.drectve`), so a plugin crate needs no build script or `RUSTFLAGS`;
* `kernel32` is reached through `raw-dylib` (no import library needed) and only when the plugin allocates.

Costs: the plugin crate must be `#![no_std]` and build with `panic = "abort"` in every profile it uses (the precompiled `core` otherwise needs the unwinding runtime); `x86_64` only. Plugins that need `std` keep working and pay what they paid before.

The end-to-end suite loads these DLLs into a real process and exercises the allocator (including over-aligned blocks and 1 MiB zeroed buffers), overlapping `memmove`, comparisons, a 200 KB stack frame, float formatting and the panic path.

## Official plugins (std)

| Plugin | Before the SDK API | After | Delta |
| --- | ---: | ---: | ---: |
| `hello` | 210,432 | 212,992 | +2,560 |
| `high-bitrate-native` | 226,304 | 232,448 | +6,144 |
| `high-bitrate-webview` | 270,848 | 273,920 | +3,072 |

The migration to `Plugin` / `Context` costs 2.5 to 6 KB each: the log macros, the service lookup and the plugin slot. They are `std` plugins on purpose (threads, sockets, file system); moving them to `no_std` would mean replacing `std::net` and `std::fs`, which is not worth it.

## no_std conversion

`high-bitrate-webview` has no `std` left: its one `std::env::var` is a small environment reader over `GetEnvironmentVariableW` (Windows) and `getenv` (macOS), and the crate builds as `cdylib` only. Release `.dylib` on `aarch64-apple-darwin`, same profile as above, bytes:

| Build | Size |
| --- | ---: |
| before (`std`, `cdylib` + `rlib`) | 302,208 |
| after (`no_std`, `cdylib`) | 50,416 |
| saved | 251,792 (83 %) |

The Windows size is not measured here (no Windows linker on this machine); the Windows build was only type-checked.

## Loader

| Point in time | `bcrypt.dll` |
| --- | ---: |
| baseline (phase 0) | 346,624 |
| phase 1 (core split) | 348,160 |
| phase 2 (host revision 2) | 368,128 |

Budget: 360 KiB = 368,640 bytes, so phase 2 left 512 bytes. The growth is the registry, the ordering, the kind-handler call and the manifest lists. The planner uses plain vectors because every collection type costs bytes here. If the loader needs room later, the lever is the `toml` parser: `toml` + `toml_parser` are about 36 KiB of the `.text`, and `plugin.toml` is flat enough for a hand-written reader.

## Phase 4 and 5 plugins (release, bytes)

| Artifact | Size | Notes |
| --- | ---: | --- |
| `webview-bridge` | 379,904 | `std`, `serde_json`, its `devtools` module (a crate of its own when measured) |
| `js-runtime` | 4,226,048 | Boa without `temporal`/`intl`; a separate download on purpose |
| `config` | 297,984 | `std`, `serde_json` |
| `hooks` | 385,536 | `std`, `retour` |
| `ui` | 529,408 | `std`, `winio` (Win32 backend); the companion window |
| `parsec-orbit-market.exe` | 1,847,296 | TLS (`rustls`), zip, ed25519 |
| loader `bcrypt.dll` | 374,272 | 5,632 bytes over the 360 KiB budget, see below |
| loader `bcrypt.dll` with the debug console | 381,952 | +7,680 (+2.1 %) over the row above |

Measured again on the release branch rather than on the phase branch these
numbers were first taken from: the DevTools client now lives in its own crate
and the webview bridge changed its thread model, so `webview-bridge` and
`js-runtime` moved by a few KB. `config`, `hooks` and the market tool are
identical.

**The debug console costs 7,680 bytes.** Measured as the difference between the
loader at the same commit with and without the feature (374,272 B → 381,952 B,
+2.1 %), both from `cargo build --release --locked`, the build
[package.ps1](../../scripts/package.ps1) ships. That is the `AllocConsole` /
`CreateFileW` / `CreateThread` imports, the `Follower`, and the two new `Config`
fields, plus the token query that tells the client's two instances apart; the
follower's thread is created with the raw `CreateThread` the plugin worker
already used, because `std::thread` would have cost about 20 KB on its own. The
switch is a run-time environment variable (`PARSEC_ORBIT_CONSOLE`), not a cargo
feature, so the one DLL that ships carries it.

**The loader is over its budget.** Phase 2 left 512 bytes of the 360 KiB
(368,640 B); the release build is 374,272 B and 381,952 B with the console,
13,312 B too much. Nothing in phase 5 links against the loader, so the growth
is the phase-3/4 work piling up in it: the service registry, the per-plugin
host tables and the extra host revision entries. The documented lever is the
`toml` parser below the `plugin.toml` reader (about 36 KiB of `.text`); until
it is pulled, the "stays under 360 KiB" line in
[architecture.md](architecture.md) is not met.

## The Orbit UI (release, bytes)

`ui` is the only plugin that links a GUI stack, and the largest one on this
branch outside the JavaScript runtime. Like `js-runtime` it is a separate
download: a client that does not install it pays none of those bytes, and the
loader does not link `winio` at all.

Wiring an existing plugin into that window costs a JSON parser, not a toolkit.
`high-bitrate-native` went 235,008 B -> 257,536 B (+22,528, +9.6 %) for its
`parsec-orbit.ui` section: `serde_json` to read the action events the window
sends back, the section schema, and the two live switches (`AtomicBool` and
`AtomicUsize`) the section writes. The window itself is drawn by the `ui`
plugin, which finds every row from the declarative schema, so a plugin that
registers a section without handling actions pays for the schema literal alone.

## macOS arm64 (M0)

Measured on `aarch64-apple-darwin` with the same release profile, `.dylib`
sizes in bytes. The Windows budget above is not a macOS budget: the macOS
loader is a plugin *host* loaded into Parsec by a patched-bundle load command,
not a proxy DLL, and M1 grows it with the installation gate, the isolation
shims and the readiness probe. The 360 KiB line still stands as the reference
until M1 settles what the injected dylib really needs, and the difference is
worth watching: the `harness` build is 62,128 B heavier than the Windows
`bcrypt.dll` (436,400 B vs 374,272 B) with the same profile, before any of the
M1 gate, shims or readiness code exists. Two caveats on the comparison: this
build links `std`/libSystem rather than the Windows no-CRT layout, and it is
not yet fully reachable, so dead-code stripping will move the number again
once the M1 bootstrap calls into the whole crate.

| Artifact | Size | Notes |
| --- | ---: | --- |
| loader `libparsec_orbit_loader.dylib` with `harness` | 436,400 | closest to the M1 injected build |
| loader `libparsec_orbit_loader.dylib` without `harness` | 16,720 | not a useful number yet: no constructor or discovery entry point is linked, so most of the crate is dead-stripped |
| `hello-nostd` (`freestanding!`) | 50,272 | libSystem heap, no CRT |
| fixture `no_std` (`.alloc`/`.stack` kinds) | 67,248 | test fixture |
| `hello` (`std`) | 285,472 | |
| fixture `plugin` (`std`) | 302,608 | test fixture |

The `no_std` jump from 10,752 B on Windows to 50,272 B here is Mach-O
section/LoadCommand overhead plus the Rust `core` flavour and the libSystem
imports (`malloc`, `free`, `realloc`, `posix_memalign`) the Windows
`raw-dylib` build keeps leaner. `core::fmt` is still the bulk of the code.

### M1a checkpoint (accepted 2026-10-14, not the completed loader)

The default release dylib with the dependency constructor, installation
binding, NSS interception, custom-SBPL isolation gate, shared-persistent
redirect, verified home arming, deferred runtime worker and the
`parsec-orbit.client` service is **571,504 B** on macOS 27 arm64
(`cbccb4f9...d5cee`; 386,528 B before those runtime additions were linked in,
369,104 B before the isolation profile and interposition table). It is
measured with the release profile above and no `harness` feature. Reaching
vendor main now links the previously dead-stripped core activation, so this
number is a checkpoint, not a final budget, and it is above the historical
360 KiB Windows reference. The new JSON/hash dependencies reuse the
workspace's existing serde/serde_json/sha2 versions; no second crypto
dependency stack was added.

### M1b checkpoint (2026-10-14, not the completed loader)

Moving the seed transaction out of the loader crate into the shared
`parsec-orbit-macos-state` rlib (so the transactional installer can seed
without linking the loader `cdylib`) rebuilt the same default release dylib at
**571,472 B**, SHA-256
`b278d7b8c4b68e720a03d45d54c7c55433e486addbf35e00d6a97ce4d377f5b0`
(32 B smaller than the M1a checkpoint; the loader re-exports `seed`/`Seed
Report` unchanged). The new installer binary `parsec-orbit-installer-macos`
is **505,680 B** in the same release profile; it is an ordinary CLI, not part
of the injected dylib, so it does not consume the loader budget.


### M1c/M2 independent checkpoint (2026-10-15)

Default release artifacts, macOS arm64, without `harness`. Sizes include the
linker's signature; applying/signing copies changes their hashes.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| loader `libparsec_orbit_loader.dylib` | 571,472 | `e56a6f798babad9203f3bd16af951447acff14af26d3c4a1cee79f30ba063ce1` |
| installer `parsec-orbit-installer-macos` | 539,328 | `172cf6d51cacd69c25e9078eca02d7866c9c367630e46b76ff52a82f5291b1cc` |
| hooks `libparsec_orbit_plugin_hooks.dylib` | 336,688 | `ada192290452e2534240705a9176cfb3446c71a6e4f47212a29373debe393161` |

The loader remains the same size as M1b after sharing the client ABI and
accepting bounded numeric release tags. The installer grew by 33,648 B for
universal parsing, full metadata, recovery/locking and assessment. Hooks are
a separate optional plugin, not added to the injected loader. The historical
360 KiB reference is still exceeded by the 571,472 B loader; no final macOS
budget or size optimization is claimed here.

## Independent UI companion (Windows x64 and macOS arm64)

The service plugin and separate winio renderer sizes, build profile, artifact
digests and validation limits are recorded in the
[UI companion checkpoint](ui-companion-checkpoint.md#release-profile-sizes), with
the native Windows build in its
[Windows follow-up](ui-companion-checkpoint.md#windows-follow-up-2026-10-15).
GUI dependencies belong only to the helper; the loader and core are unchanged.
