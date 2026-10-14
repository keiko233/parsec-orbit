# Can an unmodified `Parsec.app` launch run the loader?

Historical scope: this round required an untouched vendor bundle/signature. The later [native dependency PoC](native-dependency-poc.md) permits patching and re-signing; [ADR 0014](../decisions/0014-macos-patched-bundle.md) is the current product direction. The negative verdict below applies only to the original constraint.

Question: when the user opens the official `/Applications/Parsec.app` from Finder, the Dock, Spotlight or a `parsec://` link, can Orbit's loader initialise in that launch, as `bcrypt.dll` does on Windows, with the vendor app left as it is?

Run: 2026-10-13, macOS 27.0 (26A428), arm64. Tags as in [macos-host.md](../../spec/macos-host.md#0-conventions). Measurements below used toy apps only (`ToyBG.app`: `LSBackgroundOnly`, ad-hoc, hardened runtime; `ToyFG.app`: same without the flag). The vendor app and its client were not started, signalled or written; the evidence of the earlier rounds is reused, not repeated ([loading-constraints.md](loading-constraints.md), [loader-as-host.md](loader-as-host.md), [smoke-test.md](smoke-test.md)).

## Verdict

**Not achievable without changing the vendor app's signature or weakening an OS protection.** No mechanism that keeps the vendor bundle, its signature and the OS protections intact runs Orbit code inside the process Launch Services starts for `Parsec.app`. The closest UX that keeps the invariants is to make every *normal* way of launching Parsec reach `Parsec Orbit.app` (entry-point substitution, option C), and to state plainly that opening the vendor app directly starts a plain client.

## 1. The launch path and where code could enter

Finder/Dock/Spotlight/`open` -> Launch Services -> `execve(/Applications/Parsec.app/Contents/MacOS/parsecd)` -> dyld -> `parsecd` (hardened, Developer ID `Y9MY52XZDB`) -> `dlopen` of the signed client dylib -> `wx_main`. Every point where foreign code could join, and why each is closed:

| Entry point | Result | Tag |
| --- | --- | --- |
| `DYLD_INSERT_LIBRARIES` (any delivery: shell, `open --env`, `launchctl setenv`, `LSEnvironment`) | stripped for hardened runtime without `allow-dyld-environment-variables`; `launchctl setenv` does not reach GUI apps at all | `[measured]` cells B, E, G |
| Foreign dylib via `dlopen` (planted client dylib, rpath/appdir slot) | refused by library validation (different Team IDs); no `LC_RPATH`/`LC_DYLD_ENVIRONMENT`; stub also checks SHA-256 | `[measured]` F2/F5, `otool` |
| App extension points in the bundle | none: bundle holds only `MacOS/parsecd`, `Resources/{skel,Parsec.icns}`, `embedded.provisionprofile`; `Info.plist` has no `NSExtension`, `NSAppleScriptEnabled` or XPC/PlugIns declarations | `[measured]` `find`, `defaults read` |
| Attach to the running process (`task_for_pid`, Frida) | denied for a hardened process without debugging entitlements | `[static]` (Apple hardened-runtime model); Frida row of [loading-constraints.md](loading-constraints.md) |
| Edit the vendor bundle (`Info.plist`, re-sign with the two dyld entitlements, wrapper executable) | bundle is `root:wheel`; any edit breaks the sealed signature and the notarization, re-signing drops the profile-gated entitlement, and macOS 13+ App Management protects other vendors' bundles from non-privileged writers; all contradict invariant 1 | `[measured]` owner/mode; `[static]` signature seal; App Management from forum reports, not an Apple document `[unverified]` |
| Endpoint Security `AUTH_EXEC` (deny the exec, start the host instead) | needs the restricted `com.apple.developer.endpoint-security.client` entitlement, granted by Apple on request (waits of months reported), a provisioning profile with SIP on, and a privileged system component; Orbit has no signing identity and invariant 6 forbids privileges | `[static]` developer-forum reports; `[measured]` 0 identities |
| Launch Services duplicate bundle id (host also `tv.parsec.www`) | LS resolves by path for Dock/Finder launches, so it would not redirect them, and it makes TCC/Dock state alias the vendor app (E7) | `[unverified]`, not pursued |

Accessibility / ScreenCaptureKit companions work on the unmodified process from outside, but cannot host the plugin ABI; they stay out of scope ([ADR 0011](../decisions/0011-macos-scope.md)).

## 2. Option B: watch for the vendor launch and redirect

An agent in the user's login session notices `tv.parsec.www` starting, ends it and opens the host. It needs no entitlement, but it does not hold up:

| Observation (toy apps, 8 `BG` and 3 `FG` cold-then-warm launches via `open`) | Result | Tag |
| --- | --- | --- |
| `NSWorkspace` will/did-launch notifications for an `LSBackgroundOnly` app | **never delivered**; the vendor app is `LSBackgroundOnly=1`, so the usual launch notification does not exist for it | `[measured]` (toy); vendor flag `[measured]` |
| the same notification for a foreground app | `WILL` arrived 29-35 ms **after** the app's `main` ran; `DID` not observed | `[measured]` |
| independent process-table poll (`proc_listallpids`, 2 ms period) | sees the new pid only 8-15 ms before `main` (warm), about 350 ms before on the first cold launch; polling every 2 ms is also a standing CPU cost | `[measured]` |

Consequences, all for the vendor stub rather than the toy:

* The stub has run before any practical redirect: it takes `flock` locks and rewrites `appdata.json` with a bumped crash `count` before `wx_main` `[static]`. Ending it leaves the real state touched and a crash count that is never cleared; the stub has a "Skel Crash" repair path `[static]` string; threshold unknown `[unverified]`. This breaks invariants 2 and 3 as written.
* The visible effect is a window or Dock icon appearing and vanishing, then a second client with a different, empty login (ADR 0007).
* Reliability is a race measured in milliseconds; a launch that loses the race leaves a plain client running.

Rejected as default. A purely *informing* watcher (offer "restart with Orbit" after the fact) is possible only through polling, since the notification does not exist, and has the same state side effects when the user accepts; not proposed.

## 3. Option C: make the normal entry points reach the host

The host app is already launchable like any app. What is missing is that the user's habitual routes lead to it. Each item is opt-in at install time, recorded in `host.json`, reversed by `uninstall.sh`, and touches only Orbit-owned files or the user's own preferences, never the vendor app.

| Entry point | Mechanism | Needs | Tag |
| --- | --- | --- | --- |
| Finder / Spotlight / Launchpad | `Parsec Orbit.app` in `~/Applications`; icon taken on the user's machine from the vendor app's `Parsec.icns` (read-only, not redistributed); display name that reads as Parsec's Orbit edition | nothing | `[unverified]` icon derivation and Launch Services icon cache (E17) |
| Dock | installer replaces the vendor Dock item with the host app in the user's `com.apple.dock` preferences and restarts the Dock; the vendor app stays installed | `--dock` | `[unverified]` (E18): a Dock restart is a visible, user-level change |
| `parsec://` links (invitations and web "connect" buttons) | register the host app as handler of the scheme the vendor app claims; the vendor entry stays registered | `--url-scheme` | `[unverified]` (E15): whether a handler change prompts, persists across LS rebuilds, and how the vendor client reacts when the host receives the URL |
| Login | login item for the host app via `SMAppService.mainApp` | `--login-item` | `[unverified]` (E19): needs a registering process, appears in System Settings > Login Items |

What remains plain, and is documented as such: opening `/Applications/Parsec.app` itself (Finder, Spotlight when it is picked), any third-party launcher that targets the vendor bundle id, and a Dock item the user did not let the installer replace.

## 4. Remaining gap against Windows, for the record

The Windows loader shares the user's signed-in state because it lives in the client. On macOS the host keeps its own state ([ADR 0007](../decisions/0007-macos-isolated-state.md), decided), so the first launch of the host needs a separate sign-in even when the entry points are substituted. This is the largest UX difference left after option C; it is a consequence of an accepted decision, not reopened here.

## 5. Evidence

Toy sources and the watcher are in the local research archive (`inject-toy/`-style scratch: `toy.m`, `watcher.m`, `ToyBG.app`, `ToyFG.app`). The watcher logs `WILL`/`DID` and a process-table poll with `CLOCK_REALTIME` nanoseconds; each toy writes its own `main` entry time to a marker file. Per-launch deltas (`main` minus detection, ms): `BG` poll 8.0-14.9 (8 runs, no notification in any); `FG` poll 9.2-13.0, `WILL` -28.8 to -35.4 (3 runs).

Sources for `[static]` claims that are not binary facts (community reports, not Apple documentation; read as such): [Endpoint Security entitlement threads](https://developer.apple.com/forums/thread/736042), [Endpoint Security under SIP](https://developer.apple.com/forums/thread/791996?page=2), [App Management / SystemPolicyAppBundles](https://developer.apple.com/forums/thread/742720), [Hardened runtime exceptions](https://developer.apple.com/documentation/security/hardened-runtime.md).
