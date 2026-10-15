# 0019 - Use winio's WinUI 3 backend for the Windows companion

## Context

ADR 0016 selects winio and permits its Windows native backends. ADR 0018 moves
the renderer into an owned helper process. The maintainer selected WinUI 3 for
Windows during the native Windows adaptation follow-up on 2026-10-15.

## Decision

Build the Windows helper with winio's `winui` feature and disabled defaults.
Keep the service ABI, inherited-pipe protocol and independent process ownership.
Use winio's native window-handle accessor for activation and wake operations.
Do not enable Win32 theme-hook support merely to render WinUI controls.

Use the pinned winio release's Windows 11 21H2+ development baseline with an
installed compatible x64 Windows App Runtime. Runtime discovery stays with
winio; do not silently select another backend or install system packages.
Keep Windows UI Automation dependencies test-only, scoped to the owned helper.

## Consequences

The helper uses WinUI controls and adds a Windows App Runtime prerequisite.
An absent runtime is a helper startup failure, isolated by the existing bounded
supervisor. Public distribution still needs its own packaging acceptance gate.
The loader, core, service plugin and other plugins acquire no GUI dependency.
Measure the helper's release size separately and verify its real event loop,
controls, activation and shutdown on Windows.
