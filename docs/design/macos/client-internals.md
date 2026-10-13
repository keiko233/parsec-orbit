# macOS client internals (static analysis)

Static analysis of the official macOS client, Parsec 150.101.1 (`tv.parsec.www`,
universal x86_64+arm64, `LSBackgroundOnly=true`); nothing was executed or
installed. The raw `strings` dumps (the 266 KB stub and the 6.2 MB client
dylib) and derived disassembly live in the local research archive; the commands
below produced them with `otool`/`nm`/`dyld_info`/`plutil`/`codesign`/`shasum`/
thin-slice `strings -t x`. Web claims cite in-binary URL strings rather than
browsed pages.

## 0. Artifact inventory (verified)

- `Contents/MacOS/parsecd` (266000 B, universal, hardened runtime
  `flags=0x10000(runtime)`, Team `Y9MY52XZDB`, Developer ID Application:
  Parsec Cloud, Inc., Runtime 15.5.0). Entitlements ONLY: application-identifier,
  persistent-content-capture=true, team-identifier, device.audio-input, device.camera,
  device.microphone. Missing: `allow-dyld-environment-variables`,
  `disable-library-validation`, `get-task-allow`.
  `codesign -dvv` + `codesign -d --entitlements -` confirm.
- `Contents/Resources/skel/appdata.json` (154 B):
  `{"entry_symbol": "wx_main", "hash": "9cce8505…c596cdd5", "so_name": "parsecd-150-101a.dylib"}`
  `shasum -a 256` of the bundled dylib == `appdata.json` "hash" exactly.
- `Contents/Resources/skel/parsecd-150-101a.dylib` (6235664 B, universal).
  Separately signed (`Identifier=parsecd-150-101a`, same team, hardened runtime).
- `Contents/Info.plist`: `CFBundleExecutable=parsecd`, `LSBackgroundOnly=true`,
  `LSMinimumSystemVersion=10.9.0`, URL scheme `parsec`, `NSSupportsSuddenTermination=false`.
- No `.html`/`.js`/WebView bundle resources: `Resources/` holds only `Parsec.icns` + `skel/`.
- pkg `Scripts/preinstall` (sh): `pkill Parsec; pkill parsecd; sleep 2;`
  `rm -rf /Applications/Parsec.app "$HOME/.parsec" /Users/Shared/.parsec`
  `/Library/LaunchAgents/com.parsec.app.plist`.

## 1. Startup chain

**1a. Launch/keepalive: NO launchd/SMAppService/XPC evidence in either binary.**
`rg -i "LaunchAgent|com.parsec.app.plist|SMAppService|login.?item|launchctl|SMLoginItem|XPC|ServiceManagement|StartAtLogin|KeepAlive|RunAtLoad"`
against both dumps: zero hits. Fresh `nm -arch x86_64` symbol counts for
`launch|login|SMApp|XPC` in both binaries: 0. `dyld_info -imports` on the dylib shows
no ServiceManagement symbols. The only launchd-adjacent fact is the preinstall
`rm -rf /Library/LaunchAgents/com.parsec.app.plist` (legacy artifact cleanup, no plist
shipped in the pkg). The dylib does contain a user-facing `Run when my computer starts`
setting string (dylib.strings:14069,29119) plus `app_silent`/`app_visibility`/`app_flags`
keys (14062-14078), but **no** config key for autostart (`run_at|start_at|auto_start|`
`autostart|login_item|launch_at|boot|startup` search: only hit is the
`parsec-macos-startup.pkg` URL). Mechanism (Login Item vs LaunchAgent vs none) is
**unanswered statically**; the planned follow-up (not executed here) is a VM run that
toggles the setting and inspects `~/Library/LaunchAgents`, `~/Library/Application
Support/com.apple.backgroundtaskmanagement`, `login items`
(`osascript -e 'get login items'`), and `fs_usage`/`execsnoop` to see what persists
`parsecd`.

**1b. Stub -> skel -> dylib (confirmed, exact strings + call sites).**
The stub is a libmatoya-based loader ("LOADER_V14", stub.strings:56,634). Contiguous
`__cstring` run at thin-x86_64 file offsets 0x9295-0x9519 (decoded via `dd`+`strings -t x`):
`.parsec`, `skel`, `wx_main`, `LOADER_V14`, `lock`, `lock_client`,
`File System Error - {Lock,Lockfile,Client Lockfile,Skel Copy,Skel Missing or Broken,Skel Crash}`,
`\napp_block_hash=%s\n`, `config.txt`, `%s%cskel`, `/Applications/%s.app/Contents/Resources/skel`,
`/Users/Shared`, `so_name`, `appdata.json`, `Parsec`, `setup.json`, `useProgramData`,
`\/`, `IS_MLU`, `cc%u.txt`, `%s%ccc%u.txt`, plus join fmt `%s%c%s`.
`_main` (0x100003b50, x86_64) `leaq(%rip)` targets resolve (instr+7+disp) to exactly these:
`0x9295=".parsec"`, `0x929d="skel"`, `0x9422="%s%cskel"`, `0x942b="/Applications/%s.app/.../skel"`,
`0x94aa="/Users/Shared"`, `0x92bb="wx_main"`, `0x94cd="Parsec"`, `0x94d4="setup.json"`,
`0x94df="useProgramData"`, `0x93b5="lock_client"`, `0x9417="config.txt"`, etc.
So: home dir comes from `home_get_dir`/`getpwuid` (imported `_getpwuid`; `'getpwuid' failed`
string), own path from `_NSGetExecutablePath` (`MTY_GetProcessPath`; `MTY_JoinPath` builds
paths with `%s%c%s`), source skel is `/Applications/<bundle-name>.app/Contents/Resources/skel`
(bundle name is a parameter = `Parsec`, so a renamed app still resolves but the parent
**must** be `/Applications`). The earlier static interpretation placed the seed
under `<home>/.parsec/skel`; the later [smoke test](smoke-test.md) measured
`appdata.json` and the loaded client directly under `<home>/.parsec` instead.
Use the measured layout for fresh-state seeding. Shared mode uses
`/Users/Shared/.parsec/...` when `setup.json` sets `useProgramData` (per-computer;
dylib strings confirm `per-user`/`per-computer`/`install_type`, 13589-13592).
Single-instance via `flock` (`lock`, `lock_client` names) and crash counting via
`cc%u.txt`/`%s%ccc%u.txt` + `hash`/`count` JSON keys.
Load sequence in `_main` tail (x86_64 disasm): `MTY_CryptoHashFile` (call at ~0x10000435e)
-> `MTY_SOLoad` (`dlopen` wrapper, ~0x10000436e) -> `MTY_SOGetSymbol` (`dlsym` wrapper,
~0x100004386, symbol `wx_main`) -> call. Failure dialogs via `MTY_ShowMessageBox`
(`Cannot load Parsec`, `...incompatible with your machine...`, `...out of date or corrupt.
Please try reinstalling from parsec.app.`). Restart support: `MTY_RestartProcess` +
`MTY_StartInProcess` (`_execv` imported).

**1c. Self-update + verification.**
Updater lives in the **dylib**, not the stub: stub has zero `builds.parsec.app`/`channel/`
strings (only the reinstall sentence); dylib has `builds.parsec.app` (13881),
`/channel/%s/appdata/%s/latest` (13883), `/channel/%s/binary/%s/gz/%s` (13884),
`appdata.json` (13882), `app_block_hash` (13880), `app_ui_channel` (13879),
`updater: We got an update` (13885), `hosting_get_updater_state` (13798-13799),
`MTY_HttpAsyncRequest`, `Using websocket host '%s'`, and the ARM-migration pointer
`https://builds.parsec.app/package/parsec-macos-startup.pkg` (13304, next to
`Build Date: Nov 18 2025`, `Parsec performs best when native, ... to download the new
ARM installer`). Verification is **SHA-256, symmetric-hash style**: `appdata.json`
carries a hex `hash` of the whole `so_name` dylib (byte-identical match verified) plus
per-block `app_block_hash`; both binaries import `_CC_SHA256` (and `_CC_SHA1`, stub also
`_CCHmac`/`_CCRandomGenerateBytes`) from libSystem; stub defines `MTY_CryptoHash`/
`MTY_CryptoHashFile`. No `rsa|ed25519|SecKeyVerify|SecTransform|SecPolicy|SecKey`
symbols anywhere (`rg -i` over both dumps + `dyld_info -imports`: only `_CC_SHA1`/
`_CC_SHA256` + CG/IOSurface noise). Outer trust is Apple codesign (Developer ID +
hardened runtime) on both Mach-Os. No standalone signature-verification routine found
statically; the planned follow-up is disassembling the dylib updater around the
`/channel/` users (stripped, so anchor on the `updater: We got an update` and
`app_block_hash` cstring xrefs) or observing traffic in a VM.

## 2. Who presents the UI (LSBackgroundOnly=true)

Both halves are UI-capable; the **dylib owns the main window**, the stub owns the
bootstrap/app shell:
- Stub links AppKit/Metal/IOKit/OpenGL/Foundation/CoreFoundation/SystemConfiguration/
  Carbon/WebKit/QuartzCore (otool -L) and imports `NSApplication/NSWindow/NSMenu/NSAlert/
  NSWorkspace/NSTimer/WKUserScript/...`; selectors include `finishLaunching`,
  `applicationDidFinishLaunching:`, `setActivationPolicy:`, `activateIgnoringOtherApps:`,
  `makeKeyAndOrderFront:`, `openURL:`, `terminate:`, `sharedApplication`, plus the full
  `WKScriptMessageHandler` protocol block and `userContentController:
  didReceiveScriptMessage:` / `evaluateJavaScript:completionHandler:` / `loadRequest:` /
  `loadHTMLString:baseURL:` / `addUserScript:` selectors (stub __objc_methname addrs
  0x100009b2a/0x10000a6df/0x10000a9ce/0x10000a9e6/0x10000b533). With `LSBackgroundOnly=true`
  the process starts without Dock/menu, then presumably calls `setActivationPolicy:` +
  `activateIgnoringOtherApps:` to become foreground when it shows windows (standard
  background-agent pattern; exact call order not traced statically).
- Dylib (loaded via `wx_main`, the sole global `T _wx_main` since it is stripped)
  links AppKit + WebKit **strongly** (`LC_LOAD_DYLIB`, not weak; ScreenCaptureKit is the
  only weak link) plus Metal/VideoToolbox/IOSurface/CoreMedia/AVFoundation/CoreAudio/
  Security/SystemConfiguration, and owns user-visible strings: `Parsec could not create
  the main window` (14066), `Parsec - Screen %d`, `Click here to open Parsec...`,
  the whole Client/Host/Experimental settings corpus, and `Navigating WebView to %s...`.
- Execution location: the **process** is `/Applications/Parsec.app/Contents/MacOS/parsecd`
  (path discovered via `_NSGetExecutablePath`), but the **code** it runs is
  `~/.parsec/parsecd-<ver>.dylib` (later measured layout; or `/Users/Shared/.parsec/...` per-computer).
  So the client runs from /Applications *and* from ~/.parsec simultaneously by design:
  immutable launcher + user-writable payload.

## 3. UI stack

**libmatoya v5.0: vendored statically, not loaded.** `libmatoya/v5.0` (dylib.strings
11456,26271 = arch slices), `https://github.com/snowcone-ltd/libmatoya` (908,13537,15723),
`MTY_*` log prefixes everywhere, UA `parsec/%s %s/%s libmatoya/%s` (12894).
`otool -L` shows **no** libmatoya dylib dependency; `nm` on the thin dylib slice finds
**zero** `MTY_` symbols (stripped; only `T _wx_main` global of interest, 630 total symbols
all `U` except a handful), while the stub defines ~40 `t _MTY_*` (Alloc/CopyFile/CryptoHash/
SOLoad/SOGetSymbol/RestartProcess/ShowMessageBox...). Conclusion: libmatoya is compiled
in. windowing via `MTY_WindowCreate`/`MTY_AppStayAwake` log strings (11510-11513) and
`__mty_app_app_0/__mty_app_window_0/__mty_app_view_0` view names. Next check for widget
internals: class-dump/WK introspection at runtime; static ObjC metadata only shows
protocol conformances (`NSApplicationDelegate`, `NSWindowDelegate`,
`WKScriptMessageHandler`, ...), no custom class names (classname section holds only
those Apple protocols).

**WKWebView: present, JS-bridged, URL unresolved statically; looks like the opt-in/
rollout web UI, not the primary native UI.**
Both binaries send `evaluateJavaScript:completionHandler:` / `loadRequest:` /
`loadHTMLString:baseURL:` / `addUserScript:` and implement
`userContentController:didReceiveScriptMessage:` (dylib methname addrs
0x2ec709/0x2ecb83/0x2ecb9b/0x2edad4; failure log `'WKWebView:evaluateJavaScript' failed
to evaluate string '%@'`, 11465). Capability/rollout markers: `features_webview` (12960),
`client_web_ui` (14095), `X-Parsec-WebView-Enabled` request header (12827),
`app_ui_channel` (13879), `release-ui` (13706), `app_new_ui_prompt` + `app_can_logout`/
`app_friends` nearby. The navigate event is logged as `Navigating WebView to %s and will
check for an update at %s` (14096), i.e. WebView navigation doubles as an update check,
fed by the `/channel/` endpoints above. **No target URL string exists**: no
`web.parsec.app`, no `file://` bundle page, no `localhost` UI URL in either dump; the
bundle ships no HTML/JS; `mainBundle`/`fileURLWithPath:` selectors exist but bind to no
visible page name. Candidates by analogy (Windows WebView2 UI) would be a downloaded
`release-ui` bundle via `/channel/%s/binary/%s/gz/%s`, but that is inference, not
evidence. Other URLs actually embedded: `https://dash.parsec.app` (+`/forgot`, `/signup`,
`/plan/?getWarp...`), `https://parsec.app/downloads|/config|/changelog|/privacy|/terms`,
support articles (Alpha-Channel, 4:4:4-FFmpeg, mic, macOS-install host_audio, provision
teams, error codes, feature matrix), `https://parsec.gg/g/%s/%s/`, avatar CDN,
`kessel-api.parsec.app`, `public.parsec.app`, `stun.parsec.app`, `stun6.parsec.app`.
**No CDP/remote-inspection channel**: `rg -i "remote.?debu|devtools|CDP|chrome-devtools|
inspectable|setInspectable|webInspector|allowsInspect|8097|9222|localhost.*debug"` =>
only a random-blob false positive (`,OpDCoO++OoCDpO,i`). Automation surface is therefore
`evaluateJavaScript:` from inside the process + `WKScriptMessageHandler` inbound, i.e.
usable only with code execution in `parsecd`, plus Safari Web Inspector out-of-process
(which needs the app to allow inspection; entitlements lack `get-task-allow` and the
hardened runtime blocks injection, so expect `defaults write`/Safari-Developer gating or
re-signing to be necessary). Next checks: break on `-[WKWebView loadRequest:]` /
`evaluateJavaScript:completionHandler:` in a VM, log the NSURL + JS strings; check
`WKWebView._allowsInspectableWebContent`/`inspectable` default via runtime introspection.

## 4. Settings storage + Bandwidth ladder (high-bitrate grounding: YES)

Config keys live in the dylib (representative slice dylib.strings:13640-13700):
`app_force_relay`, `network_*`, `client_decoder_index/_h265/_444/_10bit`, `client_zero_copy`,
`encoder_fps`, **`encoder_bitrate`**, `host_output`, `host_encoder_min_bitrate`,
`encoder_min_qp/_max_qp`, `encoder_vbv_*`, `encoder_idr_interval`, `encoder_slices`,
`host_virtual_*`, `server_max_clients`, ... The embedded template text (13716-13727):
`See https://parsec.app/config for documentation and example. JSON must be valid before
saving or file be will be erased.` + `# NOTICE: From Parsec 150-89, the advanced
configuration has moved to config.json` + `# The config.json file located in the same
folder as config.txt.` + `# You can still add configuration to this file (config.txt)
for backwards compatibility.` + `# When you save this file (config.txt) ... migrated
to config.json.` + `# This file (config.txt) will be cleared once the setting(s) have
been saved and migrated.` + `# All configuration settings must appear on a new line.` +
`# All whitespace, besides the newline character '\n', is ignored.` +
`# See https://parsec.app/config for more information.` + `# Example:` +
`# encoder_bitrate = 10`. Runtime messages: `Failed to read config.json (%d/10)`,
`The config.json is present, but failed to be read.`, `Saving configs transferred from
config.txt`; sibling files `window.json`, `status.json`, `user.bin`, `devid.bin`.
Base directory is the same per-user/per-computer choice as skel (`~/.parsec` vs
`/Users/Shared/.parsec`, plus `.parsec-persistent`), i.e. config lives next to the
payload in a user-writable dir. Absolute leaf names (`config.json`/`config.txt`) confirmed;
full absolute path depends on `setup.json useProgramData` + home dir, so confirm by
reading `https://parsec.app/config` and the macOS-install support article
(`.../32381555792148-Install-Parsec-App-on-macOS#host_audio`, embedded string) or a VM run.
**Bandwidth ladder confirmed byte-identical to orbit's Windows assumption**: `Bandwidth
Limit` (13120 client w/ help `- Parsec will only use this amount when it needs to...`,
13377 decoder-adjacent, 13418 host `The maximum bandwidth used by Parsec, split between
all connected guests.`) with options `3,5,7,10,15,20,25,30,35,40,45,50 Mbps` (11277-11288)
and `%.0f Mbps`/`%.2fMbps` formats; **no** `>50/custom/unlimited` strings. This is exactly
the 12-entry ladder in
`crates/parsec-orbit-plugin-high-bitrate-native/src/legacy.rs:45-56` and
`crates/parsec-orbit-plugin-high-bitrate-webview/src/lib.rs:6`. High-bitrate-native-style
work is therefore grounded on macOS (same option model + `encoder_bitrate` key), but the
Windows PE signature scanner cannot be reused: the table lives in a stripped Mach-O
(`__cstring` is trivially findable; the *option-record* layout/codegen is not) and must
be re-derived per build (anchor on the 12 label bytes, then trace the two use sites the
way `legacy.rs` documents for PE).

## 5. Dynamic loading surface

- Stub (`nm -u`: `_dlopen/_dlsym/_dlclose/_dlerror` from libSystem; wrappers
  `t _MTY_SOLoad/_MTY_SOGetSymbol/_MTY_SOUnload`): the **only** data-driven target is the
  versioned client dylib (`so_name` from `appdata.json`, resolved under the user-writable
  skel dir). No plugin directory, no `@rpath/@loader_path` (pure executable, no RPATH cmds).
- Dylib (`nm -u`/`dyld_info -imports`: `_dlopen`, `_CC_SHA1/_CC_SHA256`,
  `_OBJC_CLASS_$_NSWorkspace/_WKUserScript`, no ServiceManagement/SecKey symbols;
  `LC_ID_DYLIB = parsecd-150-101a-<arch>.dylib` bare name, no RPATH, WebKit strong-linked,
  ScreenCaptureKit weak): data-driven loads are (a) the same `MTY_SOLoad` wrapper family
  (`MTY_SOLoad`/`MTY_SOGetSymbol` + `'dlopen'/'dlsym'/'dlclose' failed` strings 11430-11436)
  and (b) **`libusb-1.0.dylib`** (`2e8cc4`, `libusb 1.0.27`): `libusb-1.0.dylib Not Found`,
  `Yubikey Selection Requires LibUSB`, `Click to download LibUSB, or select Self-Managed
  above to use an external LibUSB library`, `libusb not present, cannot claim USB device`,
  `hosting_fetch_libusb/hosting_send_ipc_message` IPC. Tellingly, a Windows leftover sits
  beside it: `By toggling ... Parsec will attempt to load a user-provided libusb-1.0.dll
  from C:\Windows\System32` — the macOS self-managed search path is **not** in strings;
  next check is xrefs on the `libusb-1.0.dylib` cstring (file offset 0x2e8cc4 in thin x86_64)
  in a disassembler.
- **Loaded from user-writable locations: YES — the entire real client**
  (`~/.parsec/parsecd-<ver>.dylib`, later measured layout, or `/Users/Shared/.parsec/...`). That is the
  future-loader surface: whoever controls skel (or the hash check in front of it) controls
  execution. `libmatoya` is NOT a load-time surface (statically vendored, §3).
- Hardening caveat: hardened runtime WITHOUT `disable-library-validation` or
  `allow-dyld-environment-variables` ignores `DYLD_INSERT_LIBRARIES`/`DYLD_LIBRARY_PATH`
  and only loads Apple- or same-team-signed libraries - **except** through the app's
  own `dlopen` of the user-writable versioned dylib. That load has two gates: the
  stub's SHA-256 check (`MTY_CryptoHashFile` against `appdata.json`) and dyld's team
  check (library validation; the verified model is in
  [loader-as-host.md](loader-as-host.md)). Any loader design must also handle the
  `flock` single-instance locks, `cc%u.txt` crash counters and the `LOADER_V14`
  compat token.

## 6. Feature-mapping verdicts (orbit -> macOS)

| Orbit feature (Windows mechanism) | macOS counterpart? | Verdict + evidence |
|---|---|---|
| loader/proxy (`bcrypt.dll` PE proxy; `crates/parsec-orbit-loader`, naked-asm thunks, `DllMain` plugin host) | No import-proxy equivalent; nearest surface is the **skel `dlopen`** of a user-writable versioned dylib | REDESIGN — no static `bcrypt` import to proxy; `MTY_SOLoad`+`wx_main`+SHA-256-gated skel path (§1b/§5) is the hook point, needs a Mach-O loader (or shim dylib + hash/pinning handling), not a PE proxy |
| webview-bridge (CDP over WebView2 DevTools `:9222`; `Runtime.evaluate`/`Page.addScriptToEvaluateOnNewDocument`, `bridge.rs:571-675`) | WKWebView with `evaluateJavaScript:` + `WKScriptMessageHandler`, **no CDP port** | REDESIGN — JS evaluation exists but transport differs completely; bridge must target `-[WKWebView evaluateJavaScript:completionHandler:]`/`addUserScript:` from in-process code (or Safari inspector channel), not `http://127.0.0.1:9222/json` (`loader/src/webview.rs:24-48`) |
| high-bitrate-webview (JS via bridge, `assets/bandwidth.js`, `..500 Mbps`) | Same ladder strings, `client_web_ui`/`release-ui` UI exists, but UI bundle URL + DOM unknown | BLOCKED PENDING RUNTIME — grounded model, but need the real WebView URL + DOM: break `loadRequest:`/`evaluateJavaScript:` in VM, then port `bandwidth.js` selectors |
| high-bitrate-native (PE sig-scan + patch of 12-entry `3..50 Mbps` table, `legacy.rs`) | Same 12 labels + `encoder_bitrate` key in dylib | FEASIBLE WITH REWORK — labels confirmed (11277-11288); scanner/table-record code (`legacy.rs`) must be rewritten for stripped Mach-O (anchor labels, re-derive record layout + the two use sites per build) |
| config (`parsec-orbit-plugin-config`) | `config.json`/`config.txt` migration pair, `encoder_bitrate = N` line format, user-writable dir | PORTABLE IN PRINCIPLE — same key namespace; only path resolution (per-user vs `/Users/Shared`) + migration semantics (`config.txt` cleared after migrate) need macOS handling |
| hooks (`retour` inline hooks) | No equivalent found | PORTABLE IN PRINCIPLE — needs a macOS inline-hook backend (no `retour` on Mach-O); pattern-level portability only |
| js-runtime (`Boa`) | No equivalent found | PORTABLE IN PRINCIPLE — pure logic, platform trait already exists (`core` is "Win32-free" behind `Platform`; `load(path)->Module`) |
| market (manifests/discovery/ordering) | No equivalent found | PORTABLE IN PRINCIPLE — same caveat as js-runtime; verification harness (`verify-*`, e2e) is PE/UWP-shaped and needs a Mach-O counterpart |

## 7. Exact next checks (where static ran out)

1. Autostart/keepalive: VM install -> toggle `Run when my computer starts` -> snapshot
   `~/Library/LaunchAgents`, `backgroundtaskmanagement`, Login Items, `launchctl list`,
   `fs_usage -f filesys parsecd`.
2. Skel/hash behavior: copy `~/.parsec` before/after first run; tamper-copy test with a
   bit-flipped dylib + forged `appdata.json` (VM only) to see which of
   `File System Error - {Skel Copy, Skel Missing or Broken, Skel Crash}` fires and whether
   the SHA-256 or codesign bites first.
3. WebView URL+DOM: breakpoint/log `-[WKWebView loadRequest:]`,
   `loadHTMLString:baseURL:`, `evaluateJavaScript:completionHandler:`,
   `userContentController:didReceiveScriptMessage:`; capture `release-ui` bundle fetch
   (`/channel/.../binary/.../gz`) via proxy.
4. libusb macOS path: xrefs on thin-x86_64 file offset `0x2e8cc4` (`libusb-1.0.dylib`)
   in Hopper/IDA/Binary Ninja; check `~/lib`, bundle dir, `~/.parsec` candidates.
5. Native bitrate table: anchor the 12 `N Mbps` cstrings, find the option-record array +
   two use sites (cf. `legacy.rs` documents the PE shape), confirm `encoder_bitrate`
   clamping lives client-side.
6. Updater trust: disassemble around `updater: We got an update`/`app_block_hash` users;
   confirm hash algorithm scope (whole-file vs blocks) and any signature beyond SHA-256.

## Appendix. Key commands (all read-only)

```
plutil -p .../Parsec.app/Contents/Info.plist
cat .../Resources/skel/appdata.json
shasum -a 256 .../Resources/skel/parsecd-150-101a.dylib
codesign -dvv .../Parsec.app ; codesign -d --entitlements - .../Parsec.app
codesign -dvv .../Resources/skel/parsecd-150-101a.dylib
otool -L (stub + dylib) ; otool -arch x86_64 -l (LC_LOAD/RPATH/ID)
nm -arch x86_64 -u (imports) ; nm -arch x86_64 (defined MTY_/wx_main)
dyld_info -imports .../parsecd-150-101a.dylib | rg -i "dlopen|WK|WebKit|Sec|CC_"
rg -n -i "LaunchAgent|SMAppService|login.?item|launchctl|SMLoginItem|XPC" work/*.strings.txt
rg -n "skel|appdata|wx_main|dlopen|\.parsec" work/stub.strings.txt
rg -n "builds.parsec.app|/channel/|appdata|updater" work/dylib.strings.txt
rg -n "encoder_bitrate|Bandwidth Limit|Mbps" work/dylib.strings.txt
rg -n "WKWebView|evaluateJavaScript|WKUser|loadRequest|loadHTMLString|client_web_ui|release-ui|app_ui_channel|features_webview" work/dylib.strings.txt
otool -arch x86_64 -tv ( _main / _MTY_SOLoad / _MTY_CryptoHash(File) / _MTY_RestartProcess / _appdata_parse_file )
lipo -thin x86_64 ... ; strings -a -t x <thin> | rg "skel|appdata|wx_main|libusb|Navigating WebView|release-ui"
```
Repo citations: `crates/parsec-orbit-loader/src/webview.rs:24-48` (DevTools port),
`crates/parsec-orbit-plugin-webview-bridge/src/lib.rs:20-21` (`DEFAULT_DEVTOOLS_PORT 9222`),
`src/bridge.rs:571-675` (`Runtime.evaluate`/`Page.addScriptToEvaluateOnNewDocument`),
`crates/parsec-orbit-plugin-high-bitrate-native/src/legacy.rs:45-56` (12-entry ladder),
`crates/parsec-orbit-plugin-high-bitrate-webview/src/lib.rs:6-18` (`bandwidth.js` model).
