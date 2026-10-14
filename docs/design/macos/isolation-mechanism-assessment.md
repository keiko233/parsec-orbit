# M1a isolation mechanism assessment

**Recommendation: keep the production constructor fail-closed at exit 78.**
The mechanism assessment now resolves to the constructor-installed custom
SBPL deny profile with an owned sentinel gate: it is the only measured
mechanism that denies the protected file *metadata* surface the host contract
requires. The supported App Sandbox alternative denies protected content from
process start but allows `stat`/`lstat`/`access(F_OK)`/`readlink`/`realpath`/
`listxattr` metadata on those paths and cannot be tightened with a nested
profile (measured below). Keychain behavior under the re-signed identity and
the Mach/IPC rule set are now measured with owned fixtures (below); what
remains is runtime observation of the vendor client itself, so vendor
execution stays disabled.

## Scope and boundary

The requirement is to keep the vendor stub/client from reading or writing real
Parsec state: the current user’s `.parsec` and `.parsec-persistent`, shared
Parsec state, and Library/Keychain data. The loader is added with
`LC_LOAD_DYLIB`; vendor instruction bytes and the client image remain intact.
Hardened Runtime retains only the measured disable-library-validation
exception. System-library reads and Parsec networking are allowed. This is a
bounded boundary around the vendor process, not hostile-code containment for
arbitrary native plugins; the product spec treats those plugins as trusted.

## Evidence

**Measured constructor sandbox PoC.** The disposable stub launched through
Finder/Launch Services with Hardened Runtime and disable-LV. Its loader
constructor applied an experimental sandbox that allowed ordinary reads and
networking, denied writes outside the scratch root, and denied reads of the
listed Parsec roots. Later builds redirected `fopen`/`stat` accesses for
`/Users/Shared/.parsec-persistent` to scratch and logged scratch status writes.
The mapper did not cover `open`/`openat`, directory and lock operations,
rename/unlink, Keychain, Library or IPC. An earlier successful run read the
real shared `devid.bin`; therefore the PoC did not satisfy the credential
boundary. The audit also records that sandboxing began in the loader
constructor, after dyld initialization. It does not show that dyld or system
libraries accessed a protected credential path before that point.

**SDK support warning.** The local macOS 27.0 SDK’s `sandbox.h` marks
`sandbox_init` “No longer supported” and directs developers to App Sandbox.
The local `sandbox_init(3)` manual further warns that its named profiles must
not be used by programs built with SDK 27.0 or later and that opting into them
will kill the process. This is a concrete support constraint for that API and
those profiles. It does not erase the measured PoC result or establish that
every constructor-installed sandbox mechanism is impossible. The preserved PoC
archive identifies the exact mechanism: the loader constructor called
`sandbox_init(profile, 0, &error)` with custom SBPL text compiled into the
image (not `SANDBOX_NAMED`): `(version 1)`, `(allow default)`, global `(deny
file-write*)` with allows for the PoC scratch root and `/dev/null`, and literal
`(deny file-read* (subpath ...))` rules for `~/.parsec`, `/Users/Shared/.parsec`
and `/Users/Shared/.parsec-persistent`. The man-page kill warning is scoped to
the `kSBXProfile*` named profiles; both the PoC and the scratch canary measured
custom profile strings with `flags=0` installing successfully on macOS 27.0
(26A428). The archive paths are author-local
(`/tmp/parsec-orbit-poc-20261013/native/*/{loader.c,sandbox.sb}`) and may
expire; the profile above is the durable record.

**Current production boundary.** The activation record/path checks are not a
kernel guard. NSS home replacement is not enabled in production, and the
constructor exits 78 before vendor main. The observed shared-state read and
partial `fopen`/`stat` mapper show why API redirection alone is insufficient;
they do not show that a kernel deny policy for those specific credential paths
cannot work. No evidence requires denying dyld’s ordinary system-library
reads. The relevant timing question is whether a protected-path operation can
occur before the loader constructor installs the deny policy or before the
client is mapped.

## Candidate constraints

### Scratch canary measurement (2026-10-13)

`python3 scripts/macos-isolation-canary.py` builds an owned arm64 main, a
strong guard dylib and an earlier dependency dylib, signs all three with
HR/disable-LV and strictly verifies them. Every probe targets a new private
canonical scratch directory; no vendor binary or real account path is used.
The fixture declares 66 expected probes and reports a partial result set as
unmeasured rather than inferring outcomes.

The guarded run (`/tmp/orbit-m1a-isolation-canary.json`, 66 probes) installs
custom `sandbox_init(profile, 0)` with `(allow default)`, `(deny file-read*
(subpath ...))`, `(deny file-write* (subpath ...))` and `(deny
network-outbound (remote unix-socket (subpath ...)))` for the canonical
scratch root, plus a literal `file-read*` deny on the guard's own sentinel
file. Measured: **36 EPERM denials, 23 successful operations, 6 dependent
probes skipped after their prerequisite open failed, and one `ELOOP` probe**
that fails identically in the unguarded control. Denials cover content and
metadata opens (`open`, `openat` absolute/dirfd/relative, `fopen` and
append/trunc, `stat`, `lstat`, `access`, `opendir`, `dir_open`, `realpath`,
`listxattr`, `getxattr`), mutations (`write_open`, `unlink`, `rename`,
`mkdir`, `rmdir`, `chmod`, `truncate`, `utimes`, symlink creation and
traversal, hardlinks in both directions), alias spellings (firmlink path and
the `/tmp` alias) and Unix-socket path removal, bind and — with the new
network clause — the previously allowed connect to a socket inside the denied
root. The successes are the allowed scratch operations, descriptors acquired
before policy installation (`fstat`/read/write/`flock`), `statfs`,
CoreFoundation `dlopen`, loopback TCP, and the owned Mach probes below. The
separate `--baseline` control run (`/tmp/orbit-m1a-isolation-baseline.json`)
recorded **65 successes and the same single `ELOOP`**, so every guarded EPERM
denial is attributable to the installed policy. The `open_nofollow_alias`
probe returns `ELOOP` in both runs because its final component is a symlink
opened with `O_NOFOLLOW`; it is neither a denial nor allowed content access.
The initial fixture used an uncanonicalized `/var` alias and reversed rule
order and observed no denials; it cannot establish API ineffectiveness. Which
of those two corrections caused that difference has not been isolated.

**Mach and unix-socket rules (2026-10-13).** An unsandboxed helper publishes
two owned `CFMessagePort` names; the guarded client looks up and messages the
first and looks up the second through `bootstrap_look_up`, plus one standard
system service. Under the path/socket profile (`--ipc none`) all four
succeeded, as in the unguarded baseline. Under `--ipc allowlist`, which adds
`(deny mach-lookup)` and `(allow mach-lookup (global-name "<owned-a>"))`, the
allowed owned name still looked up and messaged successfully while the second
owned name and `com.apple.cfprefsd.agent` both failed with kern_return 1100
(`BOOTSTRAP_NOT_PRIVILEGED`). Artifact:
`/tmp/orbit-m1a-mach-allowlist.json`. The allowlist form therefore works for
an owned service, and a blanket deny is visibly harmful to standard system
lookups; the vendor client's required service list is not enumerable without
running it, so the shipped profile keeps `(allow default)` for Mach and
records this as a runtime-observation item.

The fixture also measures installation behavior. The sentinel gate recorded
`sandbox_init_rc=0`, a post-install sentinel open failing with `EPERM` and
`gate_verified=true`; the constructor exits 92/93 before main on prerequisite
or API failure and 94 when the post-install probe does not prove the policy is
active. The constructor-order log shows the **earlier dependency constructor
read a protected scratch file successfully before the guard constructor
installed the policy** (`early_before_guard=true`,
`pre_guard_open_succeeded=true`). The policy therefore protects nothing during
dyld initialization before the loader constructor runs, and the guarded run
deliberately opened two descriptors before the policy so their retained
authority could be measured. Denied `opendir`/opens left the dependent
`readdir`, `write`, `flock`, `write_through_symlink`,
`flock_through_symlink` and `fchdir` probes skipped. Keychain, Mach/XPC policy
and real vendor behavior remain unmeasured under this profile. These findings
require authority and IPC coverage beyond this path-only profile. They
establish the measured mechanism capabilities, not a supported production API
or full isolation.

**Constructor-installed path deny policy.** It fits the no-companion and
Hardened Runtime constraints in the measured PoC, and can leave system reads
and networking available. Production acceptance depends on the exact API being
supported for the shipped SDK/OS, applying the deny policy before Orbit maps
or loads the client, and proving the protected roots are denied across the
observed file operations. The existing shared `devid.bin` access also needs a
private replacement or a demonstrated safe failure path. Do not describe the
POC’s partial mapper as complete isolation.

**App Sandbox.** Apple documents this as kernel-enforced and entitlement-based,
so it is a viable candidate to evaluate, not a universal solution or an
established incompatibility. Its default container boundary could deny direct
access to the real home and shared paths from process start while preserving
the single Hardened Runtime exception. It adds App Sandbox entitlements to the
re-signed app; those are not additional Hardened Runtime exceptions. The
current Orbit root at `~/Library/Application Support/ParsecOrbit` is outside
the default app container and would need relocation or additional file-access
entitlements. The client’s observed shared `devid.bin` read would be denied
unless redirected. Keychain access uses separate macOS mechanisms (file-based
item ACLs and data-protection access groups), and the Parsec API/identity
behavior is unmeasured. Required Mach/XPC/other local IPC services are likewise
not inventoried. App Sandbox therefore needs a focused compatibility and
negative-access measurement before adoption.

The owned `scripts/macos-app-sandbox-canary.py --run` fixture was extended to
40 probes and measured on macOS 27.0 after strict verification. It uses a
unique app id, HR/disable-LV, App Sandbox and network-client capabilities, and
a read/write absolute-path exception for only its own allowed scratch
directory. It executes no vendor code or Keychain API; macOS may create only
that new fixture's app container. The artifact
`/private/tmp/orbit-as-t5odcvsd/app-sandbox-result.json` records **17 EPERM
denials, 19 successes, 3 unmeasured probes and one `ELOOP`** (the same
`O_NOFOLLOW`-on-symlink artifact as the custom-profile fixture).

Denied operations include protected content opens from the main-image
constructor and from main, traversal through an allowed-directory symlink,
the firmlink spelling, protected `opendir`, `access(R/W)`, the xattr value
read, `openat("..")` and `chdir`-relative escapes, hardlinks in both
directions, a child process reading protected content, a self re-exec reading
protected content, a new bundle write, a Unix-socket connect to the protected
path and a Mach lookup for the fixture's own `CFMessagePort` name published by
its unsandboxed helper. Allowed operations include the allowed content
spellings, `stat`/`lstat`, `access(F_OK)`, `readlink`, `statfs`, `listxattr`
names, `realpath`, allowed child reads, Unix-socket creation in an allowed
directory, the `sandbox_check` symbol, and harness-inherited descriptors —
the artifact notes those `pass_fds` descriptors cannot occur in a real Finder
launch, so they measure descriptor reuse rather than launch-time isolation.
`readdir` after the denied `opendir`, `sandbox_check_matches` (the raw
best-effort table shows filter type 1 matching `file-read-data` and
`file-write-data`; the call's success semantics are undocumented, so no
enforcement conclusion is drawn) and `mach_send` (lookup failed first)
remain unmeasured. This verifies enforcement from process start for content,
several metadata operations, aliases, child/re-exec and one owned Mach lookup,
but **`stat`/`lstat`/`access(F_OK)`/`readlink`/`realpath`/`listxattr` on the
protected path still succeed**, so App Sandbox does not meet the host
contract's metadata-denial requirement.

**Nested-layer measurement (2026-10-13).**
`scripts/macos-nested-sandbox-canary.py` signed an owned scratch app with the
App Sandbox entitlement and called `sandbox_init(custom SBPL, 0)` from a
constructor: the call returned `-1` with the error text `Operation` and
changed no policy — protected content opens stayed denied, `stat`/`lstat`
stayed allowed, and the temporary-exception path and an owned `dlopen` payload
kept working. This matches the SDK header's statement that a second
`sandbox_init` is ignored and returns an error. The App Sandbox metadata
allowance therefore cannot be tightened with a nested custom profile; file
metadata denial needs either the unsupported constructor profile or loader
interposition. Artifact: `/private/tmp/orbit-nested-9779w8u9/nested-sandbox-result.json`.

The [static client import inventory](client-isolation-imports.md) identifies
file/directory/lock/socket and Mach-adjacent surfaces in build 150-101a. It
found no direct SecItem/SecKeychain/SecTrust imports but did find Security.framework
and dynamic resolution. That absence cannot close the Keychain boundary. The
[Keychain canary](keychain-canary-plan.md) then measured two clients in two
separate, narrowly scoped transactions: the earlier App Sandbox client, and a
2026-10-13 custom-SBPL client that installed the shipped deny profile and
verified its sentinel gate before probing. Both clients returned
`errSecAuthFailed` (`-25293`) for both fake items, including the ones that
explicitly trusted them. Every item was deleted afterwards and exact controller
queries returned `errSecItemNotFound` (`-25300`). This measures failure to read
those legacy items for both candidate clients; it does not distinguish sandbox
enforcement from signing/ACL identity or other authorization conditions, and
cannot close the data-protection Keychain or real vendor boundary.

**Endpoint Security.** The local SDK exposes authorization events for exec,
file opens, XPC connections, UIPC connects and, on macOS 27, some bootstrap
lookups. The system-wide client requires the Endpoint Security entitlement,
root and TCC approval; a system extension is a deployment route rather than
a requirement for every client. The SDK's macOS 27 descendants client explicitly
requires the entitlement but neither root nor TCC approval. However, that
client receives only notifications for its own process, and authorization
events for descendants. An ES client inside the loader therefore cannot use
that API to deny its own vendor process's accesses. Bootstrap-lookup
authorization is unavailable to system-wide clients. External enforcement
would need a separate controller, conflicting with the current no-companion
shape, and the exposed events do not establish general per-message Mach IPC
coverage. These are scoped constraints, not universal impossibility.

## Mechanism resolution (2026-10-13)

The two measured candidates fork on one decision:

| Requirement (host contract) | Constructor custom SBPL (measured) | App Sandbox (measured) |
| --- | --- | --- |
| Protected content open, incl. symlink/dirfd/relative/firmlink/`/tmp` alias | denied (EPERM) | denied (EPERM) |
| Protected metadata: `stat`/`lstat`/`access`/`opendir`/`realpath`/xattr | denied (EPERM) | `stat`/`lstat`/`access(F_OK)`/`readlink`/`realpath`/`listxattr` **allowed**; `access(R/W)`, `opendir`, `getxattr` denied |
| Protected mutations: rename/unlink/mkdir/rmdir/chmod/truncate/utimes/hardlink | denied (EPERM) | hardlink and bundle writes denied; most mutations not probed |
| Alias and escape spellings (dirfd, `..`, `chdir`, firmlink, `/tmp`) | denied (EPERM) | denied (EPERM) |
| Enforcement start | after constructor install; an earlier dependency constructor read a protected scratch file before it | from process start, including the main-image constructor |
| Pre-policy descriptors | retain full authority (measured) | harness-`pass_fds` descriptors retained read/write/lock (not launch-representative) |
| Unix socket at protected path | connect/unlink/bind denied with the measured `network-outbound` clause; create/bind-socket in an allowed dir still succeed | connect denied; create in allowed dir allowed |
| Mach lookup | allow-default profile keeps owned and system lookups (measured); the allowlist form denies non-listed names, including a standard system service (`kern_return` 1100) | denied (owned `CFMessagePort` name, unsandboxed publisher) |
| Legacy login-Keychain item owned by the controller, explicit client trust | `-25293` for both probes; sentinel gate verified (measured 2026-10-13) | `-25293` for both probes (measured) |
| Tightening | n/a (already the deny layer) | nested custom SBPL refused (`-1`, no policy change) |
| Support status | deprecated/unsupported API; "No longer supported" in the SDK header; custom `flags=0` strings measured working on macOS 27.0 (26A428) | supported, entitlement-based |
| Fail-closed gate | sentinel literal deny + post-install probe; constructor exits before main when unverified (measured) | kernel/entitlement enforced; no gate needed |

**Decision.** The constructor-installed custom SBPL profile with the owned
sentinel gate is the only measured mechanism that denies the protected
metadata surface the host contract requires, so it is the selected direction
for `crates/parsec-orbit-loader-macos/src/isolation.rs`. Its unsupported-API
risk is handled fail-closed: every failure to install or prove the policy
exits the process before main. App Sandbox remains a supported fallback but
cannot satisfy the metadata requirement and cannot be tightened with a nested
profile (both measured). The production constructor therefore keeps exiting 78
while the remaining runtime-observation items below stay unmeasured.

**Open items before any vendor execution.**

1. **Keychain behavior under the re-signed custom-profile client — measured
   (2026-10-13).** Two UUID-owned fake legacy login-Keychain items were added
   and deleted by the controller and probed by a disposable custom-SBPL client
   with the sentinel gate verified. Both probes returned `-25293`; both items
   were confirmed absent by exact controller queries. The remaining gap is
   data-protection Keychain and real vendor items, which stay unmeasured.
2. **Mach/IPC rule set — measured with owned endpoints (2026-10-13).** The
   allowlist form works for an owned `CFMessagePort` name; a blanket
   `mach-lookup` deny also blocks a standard system service, so the shipped
   profile keeps `(allow default)` for Mach. Enumerating the vendor client's
   required bootstrap services is a runtime-observation item that only a
   controlled client run can close.
3. **Shared `.parsec-persistent` redirection — implemented and
   fixture-tested.** `redirect.rs` interposes the nine measured path-taking
   APIs and rewrites the shared prefix to
   `<Orbit root>/home/.parsec-persistent` only at component boundaries and
   without `..` components; everything else falls through to the real path,
   where the profile denies it. Unit tests and the owned `redirect_e2e`
   fixture pass; the redirect has not yet run against the vendor client.

The remaining runtime-observation items close only through a controlled
disposable-copy client run (the M1a acceptance gate), after the runtime wiring
in the [bootstrap checkpoint](bootstrap-checkpoint.md) is connected.
Scratch-only measurements may continue without new authorization. Any new
root, Keychain mutation or actual-account operation needs its own narrowly
described approval, literal deny rules for the real protected roots stay in
the fixture exercises, and no syscall against those roots is permitted.

## Sources

- Repository evidence: [native dependency PoC](native-dependency-poc.md),
  [bootstrap checkpoint](bootstrap-checkpoint.md), and
  [host contract](../../spec/macos-host.md).
- Local macOS 27.0 SDK: `MacOSX.sdk/usr/include/sandbox.h` and
  `sandbox_init(3)`; `EndpointSecurity/ESTypes.h` records event availability
  and the descendants restriction.
- Apple: [Configuring the macOS App Sandbox](https://developer.apple.com/documentation/xcode/configuring-the-macos-app-sandbox),
  [Keychain access groups](https://developer.apple.com/documentation/security/sharing-access-to-keychain-items-among-a-collection-of-apps),
  [Endpoint Security](https://developer.apple.com/documentation/endpointsecurity),
  and [monitoring system events](https://developer.apple.com/documentation/endpointsecurity/monitoring-system-events-with-endpoint-security).
