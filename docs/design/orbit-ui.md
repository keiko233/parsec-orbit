# Orbit UI Architecture and Design

This document records the evaluation of adding an "Orbit" management UI to
Parsec Orbit, the reverse engineering of the Parsec client's UI subsystems, the
architectural tradeoffs between in-app injection and an external native window,
and the implementation blueprint: `compio-rs/winio` on Windows and AppKit on
macOS ([ADR 0017](decisions/0017-macos-ui-appkit-main-thread.md)).

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
3. **Cross-Platform Future**: The declarative ABI and the SDK bindings are
   platform-neutral, so a plugin that registers a section works on every
   platform the service exists on. The *renderer* is not portable, though:
   `winio`'s AppKit backend owns the process main thread, which a plugin inside
   the client cannot give it. macOS therefore draws the same window with AppKit
   directly - see 3.3.

### 3.2 Why `winio` Must Be an External Window

`winio` creates top-level OS windows (`HWND`, `NSWindow`, `GtkWindow`). Trying to
force-embed a `winio` native window as an in-process child window inside
Parsec's libmatoya render surface causes severe issues:

* Window clipping, Z-order fighting, and paint message deadlocks.
* Mouse capture and keyboard focus conflicts with Parsec's input loop.
* Multi-monitor DPI scaling mismatches between libmatoya and OS controls.

Therefore, the architectural conclusion is definitive:
**`winio` renders a standalone, external companion management window.**

### 3.3 macOS: the same window, drawn with AppKit on the host's main thread

[ADR 0017](decisions/0017-macos-ui-appkit-main-thread.md) records the measurement
behind this section. `winio` does have a macOS backend (`winio-ui-app-kit`, a
non-optional dependency of `winio` 0.12 on macOS), but it cannot be used inside
the client:

* `winio-ui-app-kit`'s `App::new()` starts with
  `MainThreadMarker::new().ok_or(Error::NotMainThread)?`, so a plugin building
  an `App` from its own thread gets `NotMainThread`. The process main thread is
  the only thread AppKit accepts, and inside the client that thread belongs to
  the vendor's run loop.
* `App::block_on` runs `CFRunLoop::run_in_mode` and pumps
  `NSApplication.nextEventMatchingMask...sendEvent`, so `winio` wants to own the
  main run loop; `App::new()` also forces `setActivationPolicy(Regular)`, which
  would add a Dock icon next to the client's.
* On Windows the same `App::new()` has no thread requirement and its loop is
  per-thread, which is exactly why the Windows plugin can own a thread and
  macOS cannot.

So the macOS backend posts every window operation to the main queue
(`dispatch_async` to the main queue) and lets the host's own run loop execute
it. It draws the same declarative items with native AppKit controls
(`NSButton`, `NSSlider`, `NSPopUpButton`, `NSTextField`, `NSTextView`,
`NSTabView`, `NSScrollView`) and never pumps events itself. Neither backend
draws its widgets by hand.

The activation channel changes for the same reason: macOS has no HWNDs and no
system-wide broadcast window message, so the hidden activation window is
replaced by the distributed notification `com.parsec-orbit.ui.toggle`, and the
global hotkeys use Carbon `RegisterEventHotKey` instead of Win32
`RegisterHotKey`. Carbon was chosen because `NSEvent` global monitors and
`CGEventTap` need Accessibility or Input Monitoring approval.

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
 │   • Global Hotkey (Ctrl+Alt+O / Ctrl+F12) • host.ui.show()             │
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
 │  • Built-in: Overview, Plugins, Parsec & Orbit Config, Log             │
 │  • Dynamically rendered plugin sections from registered schemas        │
 └────────────────────────────────────────────────────────────────────────┘
```

### 4.1 Pillar 1: winio External Window Renderer

The renderer runs inside an official plugin (`com.parsec-orbit.ui`) that registers
the `parsec-orbit.ui` service table ([spec/ui-service.md](../spec/ui-service.md)).

* **Lifecycle & Concurrency**:
  * In accordance with [ADR 0004](decisions/0004-loader-stays-light.md), the
    loader stays completely free of `winio`, AppKit or GUI code.
  * The UI plugin spawns a dedicated worker thread during `init` and hosts the
    `winio` single-threaded async event loop. On macOS there is no such thread:
    the plugin owns no event loop at all and posts each AppKit operation to the
    main queue, where the host's run loop executes it (3.3).
  * The window is created hidden on start-up, so Parsec's launch never waits on
    it and the first `show()` has nothing left to build. The macOS window is
    built in a main-queue block during `init` for the same reason.
  * Only the user's instance draws. Parsec starts its client twice - the service
    copy in session 0 and the user's copy on the desktop - and the window and
    the hotkeys belong to the second; the first registers the `parsec-orbit.ui`
    service and nothing else. The check is the same token query the debug
    console uses. macOS runs the client once, so there is no instance check
    there.
  * Cross-thread messages are posted to the window thread and followed by a
    `WM_NULL`. `winio`'s loop sits in `MsgWaitForMultipleObjectsEx` when idle
    and the queue's own waker only counts during a poll, so that empty message
    is what ends the wait and lets the loop drain the queue.
* **Styling**:
  * Styled with a dark theme consistent with Parsec (background `#18181b`,
    surface `#27272a`, border `#3f3f46`, text `#f4f4f5`, accent `#8b5cf6` or
    Parsec crimson `#e01e5a`).
  * **Built-in Tabs** (the first implementation ships the Overview tab; the
    rest are planned):
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
   * The UI plugin registers a global Windows hotkey (default: `Ctrl+Alt+O` / `Ctrl+F12`).
   * Optional system tray icon integration (left-click opens UI, right-click
     context menu).
   * If `parsec-orbit.hooks` is active, plugins can optionally hook client input
     or native buttons to trigger `ui.show()`.
2. **JavaScript / TypeScript Plugins**:
   * Direct programmatic access: `host.ui.show()` / `host.ui.toggle()`.
   * **WebView Bridge Integration**: When `com.parsec-orbit.webview-bridge` is
     present, a lightweight JS helper injects an "Orbit" button into Parsec's
     Settings navigation bar or binds a shortcut inside the WebView2 DOM.
     Clicking the button calls `orbit.emit("orbit:ui:show")`, which the plugin
     realm receives and translates to `host.ui.show()`.
3. **External CLI & Orchestration**:
   * The `orbit-manager.ps1` script provides `orbit-manager.ps1 ui` (or
       `orbit-manager ui`) to bring the companion window up from outside Parsec.
       It posts the activation message instead of pressing keys, so the
       window's own visible/hidden state stays correct; focusing an existing
       window and synthesizing the hotkey are the fallbacks.
   * On macOS the equivalent request is the distributed notification
       `com.parsec-orbit.ui.toggle`. `orbit-manager-macos.py` has no `ui`
       subcommand yet, so today the notification has to be posted by another
       tool.

## 5. Architectural Integrity & Size Policy

* **Loader Remains Unaffected**: The `bcrypt.dll` loader proxy and
  `parsec-orbit-core` link no GUI crates, no `compio`, and no `winio`. The size
  baseline (~340 KB) is completely protected
  ([ADR 0004](decisions/0004-loader-stays-light.md)). On macOS the same holds
  for AppKit: only the UI plugin links `objc2`/`objc2-app-kit`, and the AppKit
  frameworks themselves are system libraries, so the plugin stays in the same
  size class as its Windows build.
* **Plugin Overhead**: Plugins pay zero binary size cost for GUI dependencies. A
  `no_std` Rust plugin registering a settings section only emits a static JSON
  string through `extern "system"` service calls.
* **Isolated Failure Domain**: Any unexpected issue or panic inside the `winio`
  event loop is confined to the UI thread. On macOS the plugin owns no thread
  and no run loop, so the failure domain is the plugin's own calls rather than
  a loop it drives; a panic there still unwinds inside the plugin's main-queue
  block, not the client's run loop.

## 6. Planned: settings the window should grow into

The first implementation ships the Overview tab and whatever the plugins
register for themselves. The work below is agreed and not built; it starts with
the ABI, because two of the five items cannot be drawn with the item types that
exist today.

### 6.1 What the ABI is missing

* **A read-only multiline view.** `text` maps to a single-line label, so neither
  a latency table nor a log tail fits in a row. A `text_view` item (read-only,
  scrolling, fed by `update_state`) covers both.
* **Options that can change while the window is open.** The window builds a
  dropdown from the schema once, at registration; `update_state` only moves the
  selection. A server list that grows needs either the options to travel with
  the state or the section to be re-registered, and re-registering throws away
  the user's place in the panel.
* **Nothing new for restarting.** A `button` is enough; what the window needs is
  a way for a section to say "these values apply after a restart" so the label
  can say so too.

### 6.2 Editable bandwidth ladder

Everything up to 50 Mbps is Parsec's own list. Above it, Orbit appends
`EXTRA_BANDWIDTH_MBPS` (`[60, 75, 100, 125, 150, 200, 250, 300, 400, 500]`) to
the client's in-memory table for the native UI, and to the Web UI's list in
`assets/bandwidth.js`, which carries the same values as a second hand-synced
copy. The window only offers a coarse cut ("highest option").

It should be one editable list instead: a text input taking the values in Mbps
(`60, 75, 85, 100, 150, 500`), so a user who wants 85 Mbps adds 85.

* The native patch rebuilds the whole table, so any list works.
  `legacy::extend_bandwidth_with` asks for the values while the client maps its
  UI module, which is the last moment they can still change the result.
* Validation belongs to the plugin, not to the window: integers only, above the
  built-in maximum, sorted and de-duplicated, and capped at a small count
  because the patch allocates a replacement table. A rejected list has to say
  why in the status row rather than silently keep the old one.
* Store the list through `parsec-orbit.config` (`ConfigClient`, owner
  `com.parsec-orbit.native`) so it survives a restart, and let the WebView
  plugin read the same key instead of keeping its own copy. That also retires
  the "keep in sync with `../src/lib.rs`" comment in the script.

### 6.3 STUN swap: latency table, pinning, custom servers

The plugin probes every candidate and logs each result, but the window only
sees the winner. Picking a server by hand, seeing why a server lost, and adding
one of your own all need the list itself.

* **Latency table**: a `text_view` row that `refresh` rewrites after each pass,
  one line per candidate (`stun.qq.com@3478  23 ms  (active)`), including the
  ones that timed out. The plugin already has this data.
* **Pin a server**: a mode control (`auto` / `pinned`) plus a picker. While
  pinned, `refresh` keeps measuring but does not move `selected()`. A pinned
  server that stops answering must be visible as unhealthy, and there has to be
  an explicit way back to `auto`.
* **Custom servers**: an editable list, one `host@port` per line, in the grammar
  `Candidate::from_text` already accepts. Where it is written down is part of
  the design: the order today is `PARSEC_ORBIT_STUN_SERVERS`, then
  `%APPDATA%\ParsecOrbit\servers.txt`, then `DEFAULT_SERVERS`. Writing the file
  keeps the hand-editable escape hatch and needs no new store. The panel must
  show which source won, or a user cannot tell why an edit did nothing.

### 6.4 Overview: the settings that are not a plugin's

* **Debug console**: the four values `parsec-orbit-core` accepts (`off`, `1`,
  `client`, `host`) as a dropdown, written to the user's environment so the next
  launch reads it. The window only exists in the client instance, so the service
  side cannot be previewed from it; `host` needs the machine scope and therefore
  the elevation of section 6.5.
* **Versions**: `Context` already answers `loader_version()`, `abi_version()`,
  `plugin_dir()` and `data_dir()`. The row shows the loader's version rather
  than the UI plugin's alone, and names the instance it is running in.
* **Cheap extras in the same shape**: open the log file and the data directory,
  and copy the version block for a bug report.

### 6.5 Restart and apply

Almost everything above is read at start-up: the loader's console switch when
the process starts, the ladder when the client maps its UI module, the candidate
list on each probe pass. The window needs one button that persists what can be
persisted and restarts what has to be restarted.

* The sequence already exists as `Stop-Parsec` / `Start-Parsec` in
  `scripts/orbit-manager.ps1`: stop the `Parsec` service, kill `parsecd`, start
  the service again, which relaunches the client.
* Restarting the service needs elevation, so the button runs the helper through
  `Start-Process -Verb RunAs` and the user sees one UAC prompt. The window has
  to assume it is about to die: write the pending values first, then ask for
  the restart.
* A client-only restart is the cheaper path when nothing the service owns
  changed, because the service relaunches `parsecd` by itself. It must not be
  the default for plugin or loader changes, where a DLL may still be mapped.
* Open question: whether to add a `restart` verb to `orbit-manager.ps1` and let
  the button call it, so the sequence lives in one place.

### 6.6 Log tab

The debug console exists because there was nowhere else to put the loader's
output. With a window there is: the console keeps working for users who want it,
and the window grows a Log tab over the same file.

* Source: the loader's own file, `PARSEC_ORBIT_LOG` first, then
  `%APPDATA%\ParsecOrbit\parsec-orbit.log`. The service instance runs as
  `LocalSystem`, so its `%APPDATA%` is a different directory; the Log tab in the
  client instance shows the client's file unless both instances are pointed at
  one path.
* Tailing: reuse the console's rules (`console::follow`, one poll every
  `FOLLOW_INTERVAL` = 200 ms, the same rotate and small-file handling) so the
  window and the console never disagree about what a run wrote. The plugin
  pushes the tail through `update_state`, and only when the file changed - the
  window already compares values before it touches a control.
* Bounds: keep the visible tail to a fixed number of lines and drop the oldest
  first, so a long session cannot grow the control, or its copy in the registry,
  without limit.
* Filtering and copying belong to the plugin: a level filter (info / warn /
  error) and a button that puts the visible text on the clipboard.
