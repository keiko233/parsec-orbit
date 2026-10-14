# Size report

Measured on `x86_64-pc-windows-msvc`, release profile (`opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`), static CRT where a CRT is linked at all. Sizes are bytes of the DLL.

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
| `webview-bridge` | 379,904 | `std`, `serde_json`, `parsec-orbit-devtools` |
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
