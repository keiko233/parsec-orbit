# M2 independent client and plugin checkpoint

Date: 2026-10-15. Host: macOS arm64. The maintainer has no dedicated test
account/remote endpoint and requested completion of independent code and
local verification first. This checkpoint records that scope; it does not
close the account/session, native bitrate or WebView exit gates.

## Implemented and measured

| Area | Result | Boundary |
| --- | --- | --- |
| Client service | Shared v1 ABI and Rust SDK callback binding (`35d20c9`); JSON reports actual mapped build/hash/token | Every build remains `not-verified` for internals-dependent plugins |
| Hooks | Classic already-bound imported-pointer backend (`56bc6b2`), enable/disable/remove, original callable after remove, exact page-protection restoration | No inline, unresolved-lazy or chained-fixup support; owned functions only in vendor processes |
| Config / Boa | macOS package manifests; set/get `{\"value\":42}`, JS `1 + 2 -> 3` | Retained HR + disable-LV only, no extra JIT exceptions |
| JS kind | Owned manifest-driven JS plugin logged `6 * 7 -> 42` | No WebView service required or inferred |
| Marketplace | Platform-filtered signed-index install and arm64 staged ad-hoc signing (`37dc0c7`) | Loopback HTTP integration, not remote publication |
| Client update | Isolated 101a→104a and 104a→105c paths observed, loader/plugin reinitialization | Client dylib update, not full-app updater concurrency proof |
| Original path | Explicit Launch Services original-path launch and exact restore passed | Administrator-assisted one-run rename experiment; see [signing checkpoint](signing-checkpoint.md) |

The official config and JS runtime `plugin.macos.toml` files are packaged as
`plugin.toml`; Windows manifests keep their existing meaning. The hooks Windows
backend is token-equivalent to the original implementation, apart from moving
it to a target module and retaining its public helper reexports.

A readiness regression discovered during the real update was fixed: current
clients can announce `Parsec release17 (...)`. The loader accepts only an empty
or up-to-four-digit release tag, the actual mapped build, and Loader 14 from
the validated current-launch log. It does not mark the new build verified.
The acceptance controller now snapshots log inode/EOF per attempt, discards
old partial lines, handles rotation/truncation, refuses linked/non-regular
logs, caps reads, and requires all current ready counts to have zero failures.
Old successful logs cannot conceal a new failed launch. Identity-helper
compilation failure refuses launch; success also requires current completion
and an observed on-screen window.

## Official-client evidence

Disposable copy: `/private/tmp/orbit-m1c-i30pesbu/Parsec.app`, independent
state under `orbit/home`. The 105c acceptance at
`acceptance-8a242071/acceptance-result.json` passed with PID 92011,
start identity `1791995787/384209`, a Parsec-owned window and
**7 loaded, 0 skipped, 0 failed**. Its current-launch log measured:

- `parsec-orbit.client` status `not-verified`, Loader 14, mapped 150-105c hash
  `26a0fe52fd701ee1a75f1f90bcc0b0eb678e6b133df37b7732b0d220388da0ea`.
- Owned hook values: before **8**, enabled **1007**, original **8**, disabled
  **8**, re-enabled **1007**, removed **8**; all service results zero.
- Fixture config and Boa results above, and manifest-driven JS kind **42**.

The same owned hook/config/Boa/JS scenarios passed under both 101a and 104a
in the original-path trial. These exercise the host environment, not vendor
functions or session behavior. Process stopping used PID, executable path
and start identity; no process-name kill was used. Real-account roots were
checked only with lstat and were unchanged; their contents/credentials were
not read. This is not a complete syscall audit. Formal uninstall of the
private copy restored the pristine tree/signature without warnings and kept
all Orbit evidence/data (`/tmp/orbit-m2-final-copy-uninstall.json`).

## UI and native bitrate: evidence, not support

The bounded main-thread Objective-C fixture inventories current NSApp window
view classes and loaded WebKit images; it retries an early empty view tree.
The signed-out client inventory measured one native Matoya
`__mty_app_view_0` view and no WKWebView. Loaded WebKit/JavaScriptCore libraries
do not establish a page, URL, DOM or bridge. The direct repeat had an internal
NSApp window but no on-screen CGWindowList entry, so that acceptance correctly
failed the window gate; the internal inventory is recorded separately.
No macOS WebView provider or high-bitrate WebView manifest is shipped.
Evidence: `ui-probe-result.json` and
`acceptance-2dc28ea9/acceptance-result.json` under the disposable scratch root.
The explicit Launch Services repeat at
`acceptance-d5c22292/acceptance-result.json` then passed with PID 13974,
a Parsec-owned on-screen window and 7 loaded / 0 skipped / 0 failed; the
inventory again observed the native view. Signed-in surfaces remain unverified.

`scripts/macos-inspect-bitrate.py --xrefs <client.dylib>` is bounded read-only
static tooling. For 104a (`eed68639...f0a6`) it found a table candidate at file
5940772, VM `0x27a624`; for 105c (`26a0fe52...0ea`) at file 6073228,
VM `0x282b8c`. Both are in `__TEXT,__const`, have twelve records with stride
`0x144`, numeric Mbps value at `+0x40`, and two adjacent ADRP/64-bit ADD
candidate references. The JSON explicitly says `verified: false`; these are
not runtime use-site validation. Current output: `/tmp/orbit-m2-bitrate-105c.json`.

`scripts/macos-text-const-canary.py` uses separately signed owned scratch
hosts, retained HR and only disable-LV; it never modifies a vendor file or
process. On a dedicated 16 KiB signed const-data page, plain `mprotect(RW)`
returned EACCES 13 and `vm_protect(RW)` returned protection failure 2, while
`vm_protect(RW|COPY)` allowed the owned byte change and restoration of the
original protection. This is synthetic data-only evidence, not vendor table
mutation. Evidence:
`/private/tmp/orbit-text-const-canary-cegcv2pk/artifact.json`.

A separate dedicated cold-function page contained `mov w0,#7; ret`, had not
been called, and was distinct from the active controller's page. The child
flushed preflight/before-protection events, then ended with SIGBUS during the
COPY-protection attempt, with no returned result captured. It did not write
instructions, restore RX or execute changed code. The precise cause remains
unresolved; no executable-memory feasibility is inferred. Evidence:
`/private/tmp/orbit-text-const-canary-1gh9c8ay/artifact.json`.

## Verification and remaining gates

On the final code, `cargo test --workspace --locked` passed (63 successful
suite blocks), as did workspace all-target checks, formatting, targeted
all-target Clippy `--no-deps -D warnings`, Python suites (8 bootstrap,
18 acceptance, 5 mocked Keychain, 6 static inspector), Python compilation,
and the pnpm build/test suite. Installer tests include 21 unit + 14 scratch
cases and exact metadata restoration; market has 52 unit cases and two HTTP
integrations. Hook tests include a real ad-hoc-signed HR owned host with late
images and symbol lifetime after closing the caller's handle.

Strict Clippy with dependency linting also encounters pre-existing core
warnings; SDK all-target linting has existing test warnings. Those are not
silently reported as green. Changed product libraries and the targeted
no-deps checks pass. The Windows installer/state/loader all-target cross-check
passes compile-only. Cross-building hooks/market on this host stops in the
Windows C dependencies (`libudis86-sys` missing `string.h`, ring missing
`assert.h`); Windows runtime behavior awaits the existing Windows CI job.
No CI run, push, PR or release was performed here.

Still required: a dedicated account and endpoint for login, connect,
disconnect/reconnect, input/audio; TCC/Keychain/camera/microphone and account
coexistence; actual Dock/Spotlight/URI entries; quarantined local-install
experience; runtime bitrate layout/menu/use-site/rollback evidence; a usable
WebView surface; full-app replacement/concurrent updater and disk-full
scenarios. End-user installation scripts remain deferred by the maintainer.
Do not promote 101a, 104a or 105c to `verified` from these measurements.
