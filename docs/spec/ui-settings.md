# Built-in UI settings

The UI service owns the `orbit.overview` and `orbit.log` sections in its
interactive instance. They use the ordinary `parsec-orbit.ui` JSON contract;
the companion replaces its placeholder Overview when `orbit.overview` exists.
The service's plugin-owned worker handles their actions. Session 0 does not
start this worker or a window.

## Overview and restarting

Overview reports the loader version, ABI, UI version, instance PID and plugin
and data paths. It provides opening the log file and data directory, copying
version information, choosing the console target, and restarting to apply
settings that were read at startup.

Console values are `off`, `1` (both), `client`, and `host`. The pending selection
is stored in the UI plugin's generated `ui-preferences.json` with `format: 1`
and a `console` string. On Windows it is also written to the current user's
`Environment\PARSEC_ORBIT_CONSOLE` registry value. Saving does not retroactively
open or close the current process's console. The elevated restart helper merges
the same variable into the Parsec service's `Environment` MULTI_SZ, preserving
unrelated variables. Machine-wide environment variables are not changed.

The Windows **Restart and apply** action launches the adjacent companion with
`--restart-and-apply <console> <requesting-pid>` through `runas`. Maintenance
runs before GUI initialization. The helper validates and retains the exact
requesting `parsecd.exe` process before modifying service state, stops and waits
for the Parsec service, terminates the requesting process if still running, and
starts the service again. This disconnects active sessions. Cancellation and
launch failures are displayed in Overview; subsequent maintenance failures are
displayed by the independent helper. Saved plugin settings remain persisted
when UAC is cancelled. A missing service is reported as unsupported rather than
guessing how to launch a standalone client. The Windows action is unsupported
on macOS.

## Log

The Orbit source follows layout log candidates (the explicit `PARSEC_ORBIT_LOG`
override first, then platform defaults), choosing an existing file when one is
present. On Windows the client Parsec source is `log_cl.txt`; its lines are prefixed with
`[parsec] `. The UI does not claim access to another account's service logs.
The macOS host has no verified client-log path exposed to this panel, which
reports that source as unavailable and follows the Orbit log alone.

Both console and Log use `parsec-orbit-log-tail::Follower`: poll at 200 ms,
preserve incomplete lines, detect truncation and replacement using an anchor,
read a fresh small startup file from its beginning and an old/large file from
its end. Each poll reads at most 64 KiB. Retain at most 1000 lines and 256 KiB
of line text, evicting the oldest first; a single oversized line is truncated
on a UTF-8 boundary. Publish text only when the visible result changes.

Source filtering supports All, Orbit and Parsec. Minimum-level filtering
supports All, Info, Warn and Error using the log's bracketed severity tags;
untagged Parsec lines appear with All and Info. Copy copies the currently
filtered text. Viewing and filtering logs does not require elevation.

## Feature panels

High Bitrate edits the additional integer Mbps values in Parsec's native and
WebView bandwidth choice lists, leaving the built-in options up to 50 Mbps.
The native panel owns the persisted configuration, which the WebView plugin
also reads. Invalid values and persistence failures must be visible. Changes
made after patching require restarting the client.

STUN Swap offers automatic selection by measured round-trip latency and manual
pinning to a particular candidate, including custom entries. The panel shows
each candidate's resolved address, health and latency, plus the winning source
of the server list. Since the DNS hook changes an address rather than a UDP
destination port, only port 3478 is supported for redirection; unsupported
ports must be rejected explicitly. A failed manual selection must not silently
use another candidate. Custom-server edits are validated and explicitly saved;
in-progress edits are not replaced by probe results.
