# 0030: Render the macOS companion as an AppKit split view below winio

## Context

[ADR 0029](0029-native-design-language.md) asks for a sidebar and content split
that adopts the system's materials on macOS. The interim renderer draws every
setting as a winio widget, a label over one control, inside a `TabView`. winio's
AppKit backend publishes no sidebar, switch, stepper or settings layout, and a
winio layout pass positions views by absolute frames, which cannot produce
system spacing or a source-list selection.

winio does hand out the native `NSWindow` (`as_window().as_app_kit()`), so the
screen can be built in AppKit while winio keeps the process.

## Decision

winio keeps owning `App`, the event loop, the `Window` and the component
lifecycle. The window's content is an `NSSplitViewController` set as the
window's `contentViewController`, and winio's own layout pass is a no-op.

1. The sidebar is a source-list `NSTableView`: the `orbit` pages, a separator,
   a "Plugins" heading and the plugin pages, with SF Symbols from the icon list
   in the [specification](../../spec/ui-service.md#icons). The detail is a
   scroll view whose document is one page column, at most 760 pt wide, under a
   title bar that extends into the content (`fullSizeContentView`).
2. Settings map to the standard controls: `NSSwitch`, `NSTextField` with
   `NSStepper`, `NSSlider`, `NSPopUpButton`, `NSTextView` for multiline text,
   JSON and logs, and `NSButton`. Nested settings are disclosure rows. Colours
   are the system's semantic colours and the accent colour; the companion has
   no palette.
3. The restart notice floats over the page. It is `NSGlassEffectView` where the
   class exists (macOS 26) and `NSVisualEffectView` otherwise, chosen by a
   runtime class lookup, so the binary also starts on earlier releases.
   Glass is not applied anywhere else; the system adopts it for the sidebar and
   controls itself.
4. Controls are built from typed values only. Plugin strings reach AppKit
   through `NSString` setters, never as attributed text or markup. Target-action
   handlers and text notifications read their control and post a winio message;
   no plugin or IPC work runs inside an AppKit callback. Classes with
   Objective-C identity (the sidebar data source, the action target, the flipped
   document view) are declared with `objc2::define_class!`.
5. What a renderer decides without a screen (the navigation order, the restart
   notice condition, number and JSON input validation, and when a parent value
   replaces a draft) lives in `model.rs`, which has no AppKit types and is
   tested on every platform. The Windows renderer predates it and keeps its own
   copy until it is moved onto `model.rs` on a Windows machine.

## Consequences

The macOS window has the structure, spacing, selection, focus ring, accent and
contrast behaviour of the system, and `accessibilityIdentifier` carries each
setting's `key`. The cost is AppKit code that winio does not check: it uses
`unsafe` where `objc2` leaves selectors unbound, and it needs the AppKit
feature list in `Cargo.toml`. The loader, `parsec-orbit-core` and the service
plugin gain no dependency; the companion's size is measured in the
[size report](../size-report.md).

Releases before macOS 26 use the vibrancy fallback, which is implemented but not
verified on a development machine running macOS 26 or later. macOS distribution,
signing and notarization are outside this decision.
