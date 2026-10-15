# 0028: Describe UI by pages, groups and settings in service version 2

## Context

Version 1.0 of `parsec-orbit.ui` registers a flat list of typed items: a control
type, a label and a few optional numbers. It cannot say what a setting is, only
which widget to draw, so the renderer has no basis for cards, grouping, status
text, icons or a restart notice. The consequences are visible in practice:

* The Overview section reserves the key `restart` for a window-wide footer
  button, a special case no other plugin can use.
* A select's options travel as `{value, options}` inside the state, because
  state is only values.
* A failed save is expressed by overwriting a read-only text row, with no way
  to mark an input invalid or disable a button while work runs.
* Controls were chosen by widget (`readonly_text`, `text_view`, `json_editor`)
  rather than by meaning, and unknown types silently became text.

All consumers are in this repository (the UI service, three official plugins,
the Rust and JavaScript SDKs, the sample), and the specification is a draft.

## Decision

Replace the contract with version 2.0 of the same interface name. It is a
breaking change; version 1 is removed, with no adapter.

1. A **page** is one navigation destination: `id`, `title`, optional
   `description`, `icon` and `order`, a `group` (`orbit` or `plugins`) and an
   ordered list of **blocks**.
2. Blocks are `banner` (severity, title, message), `group` (a titled list of
   settings) and `actions`. A **setting** has a `key`, `label`, optional
   `description` and `icon`, and exactly one **control**: `toggle`, `number`,
   `slider`, `select`, `text`, `json`, `button`, `value` or `log`. A setting may
   hold one level of nested settings, shown when it is expanded.
3. Updates are patches with two parts. `values` sets control values. `props`
   changes presentation: `enabled`, `visible`, `busy`, `label`, `description`,
   `options`, and a `status` (`severity` and `text`) shown with the setting.
   A page-level `restart_required` flag replaces the reserved Overview button;
   the renderer owns a single restart notice that triggers the built-in action.
4. Actions are `{page, key, type, value}` with `type` `change` or `click`. Text
   controls commit on Enter or focus loss unless they declare `commit: change`.
5. Icons are a closed vocabulary of semantic names. Each renderer maps them to
   its own symbol set; an unknown name draws no icon.
6. Unknown block or control types, duplicate keys and values over the limits are
   rejected at registration. The schema is closed; extensions raise the service
   minor version, which plugins can read from the interface header.
7. The companion protocol moves to version 2 with the same names, and the
   service gains `navigate`.

## Consequences

Renderers can implement each platform's idiom (a Windows settings card, a macOS
form row) from meaning, and validation and busy state become part of the
contract. Plugin authors write less structure and get inline errors and restart
handling without private conventions.

The three official plugins, the built-in Overview and Log pages, both SDKs, the
sample and their tests migrate in the same change as the service. An external
version 1 consumer compares the major version, ignores the interface
([Services](../spec/services.md)) and runs without a UI instead of misbehaving.
[UI service](../spec/ui-service.md) is the normative description.
