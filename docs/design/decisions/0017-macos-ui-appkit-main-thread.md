# 0017 - The macOS UI window is AppKit on the host's main thread

## Context

[ADR 0016](0016-orbit-ui-winio.md) picks `compio-rs/winio` as the companion
window runtime and runs it on a thread the UI plugin owns. That holds on
Windows, where `winio`'s Win32 backend drives its own message loop on that
thread.

macOS is different in two ways that were not visible when 0016 was written:

1. `winio`'s AppKit backend (`winio-ui-app-kit`) refuses every thread that is
   not the process main thread: `App::new()` starts with
   `MainThreadMarker::new().ok_or(Error::NotMainThread)?`. A plugin cannot
   satisfy that, because inside the client the main thread belongs to the
   vendor: the stub is an AppKit application (`NSApplication`,
   `NSApplicationDelegate`, `setActivationPolicy:`,
   `activateIgnoringOtherApps:` - [client internals](../macos/client-internals.md))
   and the client dylib owns the visible window.
2. AppKit is not thread-safe for UI objects. Creating or mutating an `NSWindow`
   off the main thread is unsupported regardless of which crate wraps it.

An alternative - running `winio`'s AppKit backend on the main thread and
pumping events from inside it - was rejected: it would install a second event
pump next to the vendor's, competing for the same `NSApplication` event queue
while the vendor is streaming video and drawing its own UI.

## Decision

The UI plugin keeps one declarative ABI and two window backends:

* **Windows** keeps `winio` on a thread of its own ([ADR 0016](0016-orbit-ui-winio.md),
  unchanged).
* **macOS** draws the same window with AppKit (`objc2` / `objc2-app-kit`) and
  creates it on the **host's main thread**: plugin code runs on the loader's
  worker, and every AppKit operation is posted to the main queue
  (`dispatch_async` to the main queue), where the host's own run loop executes
  it. Nothing in the plugin pumps events; the host does. The window is still a
  separate top-level `NSWindow` - a companion window, not a view injected into
  the client's window.

Consequences of that choice, all part of the contract:

* The window exists from plugin start-up (built hidden in a main-queue block),
  so the first `show()` never waits on AppKit.
* `show`, `hide` and `is_visible` answer from any thread: the visibility flag is
  an atomic, and only the actual window calls run on the main thread.
* A host whose activation policy is `Prohibited` (the client's stub starts as
  `LSBackgroundOnly`) cannot order a window front. The plugin promotes the
  policy to `Accessory` when it is still `Prohibited` and leaves every other
  policy to the vendor; it never installs a second Dock icon.
* Hotkeys use Carbon `RegisterEventHotKey` (the macOS counterpart of Win32
  `RegisterHotKey`), not an `NSEvent` global monitor, because the latter needs
  Accessibility or Input Monitoring approval. `Control+Option+O` and
  `Control+F12` keep the Windows bindings' meaning; a combination another
  program already owns costs only that shortcut.
* The external activation channel is the distributed notification
  `com.parsec-orbit.ui.toggle`, the counterpart of the Windows hidden
  activation window and its `PARSEC_ORBIT_UI_TOGGLE` message.
* Only the UI plugin links AppKit. The loader, `parsec-orbit-core` and the SDKs
  stay Win32- and AppKit-free ([ADR 0004](0004-loader-stays-light.md)).

## Consequences

* The plugin's macOS artifact depends on `objc2`, `objc2-app-kit` and the
  dispatch main queue; the Windows artifact is unchanged.
* The window's lifetime is tied to the host process' main run loop. A host that
  never runs an AppKit run loop (a test harness, a headless tool) creates no
  visible window; the service still answers from the registry. Evidence from
  such a host is not evidence about the real client.
* `docs/spec/ui-service.md` keeps one service contract and states the
  per-platform mechanism; the activation names are part of that contract.
* Measurement stays scoped: a window that an owned host shows is `[measured]`
  for that host. Whether the patched client shows it under its own run loop is
  an M2 gate, like the other client-session surfaces
  ([macos-phases.md](../macos-phases.md)).
