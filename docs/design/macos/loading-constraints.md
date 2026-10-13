# macOS loading and signing constraints (toy-harness evidence)

Historical experiment: signing/entry conclusions here apply to that run. Current patched-bundle evidence is in [native-dependency-poc.md](native-dependency-poc.md); current requirements are in [ADR 0014](../decisions/0014-macos-patched-bundle.md).

Empirical answer to "can a foreign dylib be loaded into the Parsec macOS
client?", measured with a toy harness rather than with the client itself. This
document records measurements and the options that were evaluated; it is
design input for the macOS spec (see [README.md](README.md)), not an
operational guide, and none of it was performed against an installed client or
live user state. The two-gate model that reconciles these results with the
client's own `dlopen` behaviour, and its independent re-verification, are in
[loader-as-host.md](loader-as-host.md). The loader-as-host design - which is
not part of this document's option list because it was evaluated in the next
round - is confirmed dynamically in [smoke-test.md](smoke-test.md).

Run: 2026-10-13, macOS 27.0 (build 26A428), arm64 native (no Rosetta), Apple
clang 21.0.0. `security find-identity -v -p codesigning` => **0 valid
identities** (no Developer ID certificate available). The real Parsec app was
never executed or installed; the harness (`app.c`, `payload.c`, `jit-exec.c`,
`ent-*.plist`, `bin/`, `fakeparsec/`) is in the local research archive.

The first harness build did not compile (missing `<errno.h>`, `<stdint.h>`);
the fix added those includes plus a `dlopen <path>` argv mode for cell F.

## 1. Verified Parsec package facts (both binaries)

Target: `/tmp/parsec-macos/expanded/Payload/Parsec.app`, v150.101.1, bundle id `tv.parsec.www`, `LSBackgroundOnly=true`.

`codesign -d -vvv --entitlements - Contents/MacOS/parsecd`:

- `Format=app bundle with Mach-O universal (x86_64 arm64)`, 266 KB stub.
- `flags=0x10000(runtime)` (hardened runtime), `TeamIdentifier=Y9MY52XZDB`, `Authority=Developer ID Application: Parsec Cloud, Inc. (Y9MY52XZDB)`, Runtime Version 15.5.0.
- Entitlements: `com.apple.application-identifier` (Y9MY52XZDB.tv.parsec.www), `com.apple.developer.persistent-content-capture=true`, `com.apple.developer.team-identifier`, `com.apple.security.device.audio-input/camera/microphone`. **Absent:** `allow-dyld-environment-variables`, `disable-library-validation`.
- `spctl -a -vvv -t exec parsecd` => `accepted, source=Notarized Developer ID, origin=Developer ID Application: Parsec Cloud, Inc. (Y9MY52XZDB)`.

`codesign -d -vvv --entitlements - Contents/Resources/skel/parsecd-150-101a.dylib`:

- `Format=Mach-O universal (x86_64 arm64)`, 6.2 MB, `flags=0x10000(runtime)`, same Developer ID team. No bound entitlements (dylib).
- `spctl -a -vvv` => `accepted, source=Notarized Developer ID` (same origin).
- Its install-name differs per slice (`parsecd-150-101a-x86_64.dylib` vs `...-arm64.dylib`); no LC_RPATH.

`otool -l parsecd`: LC_LOAD_DYLIB entries are system frameworks + libSystem/libobjc only (AppKit, Metal, IOKit, OpenGL, Foundation, CoreFoundation, SystemConfiguration, Carbon, WebKit, QuartzCore, CoreGraphics, CoreServices, CoreVideo). **No LC_RPATH, no LC_DYLD_ENVIRONMENT** (grep count for `LC_DYLD_ENVIRONMENT` = 0), no `@rpath/@loader_path` dependency. There is NO appdir-relative DLL search path: unlike Windows (appdir `bcrypt.dll` proxy), dyld has no load slot we can squat.
`Contents/embedded.provisionprofile` exists ("CI Provisioning Profile", Team Y9MY52XZDB, ProvisionsAllDevices=true); its Entitlements grant `persistent-content-capture` + `application-identifier` + `keychain-access-groups` + `team-identifier`. The hardened-runtime dyld entitlements are NOT in the profile either.
xattr: every file carries only `com.apple.provenance`; **`com.apple.quarantine` is absent** (fresh `pkgutil --expand-full`, never quarantined).
`strings parsecd` (both slices): `'%s%cskel'`, `/Applications/%s.app/Contents/Resources/skel`, `.parsec`, `'dlopen' failed to find '%s'`, `File System Error - Skel Missing or Broken/Copy/Crash`, `app_block_hash=%s`, `config.txt`, `/Users/Shared`. `skel/appdata.json` = `{"entry_symbol":"wx_main","hash":"9cce85...cdd5","so_name":"parsecd-150-101a.dylib"}`. So the stub resolves the real client from `/Applications/<name>.app/...skel` and `~/.parsec`-style state, dlopens the versioned dylib by symbol `wx_main`, and formats an `app_block_hash=%s` string (hash check over the dylib block; exact algorithm not reversed, static observation only).

## 2. Toy matrix (all arm64-native; x86_64 builds compile, cannot run - no Rosetta; dyld policy is arch-independent)

Conventions: `T=/tmp/parsec-orbit-research/inject-toy`. `bin/app-plain` = linker-signed adhoc (flags `0x20002(adhoc,linker-signed)`). Hardened variants were re-signed ad-hoc with the runtime option enabled (+ entitlements plist as noted). Payload = `bin/payload.dylib` (adhoc ctor writing `marker-<pid>.txt`). Marker dir = `T` itself.

### Cell A - baseline: plain app + DYLD_INSERT_LIBRARIES (PASS, injected)

```
$ DYLD_INSERT_LIBRARIES=$T/bin/payload.dylib ./bin/app-plain
APP pid=66800 DYLD_INSERT_LIBRARIES=/tmp/parsec-orbit-research/inject-toy/bin/payload.dylib
APP environ_count=54
APP env: DYLD_INSERT_LIBRARIES=/tmp/parsec-orbit-research/inject-toy/bin/payload.dylib
APP target(41)=42
exit=0
$ ls marker-*.txt
marker-66800.txt   # ctor ran
```

### Cell B - hardened(runtime, no extra entitlements) + DYLD_INSERT_LIBRARIES (BLOCKED, env stripped)

```
$ DYLD_INSERT_LIBRARIES=$T/bin/payload.dylib ./bin/app-hard-none
APP pid=66811 DYLD_INSERT_LIBRARIES=(null)
APP environ_count=53        # one fewer: the DYLD_* var is gone, not just hidden
APP target(41)=42
exit=0
```
No marker file, no stderr. dyld4 silently strips `DYLD_INSERT_LIBRARIES` before main. Same result via direct exec of an app bundle binary and via `open --env` (see G).

### Cell C - hardened + BOTH entitlements (allow-dyld-env + disable-libval) + INSERT (PASS, injected)

```
$ DYLD_INSERT_LIBRARIES=$T/bin/payload.dylib ./bin/app-hard-both
APP pid=66812 DYLD_INSERT_LIBRARIES=/tmp/parsec-orbit-research/inject-toy/bin/payload.dylib
APP environ_count=54
APP env: DYLD_INSERT_LIBRARIES=/tmp/parsec-orbit-research/inject-toy/bin/payload.dylib
APP target(41)=42
exit=0
$ ls marker-*.txt   # marker-66812.txt present; ctor ran
```
`DYLD_PRINT_LIBRARIES=1` stderr contains `dyld[<pid>]: <7DA0530B-...> /private/tmp/parsec-orbit-research/inject-toy/bin/payload.dylib` (87 stderr lines total).

### Cell D - hardened + ONLY allow-dyld-environment-variables + adhoc dylib (env honored, LOAD SILENTLY REJECTED)

```
$ DYLD_INSERT_LIBRARIES=$T/bin/payload.dylib ./bin/app-hard-allow
APP pid=66916 DYLD_INSERT_LIBRARIES=/tmp/parsec-orbit-research/inject-toy/bin/payload.dylib
APP environ_count=55
APP env: DYLD_INSERT_LIBRARIES=/tmp/parsec-orbit-research/inject-toy/bin/payload.dylib
APP target(41)=42
exit=0
```
No marker file. **No dyld error text anywhere**: stderr (redirected to file) contains zero mentions of `payload` (`grep -i payload D-stderr.txt` exit=1); `DYLD_PRINT_LIBRARIES=1` lists 86 images with no payload line (vs 87 with the payload line in cell C). Exit 0. Library validation rejects the adhoc-signed (TeamIdentifier unset) dylib without a diagnostic on this OS.

### Cell E - hardened + ONLY disable-library-validation + INSERT (BLOCKED, env still stripped)

```
$ DYLD_INSERT_LIBRARIES=$T/bin/payload.dylib ./bin/app-hard-libval
APP pid=66870 DYLD_INSERT_LIBRARIES=(null)
APP environ_count=53
APP target(41)=42
exit=0
```
No marker. `disable-library-validation` alone does NOT re-enable dyld environment variables. Both entitlements are independently necessary for the INSERT vector (C vs D vs E).

### Cell F - dlopen() of a planted dylib from a user-writable dir into a hardened process

`dlopen` argv mode added to toy app. Planted files in `T/fakeparsec/`: `planted-unsigned.dylib` (signature removed), `planted-adhoc.dylib` (linker-signed adhoc), `planted-both.dylib` (adhoc + `--options runtime` + both entitlements).

- F0 baseline, plain app dlopen adhoc: `DLOPEN result=OK handle=0x6a5bd9e0`, exit=0.
- F1 hardened-none dlopen **unsigned**: `DLOPEN result=FAILED err=dlopen(...): tried: '.../planted-unsigned.dylib' (missing code signature in <7DA0530B-...> '...'), ...` (plus Cryptexes/no-such-file fallback lines), exit=1.
- F2 hardened-none dlopen **adhoc**: `DLOPEN result=FAILED err=dlopen(...): tried: '.../planted-adhoc.dylib' (code signature in <7DA0530B-...> '...' not valid for use in process: mapping process and mapped file (non-platform) have different Team IDs), ...`, exit=1.
- F3 hardened + disable-libval-only dlopen adhoc: `DLOPEN result=OK`, exit=0. (Library validation is the gate; the entitlement opens it.)
- F4 hardened-both dlopen adhoc: `DLOPEN result=OK`, exit=0.
- F5 hardened-none dlopen **adhoc+runtime both sides, no team**: `DLOPEN result=FAILED ... not valid for use in process: ... have different Team IDs`, exit=1. Matching flags/entitlements on both sides do NOT help: the check is team-identity based, and adhoc signatures carry no team.

Consequence: a planted `~/.parsec/parsecd-*.dylib` can never load into the genuine Parsec stub (Developer ID team Y9MY52XZDB vs anything we can sign; we hold no signing identity at all). The `app_block_hash=%s` string additionally suggests the stub hash-checks the dylib block (unproven dynamically).

### Cell G - `open` vs direct exec vs `launchctl setenv` (no bypass anywhere)

- Direct exec of hardened bundle binary with INSERT: `DYLD_INSERT_LIBRARIES=(null)`, no marker (same as B).
- `open -a ToyHard.app --env DYLD_INSERT_LIBRARIES=...` (adhoc hardened-runtime bundle): **no new marker** - LaunchServices/`open` does not bypass the strip.
- `open -a ToyApp.app --env DYLD_INSERT_LIBRARIES=...` (plain bundle, control): new `marker-67251.txt` - `--env` delivery itself works; the hardened strip is what blocks.
- `launchctl setenv DYLD_INSERT_LIBRARIES $T/bin/payload.dylib` then `open -a` both bundles: markers before=11, after=11 - **zero new markers even for the plain bundle**. launchd does not propagate `DYLD_*` to spawned GUI apps (plus a re-activation caveat: `open -a` may activate a live instance instead of exec; the hardened-negative is consistent with B regardless).

### Matrix summary

| Cell | App | Mechanism | Result |
|------|-----|-----------|--------|
| A | plain | DYLD_INSERT_LIBRARIES | INJECTED (marker, env visible) |
| B | hardened, no extra ents | DYLD_INSERT_LIBRARIES | STRIPPED (env null, no marker, exit 0) |
| C | hardened + both ents | DYLD_INSERT_LIBRARIES | INJECTED |
| D | hardened + allow-dyld-env only | DYLD_INSERT_LIBRARIES (adhoc dylib) | env honored, dylib SILENTLY REJECTED (no marker, no dyld error, exit 0) |
| E | hardened + disable-libval only | DYLD_INSERT_LIBRARIES | STRIPPED (ent alone insufficient) |
| F1/F2/F5 | hardened, no extra ents | dlopen unsigned/adhoc/both-sides-adhoc | FAILED (missing signature / different Team IDs, exit 1) |
| F3/F4 | hardened + disable-libval (only/both) | dlopen adhoc | OK |
| G | hardened | `open --env` / `launchctl setenv` + `open` | no bypass (null / no markers) |

## 3. arm64 code-patching / JIT feasibility (retour / table-patch plugins)

### patchtest (mprotect own __TEXT page RW, flip a byte, restore RX)

- Plain app: `PATCH mprotect(RW) rc=-1 errno=13 before=42` / `PATCH result=DENIED_AT_MPROTECT`.
- Hardened app: identical (`rc=-1 errno=13`, EACCES).
- arm64 `__TEXT` is mapped immutable even without hardening. In-place inline-hook patching (retour `RawDetour`) has no RW window to write through on this platform as-is.

### jittest (mmap RW, mprotect RX without MAP_JIT; mmap MAP_JIT)

- Plain: `JIT mprotect(RX-without-MAP_JIT) rc=0`, `mmap(MAP_JIT) OK`, `mprotect(RX-with-MAP_JIT) rc=0`.
- Hardened-none AND hardened-both: `mprotect(RX-without-MAP_JIT) rc=0` but `mmap(MAP_JIT) FAILED errno=22` (EINVAL - flag rejected without `allow-jit`).
- Hardened + `com.apple.security.cs.allow-jit` (extra probe): `mmap(MAP_JIT) OK`, both mprotects rc=0. So MAP_JIT is entitlement-gated as documented.

### jit-exec (actually CALL the fresh RX page; `mov w0,#42; ret`) - the decisive probe

- Plain: `JITEXEC result=42 => EXEC_NEW_RX_OK`, exit 0.
- Hardened (runtime, no allow-jit): **SIGKILL on first execution, zero output, exit=137**. Crash report `~/Library/Logs/DiagnosticReports/jit-exec-hard-*.ips`: `termination namespace CODESIGNING code 2 "Invalid Page"`, `exception EXC_BAD_ACCESS signal SIGKILL (Code Signature Invalid)`. So on arm64/hardened, a `mprotect(RX)` that "succeeds" still produces a non-executable page: WX/XN + code-signing enforcement kills the process at dispatch. (MAP_JIT+allow-jit *mmap* acceptance was verified; *executing* a MAP_JIT page under allow-jit was NOT tested - open item, expected-allowed per Apple JIT docs.)

Consequences for plugin designs: retour-style inline hooks need (1) a writable window onto target text (absent - EACCES even plain) and (2) executable trampolines (SIGKILLed under hardened without the JIT path). Table-pointer patching (high-bitrate-native style) needs a writable data page (works for own pages) but redirect targets still need executable memory. Neither works inside the genuine hardened Parsec process.

### retour crate aarch64-macOS support: NO

- Repo pins `retour = 0.3` (`crates/parsec-orbit-plugin-hooks/Cargo.toml:16`); lockfile = retour 0.3.1.
- retour 0.3.1 `src/arch/mod.rs` (verified from the published crate tarball, checksum-matched via Cargo.lock): `cfg_if! { if #[cfg(any(target_arch = "x86", target_arch = "x86_64"))] { mod x86; ... } else { // TODO: Implement ARM/AARCH64/MIPS support! } }` - on aarch64 there is no Patcher/Trampoline at all (downstream compile break, not just missing tests).
- Upstream master README "Platforms" CI list for macOS is only `x86_64-apple-darwin` (no `aarch64-apple-darwin`); master `src/arch/mod.rs` still carries the same x86-only gate.
- URLs: <https://github.com/Hpmason/retour-rs> (repo + README platform list), <https://raw.githubusercontent.com/Hpmason/retour-rs/master/src/arch/mod.rs> (arch gate + ARM TODO), <https://docs.rs/retour> (crate docs).

## 4. Options evaluated (ranked)

Criteria: (a) Parsec files unmodified, (b) survives pkg updates, (c) complexity, (d) security-policy risk. Context: user holds ZERO codesigning identities; fresh pkg install carries no quarantine flag (only `com.apple.provenance`); pkg preinstall does `rm -rf /Applications/Parsec.app ~/.parsec ...` on every update.

1. **Ad-hoc re-sign installed `/Applications/Parsec.app` adding `allow-dyld-environment-variables` + `disable-library-validation`** - LEAST-INVASIVE WORKING OPTION. Modifies: signature only (file contents identical). Then cell C applies: `DYLD_INSERT_LIBRARIES` works. (a) FAILS strictly (seal broken) but content-preserving. (b) No: every pkg update reinstalls + wipes, must re-apply (scriptable). (c) Low: a single re-sign step that adds the two dyld-related entitlements, plus a launcher that sets the dynamic-linker variable. (d) Re-signing drops notarization; without quarantine flag Gatekeeper does not re-challenge, but any quarantined copy would fail Gatekeeper under adhoc. **Biggest cost: profile-gated `persistent-content-capture` comes from Parsec's provisioning profile + team - an ad-hoc seal cannot carry it, so screen-capture entitlement is expected to stop working; must be behavior-tested, not assumed.**
2. **Re-sign with a user Developer ID (+ the two entitlements)** - same modifications as 1 plus own identity. Currently IMPOSSIBLE (0 identities; needs paid Apple Developer membership + cert issuance). Even then, notarization is lost and the foreign Team ID still cannot satisfy the profile-gated capture entitlement. Strictly worse than 1 unless distribution needs a named team.
3. **DYLD_INSERT via wrapper script / `launchctl setenv` WITHOUT re-signing** - DEAD. Proven: dyld strips INSERT for hardened processes regardless of launcher (cells B and G: direct exec, `open --env` and `launchctl` all negative). `disable-library-validation` alone does not re-enable env (cell E). No combination avoids touching Parsec's signature.
4. **Plant a versioned dylib in the user-writable skel location (`~/.parsec/parsecd-*.dylib`)** - DEAD. Proven F2/F5: library validation rejects anything not from Team Y9MY52XZDB ("different Team IDs", exit 1), and the `app_block_hash` string indicates an additional hash check. Only Parsec's private signing key could mint such a dylib.
5. **Frida-style dynamic attach (`task_for_pid`, exception ports, frida-agent inject)** - BLOCKED on stock Parsec: hardened runtime without `get-task-allow`/debugging entitlements denies `task_for_pid`; injecting the agent dylib hits the same library-validation wall (F2). Viable ONLY after option 1 (re-signed + disable-libval, then Frida-Gadget via INSERT works). Adds Frida toolchain + SIP-conscious complexity for no benefit over plain INSERT post-re-sign.
6. **Out-of-process companion (no injection at all)** - the only route in this list with (a) fully intact (see the addendum above for the loader-as-host alternative). A separate user-signed helper can use Accessibility/CGWindowList/ScreenCaptureKit (own consent prompts + entitlements) and localhost sockets. Covers config/overlay-style features; CANNOT replicate in-process plugins (retour hooks, native option-table patch, WebView2 CDP equivalent unless the client exposes a debug port - unknown, suggested probe for a follow-up round). Survives updates (b) YES, complexity medium-high, policy risk low.

Verdict table:

| Option | Parsec files intact (a) | Survives updates (b) | Complexity (c) | Policy risk (d) | Works? |
|--------|-------------------------|----------------------|----------------|-----------------|--------|
| No-touch INSERT / wrapper / launchctl | yes | yes | low | none | NO (proven B/E/G) |
| Plant user-writable skel dylib | yes | no (version-pinned name) | low | none | NO (proven F2/F5 + hash check) |
| Frida attach, stock binary | yes | yes | high | medium | NO (hardened denies) |
| Ad-hoc re-sign + both ents + INSERT | seal only | no (re-apply per update) | low | notarization + capture-entitlement loss | YES (proven C; capture TBD) |
| Dev-ID re-sign | seal only | no | low + cert needed (user has none) | same as above | YES if cert existed |
| Out-of-process companion | yes | yes | medium-high | low | PARTIAL (no in-process hooks) |

> **Addendum (same day, later round).** The ranked list above predates the
> loader-as-host evaluation: option 6 is *not* the only route that leaves the
> installed app fully intact. [loader-as-host.md](loader-as-host.md) describes
> a host that runs the vendor bootstrap from a re-signed *copy* of the stub
> while the installed app is untouched, and [smoke-test.md](smoke-test.md)
> confirms it end-to-end. Options 1-5 stand as written.

## 5. Open questions

1. Does an ad-hoc re-signed Parsec.app actually lose screen capture (profile-gated `persistent-content-capture`)? Behavior-test with a toy re-signed bundle requesting a profile entitlement - or inspect whether the stub spawns a helper that carries capture.
2. Executing a MAP_JIT page under `allow-jit` was not tested (only mmap-flag acceptance). If the project ever ships its own signed helper, that is the sanctioned RWX path.
3. x86_64 slice behavior untested (no Rosetta); dyld policy is arch-independent so risk is low, but Intel-Mac coverage is unverified.
4. Does the macOS client expose any remote-debugging port (WebKit inspector / `--remote-debugging-port` analog) enabling the webview-bridge plugin out-of-process? Suggested grep surface: `strings` on the skel dylib for `devtools`, `inspector`, `remote-debugging`, `CDP`.
5. Exact `app_block_hash` algorithm/inputs (which file bytes, which hash) - needed only if option 4 is ever revisited; currently moot.
6. `open -a` re-activation caveat: the launchctl-plain negative may partly reflect activating a live instance rather than a fresh exec; the hardened conclusion does not depend on it.

## 6. Raw-output archive

Full per-cell transcripts were captured inline above (exact commands + stdout/stderr + exit codes + marker-dir listings). Crash reports: `~/Library/Logs/DiagnosticReports/jit-exec-hard-2026-10-13-071*.ips` (CODESIGNING Invalid Page / SIGKILL Code Signature Invalid). Toy sources/binaries: `/tmp/parsec-orbit-research/inject-toy/` (`app.c` = fixed harness, `jit-exec.c` = exec probe, `ent-{both,allow-dyld,disable-libval,jit}.plist`, `bin/app-{plain,hard-none,hard-both,hard-allow,hard-libval,hard-jit}`, `bin/payload*.dylib`, `bin/jit-exec*`, `fakeparsec/planted-{unsigned,adhoc,both}.dylib`, `ToyApp.app`/`ToyHard.app` bundle stubs). File-level stderr captures: `/tmp/parsec-orbit-research/{C,D}-std{out,err}.txt`, `/tmp/parsec-orbit-research/F5-out.txt`, `/tmp/parsec-orbit-research/jithard-{out,err}.txt`.

---
Bottom line: **no zero-touch dylib vector exists** (no LC_RPATH/DYLD_ENV slot, hardened+validated+team-locked, hash-checked skel dylib). Within this document's option list, the least-invasive working route is a re-sign of the installed app adding both dyld entitlements, re-applied after each pkg update, with the `persistent-content-capture` entitlement loss as the top behavior risk. The fully-intact route is the loader-as-host design in [loader-as-host.md](loader-as-host.md) (confirmed in [smoke-test.md](smoke-test.md)).
