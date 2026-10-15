# Restructure hand-off

State of the work on branch `refactor/restructure` at the end of the macOS
session, and what a Windows session has to do next. The design is
[restructure.md](restructure.md); this file says what is done, what is not,
and how to continue.

Everything below was built and tested on macOS arm64 only. The branch is not
pushed. Commits are Conventional Commits without attribution trailers.

## 1. Get going on Windows

```powershell
git fetch <this repository or the macOS machine's path> refactor/restructure
git worktree add ..\parsec-orbit-restructure refactor/restructure   # or check the branch out
cd ..\parsec-orbit-restructure
pnpm install --frozen-lockfile
cargo build --release --locked          # the whole workspace, as package.ps1 does
cargo test --workspace --locked         # the full suite incl. the Windows-only e2e
pnpm -r build; pnpm -r test
```

Per [workflow.md](../development/workflow.md#worktrees-and-resource-reuse):
work in a worktree, seed `target/` by a verified ReFS block clone if the volume
supports it, otherwise build independently.

## 2. Status

| # | Phase | State | Evidence (macOS) |
| --- | --- | --- | --- |
| 1 | Classify | **done** | `libs/`, `plugins/`, `tools/`; `sdk-std` dissolved ([ADR 0017](decisions/0017-one-sdk-package-no-features.md)); `devtools` into the bridge; `verify-*` merged; roles ([ADR 0018](decisions/0018-crate-roles-and-layout.md)); dependency-law test |
| 2 | Layout and core | **done** | `parsec-orbit-layout`, `core::log::format_line`, `core::Session` ([ADR 0019](decisions/0019-layout-crate-and-shared-loader-glue.md)); `core::staging::promote` ([ADR 0023](decisions/0023-staged-update-promotion.md)) |
| 3 | Registry and release tool | **done** except cutover wiring | `parsec-orbit-registry`, `parsec-orbit-release`, `manifest/` ([ADR 0022](decisions/0022-official-registry-in-manifest.md)), `trusted-keys.toml`, `host.json` |
| 4 | Installer | **not started** (only moved to `tools/`) | see section 4 |
| 5 | Market plugin | **done** on macOS with fakes | `plugins/parsec-orbit-plugin-market` ([ADR 0020](decisions/0020-market-is-a-plugin-over-registry.md)) |
| 6 | `no_std` | `high-bitrate-webview` **done**; `high-bitrate-native` not started | 302,208 B to 50,416 B on macOS ([size report](size-report.md)) |
| 7 | Cutover (0.2.0) | **not started** | section 5 |

Checks that passed at the last commit: `cargo fmt --all -- --check`;
`cargo test --workspace --locked` (647 passed, 0 failed, 1 ignored, with
`PARSEC_ORBIT_REQUIRE_NODE=1`); `pnpm -r build && pnpm -r test`; the macOS
script tests CI runs (`scripts/test-*.py`, `scripts/test-install-macos.sh`);
`cargo check --target x86_64-pc-windows-msvc` for every crate except
`parsec-orbit-registry`, `-lib-hooks`, `-lib-ui`, `-plugin-market` and
`-release` (their dependencies `ring`, `libudis86-sys`, `winio` need a C
toolchain or a Windows host to compile).

## 3. What a Windows session must verify first

These changed behaviour or code that macOS could not run. Do them before
anything new, in this order.

1. **Build and full test run** (commands in section 1). Expect everything that
   was green before; the Windows-only e2e (`tests/parsec-orbit-e2e/tests/loader.rs`,
   `sdk.rs`, `js.rs`, `forwarding.rs`) is the real check of the loader changes.
2. **One `ready:` line.** The loader no longer prints it twice: `Session::scan`
   in `core` logs `ready: N loaded, M skipped, K failed` and the Windows worker
   no longer does. Confirm exactly one such line per start in
   `%APPDATA%\ParsecOrbit\parsec-orbit.log` and in the e2e assertions.
3. **Config on Windows.** `crates/parsec-orbit-loader-windows/src/config.rs`
   now builds on `parsec-orbit-layout` (the Windows path rules and their tests
   moved there and pass on macOS). Check the plugin directory order
   (`PARSEC_ORBIT_PLUGIN_DIR`, `<client>\plugins`, `%APPDATA%\ParsecOrbit\plugins`)
   and the log location on a real client.
4. **`bcrypt.dll` size.** Record the size against the last measured
   374,272 B (381,952 B with the console) in
   [size-report.md](size-report.md). The restructure added `layout`,
   `format_line`, `Session` and `staging::promote` to the loader; the budget is
   already exceeded, so note by how much and why. ADR 0004 / the `toml` parser
   is **not** to be replaced or trimmed (owner's decision).
5. **`staging::promote` in a real client.** Put a plugin folder under
   `%APPDATA%\ParsecOrbit\pending\<id>\` (with a `plugin.toml`) while an older
   copy exists in `%APPDATA%\ParsecOrbit\plugins\<id>\`, start Parsec, and check
   the log line `pending update of <id> is now installed` and that the new copy
   loaded. Also with the old DLL's folder open in Explorer (rename must fail,
   the old version must keep loading, the log must say so).
6. **Crates that could not be type-checked from macOS.** Run
   `cargo check --workspace --all-targets` and the tests on Windows for
   `libs/parsec-orbit-lib-ui` (two `std::thread::Builder` call sites replaced
   `ContextExt::spawn`), `libs/parsec-orbit-lib-hooks`, and
   `plugins/parsec-orbit-plugin-stun-swap` (the panic hook is now inlined with
   `std::panic::set_hook`; check that a panic still reaches the loader log).
7. **`high-bitrate-webview` as `no_std`.** Build the release DLL, record its
   size (macOS: 50,416 B; Windows estimate 15 to 20 KB), check it exports only
   the three ABI functions and has no imports beyond `kernel32`
   (`dumpbin /imports`), and in a client that `PARSEC_ORBIT_PATCHES=0`,
   `off`, `false` still switch it off while unset/empty/other values leave it
   on. The `GetEnvironmentVariableW` path is type-checked only.
8. **`scripts/verify.ps1`** builds `-p parsec-orbit-loader-windows --features
   diagnostics` and runs `callpaths.exe` / `roundtrip.exe`, which now come from
   the package `parsec-orbit-verify` (bin names unchanged). On macOS the
   release build of `roundtrip` fails on the x86 inline assembly; that was
   already so before this work.
9. **Packaging with the old scripts.** `scripts/package.ps1` and
   `scripts/install.ps1` were only updated textually (package names, paths,
   DLL names `parsec_orbit_lib_*`). Run `scripts\package.ps1`, check the zips
   and `dist\index.json`. Note that `install.ps1` installs into
   `plugins\<package name>\` while the market installs into `plugins\<id>\`:
   an old install plus a new one leaves two folders with one id and the loader
   skips the second as a duplicate (migration is a phase 4 item).

## 4. Work still to do (implementation order)

### 4.1 Installer (phase 4, [restructure.md 5.4](restructure.md#54-installer))

Create `tools/parsec-orbit-installer` (bin `orbit`) with `src/common/` and
`src/os/{windows.rs,macos/}`. Today only `tools/parsec-orbit-installer-macos`
exists, moved unchanged (it keeps its `parsec-orbit-installer-macos` binary,
which `scripts/install-macos.sh` and `package-macos.py` use).

* `common/`: registry sources and cache (use `parsec_orbit_registry::{sources,
  cache, host, install}`), selection from `index.json` (`default`, `required`,
  `group`), `install-state.json` format 2 (snake_case; read format 1),
  `market.json` gets `format`, plan/dry-run.
* `os/windows.rs`: port `scripts/orbit-manager.ps1` and `scripts/install.ps1`
  and `set-web-ui.ps1`: elevation check, stop service and `parsecd`, back up
  `bcrypt.dll` once to `%APPDATA%\ParsecOrbit\backups`, copy the loader with
  retries, install plugins, `client_web_ui` toggle that keeps the order and the
  rest of Parsec's `config.json`, restart, `ui` (post the registered window
  message `PARSEC_ORBIT_UI_TOGGLE` to class `ParsecOrbitUiActivation`),
  uninstall (restore the backup; a missing backup is a hard error). Migrate
  legacy plugin folders `plugins\parsec-orbit-plugin-*\` / `parsec-orbit-lib-*`
  without `market.json` whose manifest id is official into `plugins\<id>\`.
* `os/macos/`: move the code of `tools/parsec-orbit-installer-macos` here
  (apply, macho, journal, lock, privilege, ...) with its tests, keeping its
  records (`manifest.json`, `journal.jsonl`) untouched.
* Commands: `install`, `upgrade`, `uninstall`, `status`, `plugins
  list|install|remove|update`, `ui`, `logs [--follow]` (use
  `core::console::Follower`), `webview-ui on|off|keep`.
* Bootstrap `get-orbit.ps1` / `get-orbit.sh` (< 60 lines, download
  `orbit-<platform>`, check `SHA256SUMS.txt`, run `orbit install`).
* The official public key is the constant that is today in both managers
  (`2LCW/gbjU6USD1CX5myen6o6Y0E/NN+KFjsRiSV7Sbs=`); define it once in
  `parsec-orbit-registry` and use it in `orbit` and the market plugin.
* Tests: `Fetch` double, scratch Orbit roots, a fake `parsecd.exe`; never a
  real client.

### 4.2 Settings and records in the new formats (restructure.md section 7)

* `plugins/parsec-orbit-plugin-stun-swap`: `servers.txt` to `servers.toml`
  (`servers = ["host@port"]`), the old file as fallback. Windows-only crate.
* `install-state.json` format 2 and `market.json` `format` (with 4.1).
* Single-source versions: drop `version` from the source `plugin.toml`s and
  from `declare_plugin!` (`CARGO_PKG_VERSION`); `parsec-orbit-release package`
  already stamps `version`/`kind`/`entry` into the staged manifest. Until this
  lands the three copies must agree; the Publish workflow bumps only
  `Cargo.toml`.
* Delete `plugin.macos.toml` once `package` is the only producer; the
  `scripts/*-macos.*` consumers go in the cutover.
* `docs/development/formats.md` is the rule; the registry types are the schema.

### 4.3 Market plugin follow-ups

* Make the official location the built-in default source (today a missing
  `sources.toml` is an empty list); the URL is
  `https://raw.githubusercontent.com/keiko233/parsec-orbit/main/manifest/index.json`
  once the registry job has produced it.
* Load the cached index at start (it waits for Refresh today); check the
  UAC-free flow on Windows; measure the plugin size and add it to the size
  report; verify in a real client that an update lands in `pending\` and is
  promoted at the next start.
* A loaded plugin cannot be removed on Windows (folder in use); a staged
  removal is not implemented.

### 4.4 `high-bitrate-native` as `no_std` (optional)

Only if a Windows measurement says the saving is at least 100 KB (estimate
150 to 190 KB). It needs `env::var`, `thread::sleep` and `Instant` replaced by
three kernel32 calls; if a second plugin converts, add the small `os` module
to the SDK (`env_var`, `sleep_ms`, `tick_ms`) instead of two private copies.

### 4.5 Loader `no_std` spike (only if the size budget is still missed)

Described in restructure.md 6.6. Not part of 0.2.0.

## 5. Cutover to 0.2.0 (phase 7)

1. Release workflow: per-OS `parsec-orbit-release package --platform <id>`;
   a `publish` job runs `registry add` and `registry build --sign` and commits
   `manifest/` as a bot (needs the `MARKET_SIGNING_KEY` secret and a branch
   protection rule that lets the bot push; the Publish workflow already pushes
   to `main`). Add `registry.yml` (`registry check` on pull requests touching
   `manifest/`, `registry build --sign` on push to `main`). Replace the
   `Select-String` version reads with `parsec-orbit-release version`.
2. Delete `scripts/install.ps1`, `orbit-manager.ps1`, `orbit-manager-macos.*`,
   `set-web-ui.ps1`, `package-macos.py`, `install-macos.sh`, `console-macos.*`,
   their tests and the `parsec-orbit-market` binary of the registry crate
   (`src/cli.rs`, `src/main.rs`); keep thin `package.ps1`/`verify.ps1`
   delegates. Move the macOS research scripts (`scripts/macos-*canary*`,
   `*audit*`, `scripts/fixtures`, their tests) to `research/macos/` and fix the
   CI paths.
3. Update README (install section), `docs/design/network-installer.md`
   (superseded by ADR 0021 once written), `docs/guides/macos-network-install.md`,
   mark ADR 0006 superseded, write ADR 0021 (installer in Rust) and the
   `no_std` policy ADR, and fix `docs/design/roadmap.md`.
4. Fresh install and in-place upgrade from a 0.1.x install on both OSes before
   tagging 0.2.0. Users of 0.1.x scripts re-bootstrap with `get-orbit`.

## 6. Technical decisions to keep in mind

* **The SDK has no features.** A `std` feature breaks `cargo build
  --workspace` with `E0152: found duplicate lang item panic_impl` as soon as
  one `std` plugin shares the build (reproduced; ADR 0017). `MockHost` is the
  dev-dependency `parsec-orbit-plugin-sdk-testing`. Never make it a normal
  dependency of a `no_std` plugin; `tests/parsec-orbit-e2e/tests/architecture.rs`
  enforces this and the rest of the dependency law.
* **A tool links at most one plugin `rlib`.** Two plugin rlibs export the same
  ABI symbols and an LTO release build fails (`symbol multiply defined`).
  `webview-inject` reads the patch asset with `include_str!` for that reason.
* **No TOML parser work.** The existing `toml` parse-only dependency stays
  everywhere; nothing is replaced or trimmed for size.
* **Crate splits** follow the two criteria in restructure.md 2.5.
* **`staging::promote`** is the only loader-side behaviour added by the
  restructure; it never prevents the installed version from loading.
* **Windows cross-checking from macOS** works for most crates with
  `cargo check --target x86_64-pc-windows-msvc` (installed target); use
  `--exclude` for the crates listed in section 2.
* **Registry data:** `manifest/` holds metadata only (entries for the nine
  official modules, no releases yet); packages stay on release assets.
  `registry build` has not run on real releases.
* The main checkout `/Users/keiko/Desktop/Code/parsec-orbit` was switched to a
  branch `refactor/restucture` by someone else during the session; it is not
  part of this work.

## 7. Commands that were used on macOS

```bash
cargo fmt --all -- --check
PARSEC_ORBIT_REQUIRE_NODE=1 cargo test --workspace --locked
pnpm install --frozen-lockfile && pnpm -r build && pnpm -r test
cargo check --workspace --all-targets
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc \
  --exclude parsec-orbit-registry --exclude parsec-orbit-lib-hooks \
  --exclude parsec-orbit-lib-ui --exclude parsec-orbit-plugin-market \
  --exclude parsec-orbit-release
python3 scripts/test-orbit-manager-macos.py   # and the other scripts/test-*.py CI runs
bash scripts/test-install-macos.sh
```
