# Loader-as-host: verified contract and option ranking

Historical experiment: signing/entry conclusions here apply to that run. Current patched-bundle evidence is in [native-dependency-poc.md](native-dependency-poc.md); current requirements are in [ADR 0014](../decisions/0014-macos-patched-bundle.md).

Independent re-verification of [loading-constraints.md](loading-constraints.md) plus
the static evaluation of the loader-as-host design; its dynamic confirmation is
in [smoke-test.md](smoke-test.md).

Run: 2026-10-13, macOS 27.0 (26A428), arm64. The client package (Parsec
150.101.1) and the toy binaries were analysed but never executed - execution of
the real client happens only in the smoke test. Repo cites are to the paths in
this repository.

## Part 1 - re-verification of the loading constraints

The load-bearing claims of [loading-constraints.md](loading-constraints.md) were re-run
independently. Results:

1. Cell B (hardened-none + INSERT stripped): **CONFIRMED**.
   Re-run with `DYLD_INSERT_LIBRARIES` set on the hardened toy app ->
   `APP pid=69953 DYLD_INSERT_LIBRARIES=(null)` / `APP environ_count=53` / `APP target(41)=42` / `exit=0`. No marker, no stderr. Matches B byte-for-byte (incl. environ 53).
2. Cell C (hardened-both + INSERT + PRINT_LIBRARIES): **CONFIRMED**.
   Same env on `app-hard-both` -> `DYLD_INSERT_LIBRARIES=<payload>` visible, `environ_count=55`
   (54 + DYLD_PRINT_LIBRARIES), `exit=0`; stderr contains
   `dyld[69962]: <7DA0530B-...> /private/.../payload.dylib`. (B reported 87 stderr lines; not recounted - irrelevant.)
3. Cell F2 (hardened-none dlopen adhoc): **CONFIRMED**.
   dlopen of the ad-hoc-planted toy dylib from the hardened toy app -> `DLOPEN result=FAILED`,
   `... not valid for use in process: mapping process and mapped file (non-platform) have different Team IDs ...`, `exit=1`. Exact B error string reproduced (modulo pid/uuid).
4. patchtest on app-plain: **CONFIRMED**.
   patchtest on the plain toy app -> `PATCH mprotect(RW) rc=-1 errno=13 before=42` / `PATCH result=DENIED_AT_MPROTECT` / `exit=1`.
5. `otool -l` stub has no LC_RPATH / LC_DYLD_ENVIRONMENT / @rpath deps: **CONFIRMED**.
   `otool -l .../parsecd | grep -c -e LC_RPATH -e LC_DYLD_ENVIRONMENT` -> `0`.
   `otool -L` (both slices): only system frameworks (AppKit/Metal/IOKit/OpenGL/Foundation/
   CoreFoundation/SystemConfiguration/Carbon/WebKit/QuartzCore/CoreGraphics/CoreServices/
   CoreVideo) + libSystem + libobjc. Zero @rpath/@loader_path entries.
6. retour 0.3.1 aarch64 gate: **CONFIRMED** from the published crate.
   Fetched `https://static.crates.io/crates/retour/retour-0.3.1.crate`, sha256
   `a9af44d40e2400b44d491bfaf8eae111b09f23ac4de6e92728e79d93e699c527`, identical to the
   `checksum =` line under `name = "retour" / version = "0.3.1"` in the repo `Cargo.lock`
   (which was grepped directly). Its `src/arch/mod.rs` contains exactly this
   x86-only gate:
   `cfg_if! { if #[cfg(any(target_arch = "x86", target_arch = "x86_64"))] { mod x86; ... }`
   `else { // TODO: Implement ARM/AARCH64/MIPS support! } }`. No aarch64 Patcher/Trampoline.
7. `security find-identity -v -p codesigning`: **CONFIRMED** -> `0 valid identities found`, exit 0.

Part 1 score: 7/7 CONFIRMED, 0 refuted. (The matrix's D/E/F0/F1/F3-F5/G,
JIT-exec, xattr/quarantine and provisioning-profile claims were NOT re-run;
they are not load-bearing for Parts 2-3.)

## Part 2 - reconciling the two reports (combined model)

The internals report's "resolved by path + SHA-256, not team pinning" and the
matrix's F2 team rejection are both correct and compatible: they describe two
sequential gates owned by different parties.
The stub's own check is path + SHA-256 (it `snprintf`s the skel path, `MTY_CryptoHashFile`
SHA256-HEXes the dylib, `Strcasecmp`s against `appdata.json` "hash" - see Part 3a), which a
forged skel copy would pass if we also rewrote `appdata.json`. But the stub then calls
`MTY_SOLoad` = plain `dlopen(RTLD_NOW|RTLD_LOCAL)` (libmatoya `src/unix/system.c:22-34`),
and at that point **dyld applies the kernel/code-signing policy of the already-running
hardened stub process**: without `disable-library-validation` (re-verified absent today via
`codesign -d --entitlements - parsecd`: only application-identifier, persistent-content-
capture, team-identifier, audio/camera/mic device keys), dyld demands the mapped dylib's
Team ID equal the process's (Y9MY52XZDB). An attacker-writable file can satisfy the stub's
hash check but can never satisfy dyld's team check, because we own no signing identity
(Part 1.7) and ad-hoc signatures carry no team (cell F5 in
[loading-constraints.md](loading-constraints.md)). Consequence: **planting a fake
skel dylib into the genuine stub is doubly dead** - the SHA-256 gate is the softer of two
gates, and the harder (team) gate cannot be edited without Parsec's private key or
re-signing the stub (which breaks the seal and, per B, drops the profile-gated
`persistent-content-capture` entitlement).

## Part 3 - static evaluation of loader-as-host (H / H2)

Method: `otool -arch x86_64/arm64 -tv` of stub `_main` (5379/7317 lines, dumped to
`work/main-x86_64.txt`, `work/stub-full-arm64.txt`; arm64 build annotates literal pools),
`nm -arch x86_64` imports/defined, `otool -Iv` bind table, dylib `nm -u` + `__objc_methname`
+ strings. libmatoya master fetched by shallow clone (HEAD 952f2a2) for MTY_* semantics.

### a. wx_main call contract (CONFIRMED, both slices agree)

Signature: `int wx_main(int *argc, char ***argv, const char *stateDir, const char *hashHex)` -
FOUR args, not (argc, argv):
- arm64 (`blr x26`, x26 = dlsym "wx_main" @0x4248): `x0=sp+0x34` (&argc), `x1=x22` (argv),
  `x2=x19` (state base dir, see below), `x3=sp+0x7e0` (65-byte SHA256-hex buffer).
  (0x4568-0x4578 in work/stub-full-arm64.txt)
- x86_64 (`callq *-0xee0(%rbp)` @0x4697): `rdi=-0xed8` (&argc), `rsi=r13` (argv),
  `rdx=rbx` (base dir), `rcx=-0x730` (hash hex). (work/main-x86_64.txt:641)
- argv is NOT verbatim: stub copies at most 0x7f entries, then appends `"LOADER_V14"`
  (Strdup @x86 0x3d0c / same on arm64) and increments argc. So wx_main sees caller argv +
  one sentinel token.
- BEFORE the call, in order: `MTY_GetDir(0/1)` home resolution, `MTY_JoinPath`/`MTY_Strdup`
  path build, `getenv("IS_MLU")` -> `umask(0)` if set else `umask(1)`, `setup.json`
  "useProgramData" select (`~/.parsec` vs `/Users/Shared/.parsec`), skel seed-copy from
  `/Applications/Parsec.app/...` if missing (`MTY_GetFileList`+`MTY_CopyFile` loop),
  `flock` lockfiles ("lock", "lock_client"), `_appdata_parse_file` (reads "so_name",
  0x100-byte "hash", "count" crash counters), `MTY_CryptoHashFile(algo=3,...)` =
  `MTY_ALGORITHM_SHA256_HEX` (libmatoya `src/crypto.c:121`, `src/matoya.h:1568`) into a
  0x41-byte buffer, `Strcasecmp` vs expected (mismatch -> "Skel Missing or Broken" family),
  `MTY_SOLoad` (plain dlopen), `MTY_SOGetSymbol(handle,"wx_main")`, append
  `"\napp_block_hash=%s\n"` to `<base>/config.txt` (fopen "a+b"+fwrite+fclose, arm64
  0x40f0-0x4148), `MTY_WriteFile` (appdata rewrite w/ bumped count).
- AFTER the call: `MTY_DeleteFile`, `MTY_SOUnload`, `ret==1` -> `MTY_RestartProcess(argv)`
  (= `execv(MTY_GetProcessPath(), argv)`, libmatoya `src/unix/apple/macosx/system.m:61-67`,
  path via `_NSGetExecutablePath`); any other value becomes the process exit code
  (`_main` ends in `retq`, error paths via `MTY_ShowMessageBox`+return).
- **wx_main owns the event loop: CONFIRMED.** `_main`'s 24 external calls resolve (via
  `otool -Iv` bind table) to pure libc only (bzero/error/stack_chk/calloc/exit/fclose/
  fopen/fread/free/fwrite/getenv/memcpy/puts/qsort/realloc/snprintf/stat/strcmp/strpbrk/
  strtod/strtoull/umask/vsnprintf) plus local `_json_writer_indent`. Zero `objc_msgSend`,
  zero NSApplication/CFRunLoop/dispatch. All AppKit/WebKit/NSApplication ownership is
  dylib-side.

### b. LOADER_V14 / app_block_hash: which side checks what

- `LOADER_V14`: stub->dylib argv token (stub side: append; dylib side: parses). Dylib
  contains `LOADER_V` + log format `Parsec %s (150-101a, Service: %d, Loader: %d)`
  (dylib strings 19238/38186, next to `app_channel`/`app_debug`). So the dylib extracts a
  loader version (presumably int after LOADER_V) and logs it. Whether it REJECTS an
  unknown/absent version is **UNVERIFIED** statically (needs the H2 dynamic test).
- `app_block_hash=%s`: stub WRITES it (computed dylib hex) into the user config, dylib
  READS it (2 occurrences of the string in the dylib). Direction is opposite to a naive
  "stub verifies config" reading: the stub vouches for the dylib it just hashed.
  Enforcement semantics on the dylib side: UNVERIFIED.

### c. env handling

- Stub imports `_getenv` ONLY (`nm -u`: no setenv/putenv/unsetenv). Single call site:
  `getenv("IS_MLU")` (x86 @0x3c29; IS_MLU cstring 0x94f1). Set -> `umask(0)`; unset ->
  `umask(1)` + an extra `MTY_GetDir(1)`-based path build. It never sets/unsets anything,
  so wx_main simply inherits the host's environ. Consequence for H2: an ad-hoc-signed
  copied stub honors `DYLD_INSERT_LIBRARIES` (Part 1.2 mechanism), letting our loader
  ride in-process with full env control.
- Dylib imports `_getenv`; observed keys in strings: `IS_MLU` (same flag). `app_block_hash`
  and `app_ui_channel` are config-file keys, NOT env vars (former written to config.txt
  per (a); latter sits amid dylib config keys).

### d. host-identity checks in the dylib: none found (with stated limits)

- `nm -u`/`dyld_info -imports` on the arm64 dylib: NO `SecCode*`/`SecStaticCode*`/
  `SecTask*`/`SecRequirement*`, NO `proc_pidpath`/`getppid`/`csops`. Only three
  identity-adjacent imports: `__NSGetExecutablePath`, `_getenv`, `_sysctlbyname` -
  whose call sites/purposes are UNVERIFIED (could be logging, restart path, hw.model).
- Strings sweep for `tv.parsec.www` (1 hit: a `defaults write tv.parsec.www
  ApplePressAndHoldEnabled -bool false` help/hint string), `/Applications`, `LOADER`
  (only `loader`/`loader_v`/`LOADER_V` + the version log), `bundle` (only
  `CFBundleDisplayName`/`mainBundle` refs), `sign`/`tamper`/`integrity`/`unexpected
  process`/`parent` (only `cookie signature validation failed` = network cookie, and
  ImGui parent-window asserts): **no bundle-identity, team, signature, tamper, or
  process-parent validation errors exist**. Verdict: no static indication wx_main
  validates its host; NOT a proof of absence (stripped C++ could check argv[0]/exec
  path opaquely - dynamic test (i) decides).

### e. H2 placement: skel SOURCE name is a hardcoded literal (CONFIRMED)

- x86_64 @0x3f28/0x4058: `snprintf(buf,0x200,"/Applications/%s.app/Contents/Resources/skel",%s="Parsec")`
  where the %s arg is `leaq -> 0x1000094cd` = literal cstring `"Parsec"`.
- arm64 @0x3e98-0x3eac / 0x4018-0x402c: same format (literal-pool-annotated
  `"/Applications/%s.app/Contents/Resources/skel"`), %s = `x23` = `"Parsec"` literal
  loaded once @0x3b54-0x3b58 (`adrp+add` -> `"Parsec"`). Traced x23's assignment:
  no other writer on that path. NOT derived from `_NSGetExecutablePath`, Info.plist,
  `CFBundleName`, or argv[0].
- Consequence: **an H2 re-signed copy may live under any name at any path** (its
  `MTY_RestartProcess` re-execs itself via `_NSGetExecutablePath`, so restart works).
  The fixed `/Applications/Parsec.app` path is only a SEED SOURCE, consulted solely
  when `JoinPath(base,"skel")` fails `MTY_FileExists` (x86 0x3e85-0x3e99 / arm64
  0x3ff4-0x4018 branches). With a pre-seeded `~/.parsec/skel` (or per-machine
  `/Users/Shared/.parsec/skel`), the installed app is never touched. (Whether an
  existing-but-stale skel is re-synced each launch vs existence-only: UNVERIFIED.)

### f. app shell around wx_main + libmatoya reuse

- Ordered stub-side calls: MTY_GetDir, MTY_Strdup/Free, MTY_JoinPath, getenv/umask,
  calloc, snprintf, MTY_JSONReadFile/ObjGetItem/Bool/Destroy (setup.json),
  MTY_FileExists, MTY_Mkdir, MTY_DeleteFile, MTY_CopyFile, MTY_GetFileList/FreeFileList,
  MTY_GetSOExtension, MTY_Strcasecmp, MTY_CryptoHashFile, MTY_SOLoad, MTY_SOGetSymbol,
  [wx_main], MTY_SOUnload, MTY_RestartProcess, MTY_WriteFile, MTY_ShowMessageBox,
  MTY_ReadFile (appdata), flock. **No MTY_App*/NSApplication/CFRunLoop call exists in
  the stub** - the "shell" (NSAlert error dialogs via `MTY_ShowMessageBox`, confirmed
  `objc_msgSend$runModal`/`setMessageText:` on arm64) is bootstrap chrome only.
- libmatoya v5.0 (`src/unix/system.c:22-34` SOLoad = `dlopen(RTLD_NOW|RTLD_LOCAL)`;
  `src/crypto.c:121-135` hash-file; `src/unix/file.c:108-132` GetDir incl.
  `MTY_DIR_HOME=1`/`MTY_DIR_GLOBAL_HOME=3`, `src/matoya.h:1735-1746`;
  `src/unix/apple/macosx/system.m:61-67` RestartProcess = execv self) is
  **MIT-licensed** (`LICENSE`: `MIT License / Copyright (c) Snowcone Ltd.`,
  fetched 2026-10-13 from `https://github.com/snowcone-ltd/libmatoya`,
  shallow clone HEAD 952f2a2). A host (H) may legally link/compile it to replicate the
  bootstrap helpers byte-for-behavior; caveat: stub's bundled MTY may drift from master.

### g. WKWebView gating + JS automation surface (conceptual)

- Dylib `__objc_methname` (514 entries, arm64): `evaluateJavaScript:completionHandler:`,
  `loadRequest:`, `loadHTMLString:baseURL:`, `userContentController:
  didReceiveScriptMessage:` present; **`addUserScript:` absent (0 hits)** - the dylib
  never installs persistent user scripts. (Curiosity, not load-bearing: the STUB's
  methnames DO reference `addUserScript:` + `addScriptMessageHandler:name:` alongside
  the same evaluate/load selectors - the stub links WebKit strongly yet `_main` never
  touches ObjC, so the stub-side webview role, if any, is dead code or an error page;
  UNVERIFIED.)
- `features_webview`, `client_web_ui`, `app_ui_channel`, `release-ui` are dylib
  config/flag keys (neighbors: `features_h265/pen/gamepad/...`, `app_daisychaining`,
  `Navigating WebView to %s and will check for an update at %s`). So the web UI is
  flag/rollout-gated, consistent with C. No target URL / file:// page / localhost port
  strings were found (the internals report's claim; not re-swept).
- Conceptual verdict: an in-process loader CAN call `evaluateJavaScript:` on a WKWebView
  instance it can obtain, and CAN receive messages via its own script-message handler,
  but statically there is no creation-time hook (no `addUserScript:`) and no known
  instance handle - obtaining one needs runtime introspection (the (iii) dynamic test).
  No CDP equivalent exists; the Windows `webview-bridge` CDP transport
  (`crates/parsec-orbit-plugin-webview-bridge`, DEFAULT_DEVTOOLS_PORT 9222 per C) has
  no macOS counterpart and must be redesigned around `evaluateJavaScript:`.

## Part 4 - top unknowns for dynamic tests (list only, NOT run)

(i) Foreign-host execution: does `wx_main(&argc, argv+LOADER_V14, baseDir, hashHex)` start
    the full client when called from our own launcher (H) or a re-signed stub copy (H2)?
    User-visible (the real client would start); needs user approval. Decides H vs H2 and
    exposes any LOADER-version gate / exec-path sensitivity.
(ii) Entitlement scope under ad-hoc signing: does viewing-only work while hosting breaks
    (profile-gated `persistent-content-capture`), per the loading-constraints
    option-1 caveat? Decides whether
    H2 is viable for hosts or viewers-only.
(iii) WebView URL/DOM: break `loadRequest:`/`evaluateJavaScript:` at runtime, capture the
    `release-ui` URL + DOM, then port `bandwidth.js`/bridge selectors. (Gates the
    high-bitrate-webview port; the 12-entry `3..50 Mbps` ladder itself is confirmed
    shared ground: repo `crates/parsec-orbit-plugin-high-bitrate-native/src/legacy.rs:45-56`.)

## H/H2 verdict (static)

H2 is the statically favored design: ad-hoc copy honors INSERT (Part 1.2), vendor
bootstrap runs byte-identical (no replication risk), any-path placement is legal
((e)), and no host-identity check was found ((d)). H is fully specified as fallback
(all bootstrap steps + MIT helpers in (a)/(f)) but reimplements crash-counter/appdata
semantics we only partly mapped. Both hinge on dynamic (i): LOADER-version gate,
exec-path sensitivity, and what `_NSGetExecutablePath`/`_sysctlbyname` actually gate.

> **Dynamic follow-up (same day).** [smoke-test.md](smoke-test.md) resolves (i)
> for the current build: the re-signed copy with an injected dylib booted the
> real client, `Loader: 14` was accepted, the client self-updated inside its
> isolated state directory, and the installed app plus the user's live state
> were untouched.
