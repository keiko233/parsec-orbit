# macOS platform rules

Status: **the M0 platform layer is implemented** (`crates/parsec-orbit-loader-macos`: `dlopen` platform, append logger, config paths; the SDK `freestanding!` flavour; std/`no_std` fixtures loaded in a non-Parsec host). The launch bootstrap, installation, signing gates and services are M1/M2 and still draft. The host, the loader dylib, state isolation and installation are in [macos-host.md](macos-host.md); this document says where files live, what a macOS plugin is, and how each service and official plugin behaves on macOS. Evidence tags (`[measured]`, `[static]`, `[unverified]`) are defined in [macos-host.md](macos-host.md#0-conventions). Decisions: [ADR 0008](../design/decisions/0008-macos-dylib-kind.md) (`dylib` kind), [ADR 0009](../design/decisions/0009-macos-hooks-interpose.md) (hooks), [ADR 0010](../design/decisions/0010-macos-webview-backend.md) (WebView).

## 1. Targets

| Item | macOS value |
| --- | --- |
| OS / architecture | macOS on `aarch64` only; Rust target `aarch64-apple-darwin` |
| Plugin ABI | unchanged: `ABI_VERSION 1`, host revision 2; the 64-bit sizes apply (`HOST_REVISION_1_SIZE` 96, `HOST_REVISION_2_SIZE` 208) |
| Native kind | `dylib` (a Mach-O dynamic library); `dll` is the Windows kind |
| Calling convention | `extern "system"` is the platform C ABI on macOS, so every ABI, service and kind-handler function keeps the signature of [plugin-abi.md](plugin-abi.md) `[static]` |
| Client mode / hosting | client mode only; hosting is out of scope ([ADR 0011](../design/decisions/0011-macos-scope.md)) |

## 2. Paths

`<home>` below is the **real** home directory of the user, resolved by the loader before its own `getpwuid` interposition is active. The client and its bootstrap see the isolated home instead ([macos-host.md](macos-host.md#7-state-isolation)); plugins that call `getpwuid` or read `$HOME` see whatever the host process has, so a plugin must take paths from the host table (`plugin_dir`, `data_dir`) and not derive them.

| Item | Path |
| --- | --- |
| Orbit root | `<home>/Library/Application Support/ParsecOrbit`; overridden by `PARSEC_ORBIT_HOME` (used by tests) |
| Plugin directories, in priority order | `PARSEC_ORBIT_PLUGIN_DIR` (**`:`-separated**), then `<Orbit root>/plugins` |
| `data_dir` of a plugin | `<Orbit root>/data/<id>` (the host table's `data_dir`; may not exist yet) |
| Log file, first usable | `PARSEC_ORBIT_LOG`, `<Orbit root>/parsec-orbit.log`, `$TMPDIR/parsec-orbit.log` |
| Config service storage | `<Orbit root>/data/com.parsec-orbit.config/<owner>.json` |
| Market | `<Orbit root>/market/` (`trusted-keys.txt`, `--keys` overrides) |
| Client state (owned by the client) | `<Orbit root>/home/.parsec` |
| Shared-persistent replacement | `<Orbit root>/home/.parsec-persistent` (full API coverage remains a gate) |

* There is no `<client dir>/plugins` on macOS (the patched app is not a place for user data; plugin updates must not invalidate its newly established seal).
* Duplicate directories are dropped by comparing the **canonicalised** path case-sensitively, the first spelling wins. (APFS is case-insensitive by default; two spellings of one directory are canonicalised to the same path by the filesystem API.)
* `PARSEC_ORBIT_DEVTOOLS` and `PARSEC_ORBIT_DEVTOOLS_PORT` have no port meaning on macOS (see WebView); `PARSEC_ORBIT_HOST` does not exist.

## 3. Plugin libraries

* A native plugin is a directory with `plugin.toml` with `kind = "dylib"` and an `entry` that is a `.dylib` file. The loader maps it with `dlopen(path, RTLD_NOW | RTLD_LOCAL)` and resolves `ParsecOrbitPluginQuery`, `ParsecOrbitPluginInit`, `ParsecOrbitPluginShutdown` and the compatibility export `ParsecPluginInit` with `dlsym`; a failure is reported with the `dlerror` text: `could not load <path> (<dlerror text>)`.
* The exports are plain C symbols; on Mach-O the compiler prefixes `_` and `dlsym` takes the name without it. The library must export them (for Rust: `#[no_mangle]`, which `declare_plugin!` emits, in a `cdylib`).
* **Loose libraries are not supported.** A `.dylib` without a manifest is not a plugin (the Windows "loose `*.dll`" layout is a legacy of the first prototype and is not carried to macOS).
* **Signing.** Apple silicon refuses code without a signature. Every `.dylib` a plugin ships must carry at least an ad-hoc signature at the moment it is loaded. Rust and Apple's linker produce a linker-signed (ad-hoc) binary by default; anything that rewrites or strips the file must re-sign it. The market tool and the installer sign (section 9). The host enforces no team or identity ([macos-host.md](macos-host.md#8-signing-and-entitlements)). An ad-hoc SDK fixture loaded in the HR + disable-LV copy `[measured]` ([PoC](../design/macos/native-dependency-poc.md)); since M0 the formal loader also maps the std and `no_std` fixtures in a non-Parsec host `[measured]` (`crates/parsec-orbit-loader-macos/tests/fixture_e2e.rs`). Loading inside the hardened bundle is repeated in M1a; unsigned/corrupt rejection remains a gate (E6). The HR exception is application-wide, not a plugin trust guarantee.
* **Architecture.** The library must contain an `arm64` slice.
* Keep `kind = "dylib"` out of Windows packages and `kind = "dll"` out of macOS ones: the two are different artifacts (section 9).

The platform-native kinds are defined in [plugin-manifest.md](plugin-manifest.md) and [loading.md](loading.md#4-per-plugin-load).

## 4. `Platform` and `Logger`

`parsec-orbit-core` stays platform-free; the macOS loader supplies two implementations in `crates/parsec-orbit-loader-macos` (`src/platform.rs`, `src/log.rs`).

* **`Platform::load(path)`**: the `dlopen`/`dlsym` behaviour of section 3, returning `Module { handle, exports }` with `handle` the `dlopen` handle as `usize`. Never `RTLD_GLOBAL`; never `dlclose` (there is no unload). `Platform::native_kind` returns `dylib`.
* **`Logger`**: appends UTF-8 lines to the log file (`O_APPEND`, one `write` per line) and mirrors them to stderr in debug builds. A line carries the same format as the Windows log. The process name in a line is the base name of the main executable (`parsecd`).

## 5. Rust SDK on macOS

`parsec-orbit-plugin-abi` and `-sdk-std` build unchanged. `parsec-orbit-plugin-sdk`'s `freestanding!` has a macOS flavour since M0, so a `no_std` plugin builds for `aarch64-apple-darwin`:

* the `#[global_allocator]` is over `libSystem` (`malloc`, `calloc`, `free`, `realloc`, and `posix_memalign` for over-aligned blocks), declared with `extern "C"` in a `#[link(name = "System")]` block (`no_std` builds pass `-nodefaultlibs`, so the link name is explicit);
* the `#[panic_handler]` logs through the host and then terminates the process with a trap (`brk`/`udf`), the macOS equivalent of the Windows fast-fail;
* the C runtime functions (`memcpy`, `memmove`, `memset`, `memcmp`, `bcmp`, `strlen`) come from `libSystem`; the MSVC-specific items (`__chkstk`, `_fltused`, `__CxxFrameHandler3`, `_DllMainCRTStartup`, `/NODEFAULTLIB`) are not emitted; a no-op `rust_eh_personality` is exported because `liballoc` references it even without unwinding;
* `panic = "abort"` is required in every profile, as on Windows.

The M0 fixture run maps both fixtures through the loader and exercises the allocator with over-aligned blocks, the copy/reset paths and a 200 KB stack probe `[measured]` (`fixture-nostd.alloc`/`.stack`). The compare, float and panic actions of the fixture run in the Windows e2e suite and are not yet measured on macOS. Consequence: a macOS `no_std` plugin imports `libSystem` and is not "import-free" as the Windows sample is; its size is recorded in the size report (M0c). The `std` plugins need no change.

## 6. Services and official plugins

Per-service macOS behaviour. A service that a platform does not provide is absent from the registry (`query_interface` returns null), the plugin that would provide it is skipped, and its dependents are skipped with the usual message.

### Service matrix

| Service / plugin | macOS status | Where |
| --- | --- | --- |
| `parsec-orbit.kind.dylib` | not used; `dylib` is handled by the loader itself | section 3 |
| `parsec-orbit.config` (`config` plugin) | **measured** config set/get in a guarded disposable client; paths in section 2 | |
| `parsec-orbit.js`, `kind.js` (`js-runtime`) | **measured** Boa evaluation in a guarded disposable client | no JIT entitlement added |
| `parsec-orbit.client` | **provided by the loader**, shared ABI and SDK binding; current status always `not-verified` | below |
| marketplace (`parsec-orbit-market`) | **implemented and tested** | section 9 |
| `parsec-orbit.webview` (`webview-bridge`) | **partial**, M2 | below |
| `parsec-orbit.ui` (`parsec-orbit-plugin-ui`) | Programmatic show/hide through an adjacent `parsec-orbit-ui-companion` helper; release packaging deferred | [UI service](ui-service.md) |
| `high-bitrate-webview` | **blocked** until the WebView URL and DOM are known, M2 | below |
| `parsec-orbit.hooks` (`hooks`) | **implemented and measured** for bounded, already-bound classic imports, M2 | below |
| `high-bitrate-native` | **unavailable**; static candidates only, M2 | below |
| hosting-related behaviour | out of scope | [ADR 0011](../design/decisions/0011-macos-scope.md) |

The UI plugin keeps the service ABI, schemas, callback pointers and authoritative
registry in the client. It launches the adjacent `parsec-orbit-ui-companion`
helper for rendering. The helper owns the AppKit main thread and `winio` loop;
the processes communicate over private inherited stdin/stdout pipes using the
version 1 bounded JSON-line protocol described in [ui-service.md](ui-service.md).
Programmatic `show()` and `hide()` are the initial activation contract.
External CLI and hotkey activation are not implemented unless the renderer adds
them. Development packaging can build and stage the dylib, manifest and helper
together; public release artifact allowlists remain unchanged, and macOS helper
code signing and runtime-state sandbox compatibility remain unverified pending
parent-process validation.

### `parsec-orbit.client`

Provided by the loader on macOS and registered before any plugin runs, version 1.0. It tells a plugin which client it is running in so that plugins that depend on client internals can refuse an unknown build ([macos-host.md](macos-host.md#9-client-versions-and-compatibility)).

```c
/* parsec-orbit.client, version 1.0 */
typedef struct ParsecOrbitClientV1 {
    ParsecOrbitInterfaceHeader header;
    /* Passes a JSON object to `out` on the caller's thread before returning:
       {"platform":"macos","arch":"arm64","build":"150-104a",
        "dylib_sha256":"<64 hex>","loader_token":14,"status":"not-verified"}
       status is "verified", "not-verified" or "incompatible". */
    int32_t (*info)(ParsecOrbitMessageCallback out, void *user);
} ParsecOrbitClientV1;
```

`info` returns `0`, or `-4` when the build could not be detected (the status is then `not-verified` for every plugin). It is thread-safe and the JSON is valid only during the call. `parsec-orbit.client` is reserved like the other `parsec-orbit.*` names. Whether it is added to the Windows loader (a client version table exists there in practice) is open.

### `parsec-orbit.webview` and the WebView bridge

[ADR 0010](../design/decisions/0010-macos-webview-backend.md): the service keeps its name, header and every function of [services.md](services.md#parsec-orbitwebview-provided-by-the-webview-bridge-plugin-version-10); only the backend changes. On Windows the backend is the DevTools protocol over a port; WKWebView has no such port `[static]`, so on macOS the backend runs **inside the host process**:

* The bridge obtains the client's `WKWebView` through the Objective-C runtime (observing the creation of web views and their `WKUserContentController`). The 2026-10-15 signed-out window probe found one native Matoya view and no WKWebView; signed-in client surfaces remain `[unverified]`: the client has the code (`evaluateJavaScript:completionHandler:`, `loadRequest:`, `userContentController:didReceiveScriptMessage:`) and rollout flags (`features_webview`, `client_web_ui`) `[static]`; no navigation target URL was found `[static]`. `is_ready()` is `1` only once a web view is attached.
* `add_script` maps to `-[WKUserContentController addUserScript:]` (the client itself installs no user scripts `[static]`, so keys are all ours) plus an immediate evaluation in the current document; `evaluate` to `-[WKWebView evaluateJavaScript:completionHandler:]`; `subscribe`/`post` to a `WKScriptMessageHandler` registered under a fixed name and to `window.__orbit` dispatch scripts.
* WebKit objects are used on the main thread. `evaluate` and `post` called from the main thread return `-4` (not ready) instead of blocking; from any other thread they behave as specified (blocking up to the stated timeouts). The delivery thread rules of the service are unchanged.
* `window.__orbit.rpc` (the Parsec UI/client envelope on `window.MTY_NativeSendText`) is `[unverified]` on macOS; until the page is known the helper reports it as unavailable and rejects.
* **Inspector aid (best effort).** With `PARSEC_ORBIT_DEVTOOLS=1` the bridge sets the observed web view `inspectable`, so Safari's Develop menu can attach (macOS 13.3 and later). No TCP port is opened and `PARSEC_ORBIT_DEVTOOLS_PORT` is ignored. This is a development aid, not part of the service contract.

### `parsec-orbit.hooks` and the hooks plugin

[ADR 0009](../design/decisions/0009-macos-hooks-interpose.md): the macOS plugin provides the interface by **rebinding imported symbols**, not by patching function bytes. `mprotect` on `__TEXT` is refused on arm64 even without hardening `[measured]`, and `retour 0.3` has no aarch64 backend `[measured]` (the repository pins it; the crate's own x86-only gate is verified in the published source).

Semantics that differ from the Windows text:

* `resolve(module, symbol)` returns the address from `dlsym` against the images loaded in the process; `module` is an image file name or empty for all images. The client dylib is stripped and exports only `wx_main` `[measured]`/`[static]`, so a plugin can resolve system and library symbols, not client internals.
* `install(target, detour, &original)` accepts a `target` that is the address of an **imported function** (the address a symbol stub resolves to). It rebinds eligible classic lazy/non-lazy import slots whose current value exactly equals that address. Supported images loaded later receive the same bound-pointer check. Unresolved lazy stubs and chained-fixup images are left untouched; no generic late-lazy coverage is claimed. `*original` receives `target` itself, which stays callable because the function body is not modified. Calls that do not go through an import binding (direct calls inside one image, calls made before the hook was enabled through a cached address) are not intercepted. Any other `target` is rejected with `-4` (`ERR_NOT_READY`, "not supported by this loader"); `-2` if `target` is already hooked.
* `enable`, `disable` and `remove` set and restore the pointers; `*original` remains valid after `remove`.
* Import data-page writes and exact protection restoration passed in an owned Hardened Runtime scratch host and for an owned imported function inside disposable 101a/104a/105c client processes `[measured]`. This does not establish hooks on vendor internals. See the [client checkpoint](../design/macos/client-checkpoint.md).
* Inline hooks, and with them hooks on the client's own functions, are not provided in this phase; a plugin that needs them cannot be supported on macOS yet.

### `high-bitrate-native`

The bounded read-only inspector finds twelve `3..50 Mbps` records with stride
`0x144` and numeric value at `+0x40`, plus two adjacent arm64 ADRP/ADD candidate
references, in each inspected 104a/105c image `[static]`. The table candidate
is in `__TEXT,__const`, not ordinary writable data. Runtime menu/use-site
semantics and a verified-build patch policy remain `[unverified]`; no macOS
manifest or mutation backend is shipped. All detected builds still report
`not-verified`.

An owned signed-HR data canary measured `vm_protect(READ|WRITE|COPY)` and data
write/restoration on a dedicated `__TEXT,__const` page, while plain write
permission was rejected `[measured, owned synthetic host only]`. A separate
cold-function-page experiment ended with SIGBUS during the protection attempt,
before any write or returned result. Neither experiment establishes safe
vendor table mutation or execution of modified signed code. See the
[client checkpoint](../design/macos/client-checkpoint.md).

### `high-bitrate-webview`

Needs the macOS WebView target URL and the DOM of the bandwidth control, both `[unverified]`. Until they are known the plugin is not offered for macOS. Its injected script is page-realm code and unchanged in nature.

## 7. Differences in manifest and loading behaviour

See [plugin-manifest.md](plugin-manifest.md) (the `kind` default and the platform-native kinds), [loading.md](loading.md) (discovery per platform and the skip messages), [plugin-abi.md](plugin-abi.md) (lifecycle wording). Reserved device names are still rejected on macOS: ids and package paths stay portable.

## 8. Known contract issue

The registry rejects a table whose address is not aligned to `align_of::<usize>()` ([services.md](services.md#registry)). The test header in `parsec-orbit-core` is a 4-byte-aligned static, so five core tests fail on macOS when the linker places it at an address `% 8 == 4` `[measured]` (an earlier inventory attributes the failure to the header size; the header is two `u32`, 8 bytes, 4-byte aligned, so the alignment explanation holds). The contract stays: real service tables hold function pointers and are 8-byte aligned.

The fix is in M0 and applies to the test: the affected fixtures
(`parsec-orbit-core` tests, the `plugin-abi` test header, the
`parsec-orbit-fixture-plugin` interface) wrap their tables in
`#[repr(align(8))]` statics, and `cargo test -p parsec-orbit-core` passes on
`aarch64-apple-darwin` `[measured]`. The registry contract is unchanged.

## 9. Marketplace on macOS

**Status: implemented (`37dc0c7`).** Releases have per-version platform filtering, macOS Orbit paths and staged ad-hoc signing. Existing indexes without a platform retain their Windows meaning. The macOS HTTP integration test installs a real arm64 dylib from an Ed25519-signed loopback index and strictly verifies its installed signature.

[marketplace.md](marketplace.md) gains one optional index field, `platform`, per version: `"windows-x86_64"` (the default when absent, so existing indexes keep their meaning) or `"macos-arm64"`. The tool offers only versions whose platform equals the machine's.

* A macOS package is the same zip with one folder; its `plugin.toml` has `kind = "dylib"` and its `entry` is a `.dylib`.
* After the checks and before the swap-in, the tool **checks every staged `.dylib` for an arm64 slice, then signs it ad-hoc** (`codesign --force --sign -`) and verifies it; a signing failure aborts the install and leaves the old version in place. The sha256 and size in the index are those of the package as published; the installed file is signed and so differs in bytes. `market.json` records the package digest.
* Paths: plugins in `<Orbit root>/plugins/<id>`, keys in `<Orbit root>/market/trusted-keys.txt`.
* A plugin that the running client holds mapped can be replaced on macOS (the old file is unlinked; the running process keeps its inode), but the new version loads at the next start; the tool does not refuse and says so.

## 10. Release installation and terminal diagnostics

The independent terminal viewer follows `parsec-orbit.log` and the isolated
`home/.parsec/log.txt` under the selected Orbit root. It must not read the real
user's Parsec state, launch/stop the client or insert a UI into it. Log reads
must reject symlinks, foreign ownership, non-regular files and multiple links;
backlog and per-poll reads are bounded. See the
[console guide](../guides/macos-console.md).

The macOS network manager consumes a separate `release-macos.json` with
`format = 1`, `platform = "macos-arm64"`, the release `version` and two artifact
descriptors: `verifier` and `bundle`. Each descriptor specifies `url`, lowercase
SHA-256 and byte `size`. The detached signature uses the existing marketplace
Ed25519 format. The default trust anchor matches the Windows manager's official
public key; a selected `--keys` file replaces it. A missing or invalid signature
must fail unless the user explicitly selects `--allow-unsigned`.

The first verifier download is bootstrapped through HTTPS and the declared
digest; this weaker initial trust boundary must be stated. A previously verified
cached verifier checks later release manifests. All network downloads and zip
extraction are bounded, remote artifact URLs cannot reference local files, and
archive paths must not escape staging. Only verified staged artifacts may be
passed to the existing ordinary-user installation wrapper. Protected bundle
operations retain the installer privilege and restore rules; no network download
runs as root. `uninstall` restores the original app and keeps Orbit user data.
See [distribution design](../design/macos-distribution.md) and the
[network guide](../guides/macos-network-install.md).

## 11. Open questions

* **E6**, generic import/chained-fixup coverage beyond the measured classic backend, the WebView questions (client creates a web view in client mode, URL, DOM, `MTY_NativeSendText` on macOS), the `high-bitrate-native` table layout, the size of the `freestanding!` macOS plugin and the loader dylib budget: see [the phase plan](../design/macos-phases.md#evidence-status-and-remaining-questions).
* Whether `parsec-orbit.client` should be provided on Windows as well.
* Whether manifests should be able to declare per-platform entries so that one package serves both platforms (`entry.macos = ...`); today a package is per platform.
* Whether `dlopen` with `RTLD_LOCAL` is enough for plugins that share a dependency on a second library next to them (`@loader_path` handling).

The current host retains Hardened Runtime and local ad-hoc signatures ([ADR 0014](../design/decisions/0014-macos-patched-bundle.md)). Config set/get and Boa evaluation passed in a disposable official client without additional entitlements. Account, native UI and WebView evidence still constrain support; older plain-host executable-memory measurements do not justify extra exceptions. Package config and JS runtime with their `plugin.macos.toml` installed as `plugin.toml`; keep Windows manifests separate.
