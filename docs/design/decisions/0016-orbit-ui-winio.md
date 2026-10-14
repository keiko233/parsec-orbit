# 0016 - Orbit UI: External winio companion window with unified declarative ABI

## Context

Configuration and diagnostics in Parsec Orbit are currently handled
externally: environment variables, manual edits to `%APPDATA%\Parsec\config.json`,
the `parsec-orbit.config` storage service, or the debug console. Users need an
accessible UI to inspect versions, manage plugins, modify settings, and view
diagnostics.

Reverse engineering the Parsec client revealed two distinct internal UI
subsystems:

1. **Native UI (libmatoya, IMGUI in C)**: Rendered inside `parsecd-*.dll`
   (`parsecd-150-105c.dll`). The Settings tab navigation resides around VA
   `0x19280 - 0x19950`, controlled by 64-bit bitmasks (`0x200000040000001` for
   Client, `...002` for Host, etc.) and hardcoded global offsets. In-memory
   patching is brittle across client updates, and libmatoya provides no
   reusable rich widgets (JSON editors, tables, search inputs).
2. **Web UI (Chromium WebView2)**: React tree loading
   `https://builds.parsec.app/webview/release-ui/index.html`. Injecting a tab
   into `<nav aria-label="Settings">` via DevTools (`com.parsec-orbit.webview-bridge`)
   works well for web UI users, but native UI users (`client_web_ui: false` or
   unsupported configurations) cannot access it.

Evaluating `winio` (`https://github.com/compio-rs/winio`) - an asynchronous,
single-threaded native GUI runtime built on completion-based I/O (`compio`) -
showed that while it cannot be force-embedded into Parsec's internal libmatoya
render loop without serious clipping and event capture issues, it is ideal for
an independent, lightweight native companion window across Windows (Win32/WinUI),
macOS (AppKit), and Linux (GTK).

## Decision

1. **UI is rendered as an external native window using `winio`**: Orbit provides
   a standalone companion window instead of fragile in-process hooks or
   exclusive webview injection. Both native UI and Web UI users get the exact
   same reliable management interface with zero Parsec binary offset drift.
2. **Unified declarative ABI across Rust and JavaScript SDKs**: Plugins do not
   bundle GUI libraries or manage windows. Plugins declare settings "Sections"
   and "Controls" (switches, inputs, selects, buttons, JSON editors) through a
   data-driven schema passed as UTF-8 JSON over the typed `parsec-orbit.ui`
   service vtable ([ADR 0001](0001-typed-service-vtables.md)).
3. **Framework-specific activation mechanisms**:
   * **Rust plugins**: Programmatic `ui.show()`, global hotkey (`Ctrl+Alt+O` / `Ctrl+F12`),
     or system tray interaction.
   * **JavaScript plugins**: `host.ui.show()`, plus an optional webview trigger
     button or shortcut injected into Parsec's web UI when the bridge is active.
   * **External CLI**: `orbit-manager.ps1 ui` triggers the companion window.
4. **The loader stays light ([ADR 0004](0004-loader-stays-light.md))**: `winio`
   and its async event loop run on a dedicated worker thread inside an official
   plugin (`com.parsec-orbit.ui`) or companion process, never in the
   `bcrypt.dll` loader proxy or `parsec-orbit-core`.

## Consequences

* **Zero GUI bloat for plugins**: Plugins define data structures; only the UI
  service links `winio`. Rust plugins remain `no_std` compatible.
* **Full feature parity**: Native users and WebView2 users share the identical
  configuration experience.
* **Cross-platform ready**: `winio` abstracts OS windowing, establishing the
  foundation for future macOS and Linux loader ports.
* **Reliability**: Decoupling the UI from Parsec's render loop eliminates crash
  risks and breakage from Parsec client updates.
