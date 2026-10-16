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
| **Parsec Client Risk** | High (in-process render crash) | Low (isolated in webview) | Renderer and transport faults are isolated in a helper; native plugin and callback faults remain in the client |
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
 │   • Windows host hotkeys                  • host.ui.show()             │
 │   • Programmatic ui.show()/hide()          • Windows CLI activation     │
 └───────────────────────────────┬────────────────────────────────────────┘
                                 │
                                 ▼ [parsec-orbit.ui service]
 ┌────────────────────────────────────────────────────────────────────────┐
 │                 Pillar 1: winio External Window Renderer               │
 │                                                                        │
 │  com.parsec-orbit.ui (official service plugin, owns registry)           │
 │  • Private inherited stdin/stdout pipes; bounded JSON-line IPC          │
 └───────────────────────────────┬────────────────────────────────────────┘
                                 │ version 1 handshake; max 4 MiB per line
                                 ▼
 ┌────────────────────────────────────────────────────────────────────────┐
 │   parsec-orbit-ui-companion[.exe] (main-thread winio renderer)         │
 │  • WinUI 3 on Windows; AppKit on macOS                                  │
 │  • Built-in: Overview, Plugins, Parsec & Orbit Config, Log             │
 │  • Dynamically rendered plugin sections from registered schemas        │
 └────────────────────────────────────────────────────────────────────────┘
```

### 4.1 Pillar 1: winio External Window Renderer

The official plugin (`com.parsec-orbit.ui`) registers the
`parsec-orbit.ui` service table and owns the authoritative schemas, state,
subscriptions and callback pointers ([spec/ui-service.md](../spec/ui-service.md)).
It launches the adjacent `parsec-orbit-ui-companion[.exe]` executable, which
owns the native window and main-thread `winio` event loop.

* **Lifecycle & Concurrency**:
  * In accordance with [ADR 0004](decisions/0004-loader-stays-light.md), the
    loader stays completely free of `winio` or GUI code.
  * Plugin `init` launches the helper hidden and returns promptly. It never
    waits for window creation or a UI round trip.
  * The processes use private inherited stdin/stdout pipes with a version 1
    JSON-line handshake and a 4 MiB maximum line. There is no listener, port or
    authentication token. Standard output carries protocol data; diagnostics
    use the Orbit logger or standard error.
  * Callback pointers stay in the plugin process. Actions are delivered on a
    plugin-owned thread outside the UI, IPC and host main threads. Cancellation
    waits for in-flight callbacks except when called from that callback.
  * The plugin owns desired visibility and the registry. It retries helper
    failures with finite attempts and backoff, then replays schemas, state and
    visibility after a new handshake. Closing the window hides it; closing the
    parent pipe lets the helper exit.
  * On Windows, the session-0 service instance keeps the service table without
    desktop hotkeys or a renderer. Only an instance with a known nonzero process
    session and a readable non-LocalSystem process token starts the helper and
    the hotkey/CLI activation channel. The System host can also run in the
    user's session and must not claim that user's shortcuts (ADR 0026).
* **Styling**:
  * No palette of its own: the window follows the system theme, accent colour
    and each platform's controls and materials
    ([ADR 0029](decisions/0029-native-design-language.md)). On Windows the
    shell is Fluent XAML hosted by winio
    ([ADR 0027](decisions/0027-windows-fluent-xaml-shell.md)). The original
    Parsec-coloured dark theme is superseded.
  * **Built-in pages** (Overview and Log ship; Plugins and Configuration remain
    planned):
  1. **Overview**: Displays loader version, ABI level, Parsec version, loaded
     plugins count, and active UI mode.
  2. **Plugins**: Lists all discovered plugins, enabled/disabled toggle switches,
     permissions, authors, and license metadata.
  3. **Configuration**:
     * Parsec client settings: Form for bitrates, display resolution, renderer,
       and network ports, with live updates to `config.json`.
     * Orbit plugin settings: Visual editor backed by `parsec-orbit.config`.
     * Advanced raw JSON editor with syntax validation.
    4. **Log**: Real-time log streamer for `parsec-orbit.log` and `log_cl.txt`,
       with copy-to-clipboard and log level filtering. This is where the debug
       console's job ends up; whether the separate console window opens at all
       is an Overview setting (section 6).

### 4.2 Pillar 2: Unified Declarative ABI

Plugins do not write GUI code, compile widget trees, or link OS windowing
libraries. Instead, plugins register declarative data structures.

* **Declarative Schema (JSON, service version 2)**
  ([ADR 0028](decisions/0028-ui-service-abi-v2.md)):
  * **Page**: one navigation destination (`id`, `title`, `icon`, `group`,
    `order`) holding ordered blocks.
  * **Blocks**: `banner` (an inline notice), `group` (a titled run of settings)
    and `actions` (a row of command buttons).
  * **Setting**: a labelled thing the user can see or change, with one control:
    `toggle`, `number`, `slider`, `select`, `text`, `json`, `button`, `value` or
    `log`. Presentation (cards, rows, symbols) belongs to each renderer.
* **Bidirectional Communication**:
  * **Plugin -> UI**: `register_page` registers the schema; `update_page` applies
    a patch of values, presentation properties (enabled, busy, status text,
    options) and the page's restart-required flag.
  * **UI -> Plugin**: User interactions emit `change` and `click` actions to the
    plugin's registered callback (`on_action`).
* **Full SDK Symmetry**:
  * **Rust SDK**: `cx.service::<Ui>()` exposes `UiClient`, allowing `#![no_std]`
    plugins to register settings with static string literals or lightweight
    JSON payloads.
  * **JS SDK**: `host.ui` exposes typed TypeScript methods
    (`host.ui.registerSection(...)`, `host.ui.onAction(...)`) consuming the
    exact same schema.

### 4.3 Pillar 3: Framework-Specific Activation

The service API provides programmatic show/hide on Windows and macOS. Windows
retains its existing host activation paths. macOS initially supports
programmatic activation only; it has no external CLI or hotkey activation
unless a renderer adds one.

1. **Rust Native Plugins**:
   * Call `ui.show()`, `ui.hide()`, or `ui.toggle()` directly from `Context`.
   * Existing Windows host hotkeys can request visibility through the service.
2. **JavaScript / TypeScript Plugins**:
   * Direct programmatic access: `host.ui.show()` / `host.ui.toggle()`.
   * **Windows WebView Bridge Integration**: When `com.parsec-orbit.webview-bridge` is
     present, a lightweight JS helper injects an "Orbit" button into Parsec's
     Settings navigation bar or binds a shortcut inside the WebView2 DOM.
     Clicking the button calls `orbit.emit("orbit:ui:show")`, which the plugin
     realm receives and translates to `host.ui.show()`.
3. **External CLI & Orchestration**:
   * Windows retains its existing CLI activation path. The initial macOS
     renderer has no external CLI activation contract.

## 5. Architectural Integrity & Size Policy

* **Loader Remains Unaffected**: The `bcrypt.dll` loader proxy and
  `parsec-orbit-core` link no GUI crates, no `compio`, and no `winio`. The size
  baseline (~340 KB) is completely protected ([ADR 0004](decisions/0004-loader-stays-light.md)).
* **Plugin Overhead**: Plugins pay zero binary size cost for GUI dependencies. A
  `no_std` Rust plugin registering a settings section only emits a static JSON
  string through `extern "system"` service calls.
* **Isolated Failure Domain**: Ordinary renderer and transport faults stay in
  the helper process. Native plugin faults and action callback faults still run
  in the client process and are not contained by this process boundary.

## 6. Settings follow-up implementation

The five follow-up items are implemented through the companion and existing
service ABI. Their normative behavior is in [UI settings](../spec/ui-settings.md),
[additional bandwidth choices](../spec/high-bitrate.md) and
[STUN selection](../spec/stun-swap.md).

### 6.1 Rendering support

The renderer supports read-only scrolling `text_view`, editable multiline
`text_input`, and dynamic select state containing `{ value, options }`.
Updating server options does not re-register the section or discard its panel.
The companion replaces its placeholder Overview with `orbit.overview` and
shows one shared **Restart and apply** footer across plugin tabs.

### 6.2 Editable bandwidth ladder

The High Bitrate panel edits an integer array of additional Mbps choices,
including values such as 85. The plugin sorts and deduplicates values, limits
the list to 128 entries above 50 Mbps, and persists it through the config
service under `com.parsec-orbit.native`. Native and WebView implementations
read the same setting and one shared default asset. Save errors are visible;
a restart applies changes after the original table or script was installed.

### 6.3 STUN selection

STUN Swap displays each candidate's measured latency, resolved address and
health, supports automatic and manual selection, and accepts custom servers
with an explicit Save action. Drafts survive background updates. Generated
`settings.json` stores UI settings; legacy `servers.txt` remains readable.
The panel identifies environment overrides and candidate sources. The current
DNS hook preserves Parsec's port, so custom endpoints must use port 3478.
Manual failures return to Parsec DNS with an explicit unhealthy status.

### 6.4 Overview and restart

Overview displays loader/ABI/UI versions and paths, opens the log and data
directory, copies version information, and selects Off/Both/Client/Host console
behavior. On Windows, console selection is stored in the current user's
environment, with the service's own environment updated by maintenance.

**Restart and apply** launches the adjacent Rust companion in an independent
elevated maintenance mode before GUI initialization. It validates the exact
requesting Parsec process before changing service state, stops the Parsec
service, ends the requesting client if necessary, and starts the service.
Saved settings survive UAC cancellation. Errors are reported; standalone
installations without that service and macOS restart actions are unsupported.
No restart logic was added to the legacy PowerShell scripts.

### 6.5 Log

The Log tab follows the current instance's Orbit log and client Parsec log,
with source and severity filters and Copy visible log. It polls every 200 ms
and bounds retained text to 1000 lines / 256 KiB. The zero-dependency
`parsec-orbit-log-tail` crate shares rotation and partial-line handling with
the console while limiting each poll to 64 KiB. It adds no GUI dependency to
the loader. See [ADR 0025](decisions/0025-ui-settings-and-shared-log-tail.md).
