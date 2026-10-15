# 0029: Follow each platform's design language and system appearance

## Context

The first companion used a fixed dark palette taken from Parsec's own look
(`#18181b` surfaces, a violet or crimson accent), recorded in
[Orbit UI](../orbit-ui.md#41-pillar-1-winio-external-window-renderer). It
ignores the user's theme, accent colour and contrast settings, and matches
neither WinUI 3 nor macOS. Windows 11 and macOS 26 each define materials
(Mica, Liquid Glass) and controls that applications are expected to adopt rather
than imitate.

## Decision

The companion has no palette of its own. It follows the system theme (light,
dark and high contrast), the system accent colour and the platform's standard
type ramp, spacing, corner radii and controls.

* **Windows:** Mica window material, a left `NavigationView`, a title bar that
  extends into the content, theme resources for brushes and text styles, and
  settings cards. It uses the shell described in
  [ADR 0027](0027-windows-fluent-xaml-shell.md).
* **macOS:** a sidebar and content split with the system's materials. On macOS
  26 and later the standard controls and the sidebar adopt Liquid Glass through
  the system; glass is applied through AppKit's `NSGlassEffectView` only where the
  system does not do so itself. Earlier releases use the vibrancy materials they
  provide. The binary is built against the macOS 26 SDK so the system opts it in.
  AppKit access goes below winio through the native handles that winio exposes,
  as the current platform module already does.
* No custom colours. Status uses the platform's semantic colours and icons.

The Parsec-specific palette in the original design document is superseded.

## Consequences

The window looks like part of the operating system in any theme and accent, and
adopts later material changes without code changes. Orbit gives up a Parsec brand
look in this window by choice. Visual verification is per platform; the macOS part
cannot be checked on a Windows development machine, so it is implemented after
the Windows renderer and verified separately on macOS 26.
