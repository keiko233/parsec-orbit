# 0026: UI hotkeys belong to the user process

## Context

The session-only rule admitted a LocalSystem Parsec host running in the same
nonzero session as its user child. Both instances created activation windows;
the host started first and repeatedly failed to launch its companion, while
the user companion remained hidden. Shortcut registration failures were silent.
Targeting the user instance's activation window successfully showed its UI.

## Decision

Require both a known nonzero session and a readable non-LocalSystem process
token before starting the companion or registering hotkeys. Unreadable identity
fails closed. The host still provides the UI service table for plugins.
Log activation-window and individual shortcut registration failures with their
Windows error codes. This supersedes the session-only instance policy in the
UI companion design; other process/IPC contracts remain unchanged.

## Consequences

The same-session host no longer captures the user's UI activation shortcuts
or launches a renderer under the service identity. No loader change, mutable
global state, or third-party dependency is needed. Applying the change to an
already running installation requires a full Parsec restart so the old host
releases its shortcut registrations.
