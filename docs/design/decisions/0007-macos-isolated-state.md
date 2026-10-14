# 0007 - The macOS host has its own state directory

## Context
The bootstrap resolves the client's state from `getpwuid`, and the client self-updates inside it. The invariant is that Orbit never writes the user's real Parsec state ([macos-host.md](../../spec/macos-host.md#2-invariants)). The smoke test ran with the home lookup interposed to a scratch directory; the real `~/.parsec` and the installed app were byte-identical afterwards.

## Decision
The host's client uses an isolated home, `<Orbit root>/home`, and so an isolated state `<Orbit root>/home/.parsec`. The loader interposes `getpwuid` and `getpwuid_r` for the current uid to return that home, active only in the host process. State is never shared with, copied from or merged into the vendor client's state.

## Alternatives rejected
* Sharing `~/.parsec`: breaks the invariant, and the two clients would update and rewrite each other's `appdata.json`.
* Copying the user's state at install: reads and duplicates credentials (`user.bin`, `devid.bin`) for no benefit.

## Consequences
* The user signs in separately in the host's client; settings are separate. Plugins that edit client settings affect the isolated state only.
* The client self-updates in the isolated state; the host's build can differ from the vendor app's.
* That every path the client writes derives from the interposed lookup is not proven; the audit (E4) is a gate of M1.
