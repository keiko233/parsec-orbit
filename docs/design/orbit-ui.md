# Orbit UI Architecture and Design

This document records the evaluation of adding an "Orbit" management UI to
Parsec Orbit, the reverse engineering of the Parsec client's UI subsystems, the
architectural tradeoffs between in-app injection and an external native window,
and the implementation blueprint using `compio-rs/winio`.

## 1. Background & Problem Statement

Parsec Orbit plugins and configuration are currently managed out-of-band:

* Parsec settings are edited in `%APPDATA%\Parsec\config.json` or through
  environment variables.
* Plugin settings are managed programmatically via `parsec-orbit.config` or
  raw JSON files on disk.
* Runtime diagnostics rely on reading `parsec-orbit.log` or launching the debug
  console (`PARSEC_ORBIT_CONSOLE=1`).

Users and plugin authors need a consolidated visual interface to:

1. Inspect Orbit Loader version, ABI level, Parsec version, and host status.
2. Manage installed plugins (toggle enabled/disabled, view permissions, inspect
   metadata).
3. Configure core Parsec settings (bitrate, renderer, network parameters) with
   hot-reload.
4. Inspect and modify plugin-specific settings and raw configuration schemas.
5. View real-time logs and toggle developer diagnostics.

## 2. Parsec Client Reverse Engineering

Reverse engineering of the Parsec client (`parsecd-150-105c.dll` and related
binaries) revealed two entirely distinct UI implementations:

### 2.1 Native UI (libmatoya, C IMGUI)

* **Implementation**: Built with `libmatoya`, an immediate-mode GUI written in C.
* **Location**: Compiled into `%APPDATA%\Parsec\parsecd-<version>.dll`.
* **Tab Layout**: The tab order in native mode is:
  `Client | Host | Approved Apps | Network | Hotkeys | Gamepad | Experimental | Account`.
* **Rendering Mechanics**:
  * Settings navigation is drawn in an immediate-mode loop around VA
    `0x19280 - 0x19950`.
  * Active tabs are controlled by a 64-bit bitmask written to global state
    (e.g., `0x200000040000001` for `Client`, `0x200000040000002` for `Host`,
    `0x200000040000004` for `Approved Apps`).
  * Each frame checks the active mask and executes the corresponding panel
    drawing routine.
  * Clicks on external items (e.g., `Account` at VA `0x19948`) retrieve function
    pointers from context structures to open URLs (`https://dash.parsec.app`).
* **Assessment**:
  * In-memory hooking (e.g., via `parsec-orbit.hooks`) to hijack the tab loop is
    extremely fragile: function virtual addresses and global state offsets shift
    with every Parsec release (`150-105b` -> `150-105c`, etc.).
  * `libmatoya` lacks rich controls (JSON editors, scrollable tables, searchable
    lists, validation). Implementing a modern settings experience inside
    immediate-mode C requires excessive low-level custom drawing.

### 2.2 Web UI (Chromium WebView2)

* **Implementation**: Built with React, styled-components, and Radix UI primitives.
* **Location**: Hosted by Microsoft Edge WebView2, loading
  `https://builds.parsec.app/webview/release-ui/index.html`.
* **Activation**: Enabled when `client_web_ui: true` is configured in
  `%APPDATA%\Parsec\config.json`.
* **Rendering Mechanics**:
  * The Settings view is rendered by React components (such as `Oy`), mounting a
    `<nav aria-label="Settings">` element with `<ul>` navigation items.
  * Communication between the web UI and client core occurs via
    `window.MTY_NativeSendText` using a versioned JSON RPC envelope.
* **Assessment**:
  * DOM and React tree injection via the existing DevTools bridge
    (`com.parsec-orbit.webview-bridge`) is clean and high-fidelity for web UI
    users.
  * **Fatal Limitation**: Native UI users (`client_web_ui: false`, or users on
    systems without WebView2 runtime) cannot access injected webview tabs.

### 2.3 Evaluation Summary

| Dimension | In-App Native IMGUI Hook | In-App WebView2 DOM Injection | External Native Companion Window |
| :--- | :--- | :--- | :--- |
| **Audience Coverage** | Native users only | Web UI users only | **100% of users (Native + Web)** |
| **Update Resilience** | **Very Low** (binary offsets drift) | Medium (DOM structure changes) | **Immune** (decoupled from client) |
| **UI Capabilities** | Bare-bones IMGUI controls | Full web stack (CSS, React) | Native rich controls (winio) |
| **Parsec Client Risk** | High (in-process render crash) | Low (isolated in webview) | **Zero** (runs in companion thread) |
| **Cross-Platform Path**| None (Win32 specific binary) | Webview-dependent | **High** (winio Win/Mac/Linux) |

## 3. Technology Selection: `compio-rs/winio`

[`compio-rs/winio`](https://github.com/compio-rs/winio) is a lightweight,
single-threaded, asynchronous native GUI runtime for Rust built on top of the
completion-based async runtime `compio` (using IOCP on Windows, io_uring on
Linux, and kqueue on macOS).

### 3.1 Why `winio` Fits Orbit

1. **Native OS Windowing without Chromium/Electron**: `winio` drives native
   widgets (`winio-ui-win32` / `winio-ui-winui` on Windows, `winio-ui-app-kit`
   on macOS, and `winio-ui-gtk` on Linux) with virtually zero runtime memory
   overhead and small binary size.
2. **Async Integration**: It integrates seamlessly with asynchronous tasks,
   timers, and channel-based message passing, matching Orbit's actor-like
   threading model.
3. **Cross-Platform Future**: As Orbit expands beyond Windows to macOS and
   Linux loaders, the same UI code runs across platforms without rewriting.

### 3.2 Why `winio` Must Be an External Window

`winio` creates top-level OS windows (`HWND`, `NSWindow`, `GtkWindow`). Trying to
force-embed a `winio` native window as an in-process child window inside
Parsec's libmatoya render surface causes severe issues:

* Window clipping, Z-order fighting, and paint message deadlocks.
* Mouse capture and keyboard focus conflicts with Parsec's input loop.
* Multi-monitor DPI scaling mismatches between libmatoya and OS controls.

Therefore, the architectural conclusion is definitive:
**`winio` renders a standalone, external companion management window.**

## 4. The Three-Pillar Realization Logic

The UI architecture is divided into three cleanly decoupled pillars:

```text
 ┌────────────────────────────────────────────────────────────────────────┐
 │                     Pillar 2: Unified Declarative ABI                  │
 │                                                                        │
 │  Section { id: "orbit.plugins", title: "Plugins", items: [...] }        │
 └───────────────▲────────────────────────────────────────▲───────────────┘
                 │                                        │
    ┌────────────┴─────────────┐             ┌────────────┴─────────────┐
    │   Rust SDK (crates.io)   │             │   JS SDK (@parsec-orbit) │
    │   cx.service::<Ui>()     │             │   host.ui.register(...)  │
    └──────────────────────────┘             └──────────────────────────┘
                 │                                        │
 ┌───────────────┼────────────────────────────────────────┼───────────────┐
 │               ▼                                        ▼               │
 │   Pillar 3: Framework-Specific Activation                              │
 │   • Global Hotkey (Ctrl+Shift+O)          • host.ui.show()             │
 │   • System Tray Icon                      • WebView injected trigger   │
 │   • Programmatic ui.show()                • CLI: orbit-manager.ps1 ui  │
 └───────────────────────────────┬────────────────────────────────────────┘
                                 │
                                 ▼ [parsec-orbit.ui service]
 ┌────────────────────────────────────────────────────────────────────────┐
 │                 Pillar 1: winio External Window Renderer               │
 │                                                                        │
 │  com.parsec-orbit.ui (Official Service Plugin on dedicated thread)     │
 │  • Win32 / WinUI native window with Parsec dark theme                  │
 │  • Built-in: Overview, Plugins, Parsec & Orbit Config, Diagnostics     │
 │  • Dynamically rendered plugin sections from registered schemas        │
 └────────────────────────────────────────────────────────────────────────┘
```

### 4.1 Pillar 1: winio External Window Renderer

The renderer runs inside an official plugin (`com.parsec-orbit.ui`) that registers
the `parsec-orbit.ui` service table ([spec/ui-service.md](../spec/ui-service.md)).

* **Lifecycle & Concurrency**:
  * In accordance with [ADR 0004](decisions/0004-loader-stays-light.md), the
    loader stays completely free of `winio` or GUI code.
  * The UI plugin spawns a dedicated worker thread during `init` and hosts the
    `winio` single-threaded async event loop.
  * The window is created hidden on start-up (or lazily upon first `show()`) to
    ensure zero startup impact on Parsec client launching.
* **Styling**:
  * Styled with a dark theme consistent with Parsec (background `#18181b`,
    surface `#27272a`, border `#3f3f46`, text `#f4f4f5`, accent `#8b5cf6` or
    Parsec crimson `#e01e5a`).
* **Built-in Tabs**:
  1. **Overview**: Displays loader version, ABI level, Parsec version, loaded
     plugins count, and active UI mode.
  2. **Plugins**: Lists all discovered plugins, enabled/disabled toggle switches,
     permissions, authors, and license metadata.
  3. **Configuration**:
     * Parsec client settings: Form for bitrates, display resolution, renderer,
       and network ports, with live updates to `config.json`.
     * Orbit plugin settings: Visual editor backed by `parsec-orbit.config`.
     * Advanced raw JSON editor with syntax validation.
  4. **Diagnostics**: Real-time log streamer for `parsec-orbit.log` and
     `log_cl.txt`, with copy-to-clipboard and log level filtering.

### 4.2 Pillar 2: Unified Declarative ABI

Plugins do not write GUI code, compile widget trees, or link OS windowing
libraries. Instead, plugins register declarative data structures.

* **Declarative Schema (JSON)**:
  * **Section**: Defines a group of settings (`id`, `title`, `description`, `order`).
  * **Controls**:
    * `toggle`: Boolean switch (e.g. enable high bitrate).
    * `text_input` / `number_input`: Single-line inputs with validation bounds.
    * `select`: Dropdown with fixed options.
    * `button`: Triggers an action in the plugin.
    * `json_editor`: Multiline raw JSON editor for complex state.
    * `readonly_text`: Key-value status display.
* **Bidirectional Communication**:
  * **Plugin -> UI**: `register_section` registers the schema; `update_state`
    pushes updated values.
  * **UI -> Plugin**: User interactions emit action events to the plugin's
    registered callback (`on_action`).
* **Full SDK Symmetry**:
  * **Rust SDK**: `cx.service::<Ui>()` exposes `UiClient`, allowing `#![no_std]`
    plugins to register settings with static string literals or lightweight
    JSON payloads.
  * **JS SDK**: `host.ui` exposes typed TypeScript methods
    (`host.ui.registerSection(...)`, `host.ui.onAction(...)`) consuming the
    exact same schema.

### 4.3 Pillar 3: Framework-Specific Activation

Each framework environment provides an intuitive way to open and toggle the UI:

1. **Rust Native Plugins**:
   * Call `ui.show()`, `ui.hide()`, or `ui.toggle()` directly from `Context`.
   * The UI plugin registers a global Windows hotkey (default: `Ctrl+Shift+O`).
   * Optional system tray icon integration (left-click opens UI, right-click
     context menu).
   * If `parsec-orbit.hooks` is active, plugins can optionally hook client input
     or native buttons to trigger `ui.show()`.
2. **JavaScript / TypeScript Plugins**:
   * Direct programmatic access: `host.ui.show()` / `host.ui.toggle()`.
   * **WebView Bridge Integration**: When `com.parsec-orbit.webview-bridge` is
     present, a lightweight JS helper injects an "Orbit" button into Parsec's
     Settings navigation bar or binds `Ctrl+Shift+O` inside the WebView2 DOM.
     Clicking the button calls `orbit.emit("orbit:ui:show")`, which the plugin
     realm receives and translates to `host.ui.show()`.
3. **External CLI & Orchestration**:
   * The `orbit-manager.ps1` script provides `orbit-manager.ps1 ui` (or
     `orbit-manager ui`) to launch or bring the companion window to the front
     from outside Parsec.

## 5. Architectural Integrity & Size Policy

* **Loader Remains Unaffected**: The `bcrypt.dll` loader proxy and
  `parsec-orbit-core` link no GUI crates, no `compio`, and no `winio`. The size
  baseline (~340 KB) is completely protected ([ADR 0004](decisions/0004-loader-stays-light.md)).
* **Plugin Overhead**: Plugins pay zero binary size cost for GUI dependencies. A
  `no_std` Rust plugin registering a settings section only emits a static JSON
  string through `extern "system"` service calls.
* **Isolated Failure Domain**: Any unexpected issue or panic inside the `winio`
  event loop is confined to the UI thread and can be safely caught without
  crashing Parsec's streaming session.
