# Size report

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
| `parsec-orbit-market.exe` | 1,847,296 | TLS (`rustls`), zip, ed25519 |
| loader `bcrypt.dll` | 374,272 | 5,632 bytes over the 360 KiB budget, see below |

Measured again on the release branch rather than on the phase branch these
numbers were first taken from: the DevTools client now lives in its own crate
and the webview bridge changed its thread model, so `webview-bridge` and
`js-runtime` moved by a few KB. `config`, `hooks` and the market tool are
identical.

**The loader is over its budget.** Phase 2 left 512 bytes of the 360 KiB
(368,640 B); the release build is 374,272 B, 5,632 B too much. Nothing in
phase 5 links against the loader, so the growth is the phase-3/4 work piling
up in it: the service registry, the per-plugin host tables and the extra host
revision entries. The documented lever is the `toml` parser below the
`plugin.toml` reader (about 36 KiB of `.text`); until it is pulled, the
"stays under 360 KiB" line in [architecture.md](architecture.md) is not met.

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
