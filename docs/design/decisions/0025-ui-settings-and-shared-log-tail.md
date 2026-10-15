# 0025: UI settings and shared log following

## Context

The companion needs common application actions and a bounded log panel. The
console already handles partial lines and rotation; a second implementation
would give the two displays different results. The UI also needs the same path
resolution as the loaders, including environment overrides.

## Decision

Extract the platform-independent follower into the zero-dependency
`parsec-orbit-log-tail` crate. Core re-exports its existing API; the UI service
links this crate and `parsec-orbit-layout`. Neither shared crate depends on a
loader, SDK, GUI toolkit or service implementation. Limit each file read to
64 KiB and bound the UI's retained text separately.

Overview and Log are ordinary declarative sections owned by the UI service's
worker. Configuration changes are persisted by their owning plugins. Windows
restart actions launch the companion in a dedicated elevated maintenance mode,
before GUI initialization, so restarting the client cannot kill the restart
operation itself. Console selection is applied to the Parsec service's own
Environment value rather than changing the machine-wide environment.

## Consequences

The UI service gains two narrow shared-library edges, enforced by the
architecture tests. Core gains no third-party dependency or GUI code. Console
and Log share rotation behavior, while the Log panel owns filtering and text
bounds. Restarting a service requires Windows administrator approval; ordinary
configuration editing and viewing logs do not.
