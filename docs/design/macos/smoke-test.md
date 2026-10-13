# Dynamic smoke test: loader-as-host (H2)

Historical experiment: signing/entry conclusions here apply to that run. Current patched-bundle evidence is in [native-dependency-poc.md](native-dependency-poc.md); current requirements are in [ADR 0014](../decisions/0014-macos-patched-bundle.md).

Date: 2026-10-13 JST (run window 2026-10-12T23:03:57Z-23:04:47Z, dry run ~22:59Z).
Host: macOS 27.0 arm64. Official client LIVE throughout (pid 58238, never touched).
Read-only sources: `/Applications/Parsec.app` (stub sha256 `785f985d...be76d`,
unchanged after), real `~/.parsec` (150-105c, hashes unchanged after).
Scripts, logs and binaries live in the local research archive (`dynamic/`);
this page is a record of that isolated run, not a procedure, and only our own
copy was ever started or stopped.

Goal: prove or refute, with runtime evidence, that a COPY of the 266 KB stub,
re-signed ad-hoc by us, with `DYLD_INSERT_LIBRARIES=<our dylib>`, bootstraps the
real signed client dylib and starts `wx_main`, with our dylib present in-process
the whole time.

## Verdict: H2 CONFIRMED end-to-end (all 8 cells)

| # | Claim | Result | Key evidence |
|---|-------|--------|--------------|
| (a) | ad-hoc copy + INSERT works | CONFIRMED | Copy `flags=0x2(adhoc)`, no team, no runtime flag; `dyld.log:2` loads `smoke.dylib` into pid 72818; `dyld.log:617` "has interposing tuples" |
| (b) | getpwuid isolation held | CONFIRMED | Dry + full: `fakehome/.parsec` created; real `appdata.json/config.json/log.txt` sha256 + stat IDENTICAL pre/post both runs |
| (c) | stub seeded skel + SHA-256 check passed | CONFIRMED | `fakehome/.parsec/parsecd-150-101a.dylib` sha256 `9cce8505...cdd5` == installed skel byte-for-byte == skel `appdata.json` hash |
| (d) | real client dylib dlopened into copy | CONFIRMED | `dyld.log:648` 101a load, `:1398` 104a load; in-process `smoke-log.txt` shows `img[586]=.../fakehome/.parsec/parsecd-150-104a.dylib` from t+4s to kill, `smoke.dylib` at `img[0]` throughout |
| (e) | wx_main ran | CONFIRMED | `fakehome/.parsec/log.txt`: `Started`, `Client status changed to: -3`, `Hosting IPC`, `BG: me_rate`, UPNP; process stable 45 s at ~235 MB RSS, `LOADER_V14` in argv, clean SIGTERM exit |
| (f) | LOADER_V14 accepted | CONFIRMED | `log: Parsec release (150-101a, Service: -1, Loader: 14)` AND `(150-104a, ..., Loader: 14)`; zero rejection/error lines about loader |
| (g) | second-instance / UI / network | MIXED, all benign | No lock conflict (per-dir `flock`); no login window attributable to our pid, no `window.json`; self-update fetched 150-104a over network; UPNP LAN probes (same noise as real client); no TCC prompt observed |
| (h) | official client + real state untouched | CONFIRMED | pid 58238 alive before/during/after; real-state sha+stat identical; `/Applications` stub sha identical; zero new `.ips` crash reports |

## Method (summary)

The full harness, exact invocations and raw logs are kept in the external
archive (`dynamic/`); this section records only the shape of the run:

- Baseline snapshot of the live client, the installed bundle and the real state
  directory (process list, hashes, `stat`).
- Isolation: the loader dylib interposes the home-directory lookup so the
  bootstrap resolves its state under a scratch fake home; validated with a toy
  program before the real run (bare run resolved the real home, isolated run
  the fake home).
- Host: a copy of the vendor bootstrap stub, re-signed ad-hoc by us and kept
  outside `/Applications`; the installed app was a read-only seed source.
- Run: the copy was started with our loader dylib on the dynamic-linker path; a
  10 s dry run was checked for isolation first, then a 45 s instrumented run
  with file-side evidence collected by the loader. Only our own pid was
  signalled (TERM, KILL fallback).
- Verification: pre/post comparison of the real state and the installed stub
  (hashes and `stat` identical), and process checks for the official client.

## Raw excerpts

fakehome client log (`fakehome/.parsec/log.txt`, complete, 790 B):

    [F 2026-10-13 08:03:58] ===== Parsec: Started =====
    [D 2026-10-13 08:03:58] log: Parsec release (150-101a, Service: -1, Loader: 14)
    [F 2026-10-13 08:03:59] ===== Parsec: Shutdown with 1 =====
    [F 2026-10-13 08:04:00] ===== Parsec: Started =====
    [D 2026-10-13 08:04:00] log: Parsec release (150-104a, Service: -1, Loader: 14)
    [D 2026-10-13 08:04:01] Hosting IPC Status: 1 - No retry.
    [D 2026-10-13 08:04:02] BG: me_rate set to 300 seconds on init
    [D 2026-10-13 08:04:02] Client status changed to: -3
    [D 2026-10-13 08:04:02] MTY_DeleteFile: 'remove' failed with errno 2
    [D 2026-10-13 08:04:10] UPNP: Getting a valid IGD: A valid IGD ... not connected
    [D 2026-10-13 08:04:10] UPNP: Getting a valid IGD: A valid IGD ... not connected

dyld proof (`dyld.log`, pid 72818, 2020 lines):

    dyld[72818]: <DDE4181F-...> /private/tmp/parsec-orbit-research/dynamic/bin/parsecd
    dyld[72818]: <67F79006-...> /private/tmp/parsec-orbit-research/dynamic/smoke.dylib
    dyld[72818]: has interposing tuples so cannot be delayed: smoke.dylib          # :617
    dyld[72818]: <0B3F48A5-...> /private/.../fakehome/.parsec/parsecd-150-101a.dylib  # :648
    dyld[72818]: <CEB0E848-...> /private/.../fakehome/.parsec/parsecd-150-104a.dylib  # :1398

in-process proof (`smoke-log.txt`, every 2 s; first ticks `images=586`, then `1143/1146`):

    tick pid=72818 t=1791846241 images=1143
      img[0]=/private/tmp/parsec-orbit-research/dynamic/smoke.dylib
      img[1]=/private/tmp/parsec-orbit-research/dynamic/bin/parsecd
      img[586]=/private/tmp/parsec-orbit-research/dynamic/fakehome/.parsec/parsecd-150-104a.dylib

lifecycle (`full-poll.txt`, `full-ps.txt`):

    t+5s  alive=yes ... fakehome_log=yes
    ...all 9 polls alive=yes (smoke_parsecd_lines 1 -> 21)...
    t+45s alive=yes markers=2 dyld_parsecd_lines=2 smoke_parsecd_lines=21 fakehome_log=yes
    == terminate OUR copy pid=72818 ==
    exited on SIGTERM
    72818   00:46 235920 /tmp/parsec-orbit-research/dynamic/bin/parsecd LOADER_V14

untouched proof (pre == post, both runs):

    a9d09d24...  ~/.parsec/appdata.json
    08f3fb0a...  ~/.parsec/config.json
    62160486...  ~/.parsec/log.txt        # diff pre/post: SHA IDENTICAL, STAT IDENTICAL
    785f985d...  /Applications/Parsec.app/Contents/MacOS/parsecd   # unchanged
    pgrep post: 57818 CoreParsec, 58238 official client. No new .ips reports.

hash chain (seed -> self-update -> real):

    9cce8505...cdd5  installed skel 150-101a == fakehome seeded 101a == skel appdata.json hash
    eed68639...5a56  fakehome 150-104a == fakehome appdata.json hash (updater rewrote so_name+hash)
    26a0fe52...0ea   real ~/.parsec 150-105c == real appdata.json hash (untouched reference)
    # updater payload 104a is Parsec-signed: TeamIdentifier Y9MY52XZDB, flags=0x10000(runtime)

codesign copy vs original:

    copy:     Identifier=parsecd-5555...  flags=0x2(adhoc)  TeamIdentifier=not set  Sealed Resources=none
    original: Identifier=tv.parsec.www    flags=0x10000(runtime)  TeamIdentifier=Y9MY52XZDB  2026-02-04

## Top 3 surprises

1. The sandbox self-updated over the network. Seeded 150-101a ran `wx_main` once
   (`Loader: 14`, then `Shutdown with 1` = D's `ret==1` restart path, dynamic
   confirmation of D-3a AFTER-call semantics), downloaded 150-104a, rewrote
   `appdata.json` (`so_name`+`hash`), deleted 101a, and re-ran as 104a - all inside
   fakehome. So even an isolated smoke test is NOT network-quiet; budget/plan for it.
2. `stdout.log` is empty (0 B). The stub prints nothing; all evidence is file-side
   (`fakehome/.parsec/log.txt`) plus dyld output. Any future H2 harness must watch
   files, not pipes.
3. `config.txt` (646 B, 12 lines) contains NO `app_block_hash` line at end of run.
   D-3a statically places an `app_block_hash` append before `wx_main`; dynamically it
   is absent (also `hotkey.json` is client-default, byte-identical to the real one,
   not skel-seeded - skel ships only 2 files). Either the append targets another
   moment/object or the dylib migrates it away; needs one focused re-check, not a
   verdict change.

## Notes for D-part-iii / next steps

- Q1 (UI): no login window could be attributed to our pid in 45 s (no `window.json`;
  no screenshots taken; client sat at status -3 = logged-out/idle, same as the real
  client's `-3` lines). A window-visibility run (screencapture + window list) is still open.
- Q2 (gate strength): `Loader: 14` accepted, but rejection behavior (bad/missing
  LOADER_V token) was NOT probed - one argv-mutation run would close it.
- Q3 (entitlements): viewing/hosting under ad-hoc (profile-gated
  `persistent-content-capture`) NOT tested; this run only proves bootstrap + idle client.
- Q4 (TCC): no prompt was observed for our copy, but the TCC db is unreadable from
  this context (`sqlite3 ... TCC.db` -> unable to open), so this is absence-of-signal,
  not proof; a mic/camera-host run would decide it.
- Reuse: `dynamic/{smoke.c,smoke.dylib,bin/parsecd,dry-run.sh,full-run.sh}` plus
  `fakehome/` now at 150-104a are a ready-made rig for Q1-Q4 (re-run needs only fresh
  marker/log cleanup and pre/post snapshots).

## Evidence inventory (`dynamic/`)

`smoke.c`, `smoke.dylib`, `bin/parsecd` (ad-hoc copy), `pwtest*` (interpose toy),
`dry-run.sh` + `dry-*`, `full-run.sh` + `full-*`, `stdout.log` (0 B), `dyld.log`
(2020 lines), `smoke-log.txt`, `marker-*.txt`, `fakehome/.parsec/` (appdata.json,
cc0.txt, config.json, config.txt, hotkey.json, lock, log.txt,
parsecd-150-104a.dylib), `skel-hashes.txt`, `baseline-real-parsec.txt`,
`diag-baseline.txt`, `full-{pre,post}-{sha,stat,pgrep,diag}.txt`.
