# M1a client isolation import inventory

Date: 2026-10-13. Scope: static arm64 inspection of the authorized disposable
`OrbitBootstrapAudit.app` only. This records imported symbols and selected
path/API strings; it is not a trace of executed client behavior. The current
[bootstrap checkpoint](bootstrap-checkpoint.md) keeps the selected production
constructor fail-closed at exit 78 before vendor main. The [host contract](../../spec/macos-host.md)
and [isolation assessment](isolation-mechanism-assessment.md) remain controlling.

## Inspected images

Bundle: `/private/var/folders/60/bw9jrnw93r50427l2yrhhknc0000gn/T/orbit-m1a-audit-6s1esvyg/OrbitBootstrapAudit.app`
(bundle id `dev.parsec-orbit.bootstrap-audit.orbit-m1a-audit-6s1esvyg`,
version `150.101.1`, `CFBundleExecutable=parsecd`, `LSBackgroundOnly=true`).
Both images declare arm64 and were inspected with `nm -arch arm64 -u`,
`otool -arch arm64 -L/-l`, `strings -arch arm64` filtered to the families below,
`codesign -dvvv`, `file`, `stat`, and SHA-256. The client remains universal;
only its arm64 slice was inventoried.

| Image | Bytes | SHA-256 | arm64 UUID | Signature metadata |
| --- | ---: | --- | --- | --- |
| `Contents/MacOS/parsecd` | 115,568 | `a0792cace79d6bed52c202183fbc6245becf15fa2b90af8987810877068d8671` | `DDE4181F-6C2B-3CC0-9DCD-7EDF7D2A86A5` | ad-hoc, Hardened Runtime, runtime 15.5; no Team ID |
| `Contents/Resources/skel/parsecd-150-101a.dylib` | 6,235,664 | `9cce85059c7f95f1a8e578b3c9f7200cb364520e557b5c85294802c8c596cdd5` | `0B3F48A5-E7DB-373C-A00C-8E73D27DCACD` | `parsecd-150-101a`, Team `Y9MY52XZDB`, Hardened Runtime, runtime 15.5 |

`Contents/Resources/skel/appdata.json` names `parsecd-150-101a.dylib`, entry
`wx_main`, and the same client SHA-256 shown above. The stub SHA-256 identifies
this patched audit copy, not a pristine vendor stub. Both load commands report
minimum macOS 11.0 and SDK 15.5. The stub directly loads
`@executable_path/../Frameworks/libparsec_orbit_loader.dylib`.

## Direct undefined imports

Counts below are the total arm64 `nm -u` entries per image (including ObjC
runtime and graphics symbols); the listed names are the relevant direct
undefined imports. They show link-time references, not call sites, call
frequency, argument paths, or successful operations.

| Family | Stub `parsecd` | Client `parsecd-150-101a.dylib` | Isolation meaning |
| --- | --- | --- | --- |
| Total undefined imports | 246 | 619 | Inventory scope; not a count of file/IPC operations. |
| File and directory | `_access`, `_fopen`, `_mkdir`, `_open`, `_opendir`, `_readdir`, `_realpath$DARWIN_EXTSN`, `_rename`, `_stat` | Same set | Both images can reach pathname and directory APIs directly. These imports alone do not identify which paths each call uses. |
| Locks and process image | `_flock`, `_execv` | `_fcntl`, `_flock`, `_execv` | Client can use file locks and re-exec; stub uses `flock` and `execv`. Lock and update paths need runtime coverage. |
| Dynamic resolution | `_dlopen`, `_dlsym` | `_dlopen`, `_dlsym` | Additional symbols/framework functions may be resolved dynamically; this import list cannot enumerate those targets. |
| Sockets / message transport | No matching socket, bind, connect, send, or receive imports in the filtered list | `_socket`, `_bind`, `_connect`, `_send`, `_sendmsg`, `_sendto`, `_recv`, `_recvfrom`, `_recvmsg` | Client has direct network and message-socket entry points. A path-only filesystem policy does not constrain these channels. |
| Mach / Apple event adjacent | No matching Mach/bootstrap imports in the filtered list | `_bootstrap_port`, `_mach_task_self_`, `_AESendMessage` | Client has direct references relevant to Mach port and Apple Event interaction. No XPC symbol matched the inspected imports. The symbols do not establish a particular service or message. |
| Preferences / Keychain | No matching `_CFPreferences*`, `_SecItem*`, `_SecKeychain*`, or `_SecTrust*` undefined imports | No matching `_CFPreferences*`, `_SecItem*`, `_SecKeychain*`, or `_SecTrust*` undefined imports | This is not evidence that framework-mediated or dynamically resolved access is absent. The client links Security.framework. |
| User identity | No `_NSUserName` import in the filtered list | `_NSUserName` | Client can query the account name directly; home-path replacement alone does not hide every identity surface. |

Other linked client frameworks include Security, Foundation, CoreFoundation,
SystemConfiguration, WebKit, and networking/media frameworks. The stub links
Foundation/CoreFoundation and system frameworks. `otool -L` reports linked
images, not which APIs are called or the paths and records they access.

## Selected static strings and named state

The stub contains these exact path fragments/names: `.parsec`, `/Users/Shared`,
`/Applications/%s.app/Contents/Resources/skel`, `skel`, `appdata.json`,
`setup.json`, `useProgramData`, `config.txt`, and `lock_client`. The client
contains `.parsec`, `.parsec-persistent`, `/Users/Shared`, `setup.json`,
`useProgramData`, `status.json`, `user.bin`, `devid.bin`, `config.json`, and
`config.txt`. It also contains
`/System/Library/CoreServices/SystemVersion.plist`, an OS metadata path rather
than a Parsec account-state path.

These strings support treating the following as protected path families in
the host contract: current-user `.parsec` and `.parsec-persistent`, shared
`/Users/Shared/.parsec` and `/Users/Shared/.parsec-persistent`, and the client
state leaves named above beneath the selected state root. The strings do not
prove every leaf is used in this build, nor do they expose every path assembled
at runtime. No `Library/Preferences`, Keychain path, XPC service name, or
specific Mach service name appeared in this filtered string inventory.

## Required isolation and evidence boundary

1. Keep the selected M1a host fail-closed before client execution until a
   supported guard is validated. NSS home substitution and lexical installation
   record checks do not provide kernel-enforced isolation.
2. Deny reads and writes to both per-user and shared Parsec roots, including
   access through `open`/`openat`, stdio, metadata, directory enumeration,
   rename/unlink, locks, and symlink or alias resolution. Include paths reached
   before and during client mapping and self-exec. The existing partial
   `fopen`/`stat` mapper does not cover these imports.
3. Establish and measure Keychain/Security, CFPreferences/Library, Mach/XPC,
   Apple Event, Unix-domain socket, and other IPC boundaries. Missing direct
   imports or named strings cannot close these boundaries; framework calls,
   Objective-C dispatch, `dlopen`/`dlsym`, and indirect system-library behavior
   remain possible.
4. Use a bounded, loss-checked runtime audit on an explicitly safe disposable
   copy and private state before making access claims. A file trace alone does
   not prove Keychain or IPC isolation, and an observed absence in a trace is
   limited to the captured interval and event classes.
5. Preserve the host requirements for a strong loader dependency, supported
   bundle binding, private Orbit state, and no fallback to real Parsec state.
   A missing loader or guard failure must not let the vendor client continue.

Static import presence identifies candidate API surfaces, not behavior.
Static absence is weaker still: stripped call paths, Objective-C/framework
dispatch, dynamically loaded code, aliases, and kernel-mediated access may not
produce a named import or path string here. This inventory does not establish
complete isolation, client launch safety, or M1a completion.
