# UI service (`parsec-orbit.ui`)

Status: Draft (version 2.0). Version 2 replaces version 1.0 in one step; see
[ADR 0028](../design/decisions/0028-ui-service-abi-v2.md). The service implements
[ADR 0016](../design/decisions/0016-orbit-ui-winio.md); the independent renderer
process is selected by [ADR 0018](../design/decisions/0018-ui-companion-process.md);
the design language follows [ADR 0029](../design/decisions/0029-native-design-language.md).
See the [architecture walkthrough](../design/orbit-ui.md).

The `parsec-orbit.ui` service provides a native, external management window
rendered by `parsec-orbit-ui-companion[.exe]` using `compio-rs/winio`. The
official UI plugin remains in the client process, owns the authoritative
registry and callback pointers, and relays bounded JSON messages to the helper.
Plugins do not embed UI runtimes. They register declarative **pages** that
describe what each setting means, and listen for user actions. The renderer
decides how a setting looks on its platform.

On Windows the helper uses winio's WinUI 3 (`winui`) backend, with default
backends disabled, and hosts a Fluent XAML shell
([ADR 0027](../design/decisions/0027-windows-fluent-xaml-shell.md)). Its
development baseline is Windows 11 21H2 or newer and a compatible installed x64
Windows App Runtime, as documented by the pinned
[winio 0.12.0 release](https://docs.rs/crate/winio/0.12.0/source/README.md).
Runtime discovery belongs to winio. Orbit does not install that runtime or
silently fall back to the Win32 backend when it is missing. The plugin and
loader dependency boundaries are unchanged.

## Service header and table

Provider: `com.parsec-orbit.ui` (official plugin).
Interface name: `parsec-orbit.ui`.
Version: 2.0 (`(2 << 16) | 0`). A consumer that compares the major version and
finds a different one ignores the interface ([Services](services.md)).

The helper runs only for the interactive client instance. On Windows, the
process must have a known nonzero session and a readable process token that
does not belong to LocalSystem. Session 0, unknown identity/session, and a
LocalSystem host running in the user's session register only the service table,
without launching a helper or registering desktop hotkeys. Existing
hotkeys and CLI activation belong to the interactive instance. That instance
launches the helper hidden and returns from plugin `init` without waiting for
its window.

Failures to create the activation window or register either shortcut are
reported through the Orbit logger, including the Windows error code. One
unavailable shortcut does not disable the other shortcut or CLI activation.

```c
typedef struct ParsecOrbitUiV2 {
    ParsecOrbitInterfaceHeader header;
    int32_t (*show)(void);
    int32_t (*hide)(void);
    int32_t (*is_visible)(void);
    int32_t (*navigate)(const uint8_t *page_id, size_t id_len);
    int32_t (*register_page)(const uint8_t *page_json, size_t page_len);
    int32_t (*unregister_page)(const uint8_t *page_id, size_t id_len);
    int32_t (*update_page)(const uint8_t *page_id, size_t id_len,
                           const uint8_t *patch_json, size_t patch_len);
    int64_t (*on_action)(const uint8_t *page_id, size_t id_len,
                         ParsecOrbitMessageCallback cb, void *user);
    int32_t (*off_action)(int64_t subscription);
} ParsecOrbitUiV2;
```

All strings are UTF-8 `(ptr, len)`. All functions are thread-safe. Callback
function pointers and user pointers remain inside the plugin process and never
cross IPC. Action callbacks execute on a separate plugin-owned delivery thread,
not on the UI thread, IPC thread, or host main thread. They may call the UI
service recursively. Cancellation waits for callbacks already running and
prevents new callbacks from starting; cancellation called from its own callback
does not wait for itself. Queued actions bind the subscribers present when
accepted, so subscribers added later never receive older queued actions.

## Companion process and IPC

The helper executable is placed beside the UI plugin library in the plugin
directory. The plugin launches it hidden with private inherited stdin and stdout
pipes. These pipes are the only transport: there is no socket server, port,
authentication token or public listener. Standard output is reserved for the
protocol; diagnostics go to standard error or the Orbit logger.

The protocol is version 2, newline-delimited JSON. The helper and plugin perform
a version handshake before requests; a helper that reports another version is
incompatible. Each JSON frame, excluding its terminating newline, is limited to
4 MiB. A peer that sends an oversized, malformed or incompatible message is
disconnected. The helper owns the main thread `winio` event loop and receives
only serializable pages, state, the page to show, desired visibility and the
restart notice; it sends back UI actions. Native callback pointers never cross
the process boundary.

The plugin keeps pages, state, subscriptions, callbacks and desired
visibility authoritative. It mirrors renderable data to the helper. On helper
exit or protocol failure, the plugin retries automatically with a bounded
number of attempts and bounded backoff, then replays the registry and desired
visibility after a successful handshake. The supervisor makes at most three
automatic launch attempts per explicit show request, with 250 ms, 500 ms and
750 ms retry delays. An explicit show request renews the exhausted budget;
registry updates do not. Startup handshake and stalled pipe writes each have a
five-second deadline.

The plugin launches the helper without waiting for window creation. `show()`
and `hide()` update desired visibility and enqueue the request without waiting
for the UI; `is_visible()` reports the desired state. Closing the native window
hides it and updates desired visibility. Visibility requests carry a revision;
a delayed close event cannot override a newer show/hide request. When the plugin
process closes its inherited pipe, EOF tells the helper to exit.

Ordinary renderer and transport failures are isolated from the client process.
Native plugin faults and action callback faults still occur in the client
process and are not contained by the helper boundary.

## Functions

| Function | Behaviour |
| --- | --- |
| `show()` | Sets desired visibility to shown and queues the request without waiting for process startup, IPC or window creation. Returns `0` when accepted locally. |
| `hide()` | Sets desired visibility to hidden and queues the request without waiting for IPC. Returns `0` when accepted locally. |
| `is_visible()` | Returns `1` when desired visibility is shown and `0` when hidden. Answers from any thread, including while the helper reconnects. |
| `navigate(id, len)` | Selects a registered page in the window. It does not change visibility. Returns `0`, or `-3` if the page is not registered. |
| `register_page(page, len)` | Registers a page ([Page schema](#page-schema)). Returns `0`, `-1` for invalid JSON or a schema or limit violation, `-2` if the page ID is already registered. |
| `unregister_page(id, len)` | Removes a registered page and its controls. Returns `0`, or `-3` if not found. |
| `update_page(id, id_len, patch, len)` | Applies a [patch](#state-patches) to a registered page. Returns `0`, `-1` for invalid JSON, an unknown key or a type mismatch, `-3` if the page is not found. A rejected patch changes nothing. |
| `on_action(id, id_len, cb, user)` | Subscribes to user interactions within the page. Returns a subscription id `> 0` or a negative error code. |
| `off_action(subscription)` | Cancels an action subscription. Returns `0` or `-3` if not found. |

`register_page`, `unregister_page` and `update_page` update the plugin-owned
registry first, then send the change to the helper. The helper's rendered state
is a replayable mirror, not the authority.

## Activation

The helper starts hidden. Programmatic activation is available through the
service methods. On Windows, the existing host hotkeys and CLI activation also
request visibility through the service. macOS initially supports programmatic
show/hide only; no external CLI or hotkey activation is implemented unless a
renderer adds it.

Windows activation restores a minimized helper window and requests foreground
focus. Foreground focus is best effort under Windows focus restrictions;
accepting a visibility request does not guarantee foreground ownership.

## Page schema

A page is one navigation destination. The JSON passed to `register_page` is an
object. Every field not marked optional is required.

```json
{
  "schema": 2,
  "id": "com.example.bandwidth",
  "title": "Bandwidth",
  "description": "Bitrate choices offered by the client.",
  "icon": "speed",
  "group": "plugins",
  "order": 100,
  "blocks": [
    {
      "type": "banner", "key": "patch_state", "severity": "warning",
      "title": "Not applied yet",
      "message": "Restart the client to use the new bitrates."
    },
    {
      "type": "group", "title": "High bitrate",
      "settings": [
        {
          "key": "enabled", "label": "Allow bitrates above 50 Mbps",
          "description": "Adds the extra choices to the bandwidth menu.",
          "control": { "type": "toggle", "default": true }
        },
        {
          "key": "ceiling", "label": "Maximum bitrate",
          "control": { "type": "number", "min": 10, "max": 300, "step": 5,
                       "unit": "Mbps", "integer": true, "default": 100 }
        },
        {
          "key": "codec", "label": "Preferred codec",
          "control": { "type": "select", "default": "hevc", "options": [
            { "label": "H.264", "value": "h264" },
            { "label": "HEVC (H.265)", "value": "hevc" },
            { "label": "AV1", "value": "av1" } ] }
        }
      ]
    },
    {
      "type": "actions",
      "buttons": [ { "key": "reset", "text": "Reset to defaults" } ]
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `schema` | The integer `2`. Any other value is rejected. |
| `id` | Page identity: 1-128 characters of `[A-Za-z0-9_.-]`. The prefix `orbit.` is reserved for the UI service's built-in pages. |
| `title` | Navigation and header text, at most 64 characters. |
| `description` | Optional text under the header, at most 1024 characters. |
| `icon` | Optional [icon name](#icons). |
| `group` | Optional navigation group, `orbit` or `plugins` (default `plugins`). Only built-in pages may use `orbit`. |
| `order` | Optional integer; lower values come first within a group, ties by title then `id`. Default `0`. |
| `blocks` | Ordered list of [blocks](#blocks), at most 64. |

Keys (`key` on banners, settings and buttons) are 1-64 characters of
`[A-Za-z0-9_.-]` and unique within a page across all of them. A page holds at
most 256 settings in total, including nested ones, and its JSON is at most
256 KiB. Registration rejects unknown fields, unknown block or control types,
duplicate keys, a `default` that does not fit its control, and violations of
these limits. At most 64 pages may be registered.

### Blocks

| Block | Fields | Notes |
| --- | --- | --- |
| `banner` | `key`, `severity`, optional `title`, `message` | An inline notice. `severity` is `info`, `success`, `warning` or `error`. |
| `group` | optional `title`, optional `description`, `settings` | A titled run of settings shown as one visual group. |
| `actions` | `buttons: [{ key, text, icon?, style? }]` | A row of command buttons. `style` is `standard` (default), `accent` or `destructive`. |

### Settings

A setting describes one thing the user can see or change.

| Field | Meaning |
| --- | --- |
| `key` | Identity within the page, also the key in actions and patches. |
| `label` | Primary text, at most 256 characters. |
| `description` | Optional secondary text, at most 1024 characters. |
| `icon` | Optional [icon name](#icons). |
| `control` | Exactly one [control](#controls). |
| `settings` | Optional nested settings, shown when the setting is expanded. One level only; nested settings may not nest further. |

### Controls

| `type` | Value | Fields | Behaviour |
| --- | --- | --- | --- |
| `toggle` | boolean | `default`, `on_text?`, `off_text?` | A switch. |
| `number` | number | `default`, `min?`, `max?`, `step?`, `unit?`, `integer?` | A numeric field with spin buttons. Input outside `min`/`max`, or non-integer when `integer` is true, is refused locally with an inline error and no action. |
| `slider` | number | `default`, `min`, `max`, `step?`, `unit?` | A range slider that shows its current value. `min` and `max` are required. |
| `select` | string or integer | `default`, `options: [{ label, value, description? }]` | A choice from a fixed list. Option values are unique; at most 256 options. |
| `text` | string | `default?`, `placeholder?`, `multiline?`, `rows?`, `monospace?`, `commit?` | Editable text. `multiline` text spans the full width below its label and is `rows` lines tall (default 4, at most 20). `commit` is `submit` (default: Enter or focus loss) or `change` (every edit). |
| `json` | object or array | `default?` | A full-width monospace editor for structured values. Text that is not valid JSON, or is not an object or array, is refused locally with an inline error and no action. Commits like `text` with `submit`. |
| `button` | none | `text`, `style?`, `icon?` | A command button on the setting's row. Emits `click`. |
| `value` | string | `default?`, `monospace?`, `copyable?` | Read-only text. `copyable` offers a copy command. Never emits an action. |
| `log` | string | `default?`, `rows?` | A read-only, scrolling, full-width monospace region of at most 256 KiB. Never emits an action. |

An unrecognized `type` is rejected, not rendered as text.

### Icons

Icons are semantic names. Each renderer maps them to its platform symbol set
(Segoe Fluent Icons on Windows, SF Symbols on macOS). A name outside this list is
rejected at registration; a name a renderer cannot map draws no icon.

`home`, `plugin`, `log`, `network`, `video`, `audio`, `input`, `gamepad`,
`display`, `speed`, `security`, `server`, `settings`, `info`, `warning`, `error`,
`success`, `refresh`, `folder`, `copy`, `play`, `stop`, `restart`, `download`,
`delete`, `edit`, `link`, `clock`, `tools`.

### Navigation

The window shows pages in a sidebar. The `orbit` group comes first, then the
`plugins` group under a "Plugins" heading. The UI service registers the built-in
pages `orbit.overview` and `orbit.log` in its interactive instance
([UI settings](ui-settings.md)). Until a page is registered the window shows an
empty state and never invents content. Closing the window hides it; plugins
keep running.

## State patches

`update_page` takes a JSON object with any of three members. Members not present
change nothing, and a patch is applied entirely or not at all.

```json
{
  "values": { "enabled": false, "ceiling": 150 },
  "props": {
    "ceiling": { "enabled": false, "status": { "severity": "error", "text": "Must be 10-300." } },
    "codec": { "options": [ { "label": "HEVC", "value": "hevc" } ] },
    "patch_state": { "visible": true, "severity": "success", "message": "Applied." }
  },
  "restart_required": true
}
```

* `values` maps a setting key to its new control value, as listed under
  [Controls](#controls). It is rejected for a key whose control has no value
  (`button`) or whose type does not match.
* `props` maps a banner, setting or button key to presentation changes:
  `enabled` and `visible` (booleans), `busy` (boolean, shows progress and
  disables the control), `label`, `description` and `text` (strings), `options`
  (a `select` only; replaces the options), `status` (`{ severity, text }` shown
  with the setting, `severity` being `info`, `success`, `warning` or `error`;
  `null` clears it), and for banners `severity`, `title` and `message`. A
  property that does not apply to the element's kind is rejected.
* `restart_required` sets whether this page has changes that apply only after
  the client restarts. While any page has it set, the window shows one notice
  with a **Restart and apply** command that sends the built-in
  `{"page":"orbit.overview","key":"restart","type":"click","value":null}` action;
  its handling is described in [UI settings](ui-settings.md). A page's flag is
  cleared by `false`, or when the page is unregistered.

Replacing `options` keeps the selection when its value is still present. If it is
not, the control shows no selection and emits no action.

## Action events

When the user interacts with a control, the service delivers a JSON payload to
the page's `on_action` callbacks:

```json
{ "page": "com.example.bandwidth", "key": "ceiling", "type": "change", "value": 150 }
```

`type` is `change` for `toggle`, `number`, `slider`, `select`, `text` and `json`,
with the new control value, and `click` with `"value": null` for `button` and for
buttons in an `actions` block. `value`, `log` and `banner` elements never emit.
Programmatic updates by `update_page` never emit actions.

## Rust SDK API

Plugins using `parsec-orbit-plugin-sdk` access the service through
`services::Ui`. The SDK is `no_std`, so pages and patches are JSON text:

```rust
use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::Ui;

impl Plugin for MyPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        if let Some(ui) = cx.service::<Ui>() {
            ui.register_page(PAGE_JSON)?;
            ui.on_action("com.example.bandwidth", |action| {
                // `action` is the JSON payload above.
            })?;
            ui.update_page("com.example.bandwidth", r#"{"restart_required":true}"#)?;
        }
        Ok(())
    }
}
```

`show`, `hide`, `is_visible`, `navigate`, `register_page`, `unregister_page`,
`update_page`, `on_action` and `off_action` map one to one onto the table.

## JavaScript / TypeScript SDK API

JavaScript plugins declare `"ui"` in `permissions` in `plugin.toml`:

```toml
permissions = ["ui"]
requires = ["com.parsec-orbit.js-runtime", "com.parsec-orbit.ui"]
```

The SDK exports the types `UiPage`, `UiBlock`, `UiSetting`, `UiControl`,
`UiPatch` and `UiAction`, which mirror this schema.

```ts
import { definePlugin } from "@parsec-orbit/sdk";

export default definePlugin({
  async setup(host) {
    const { ui } = host;
    if (!ui) throw new Error('needs permission "ui"');

    await ui.registerPage({
      schema: 2,
      id: host.plugin.id,
      title: "My plugin",
      icon: "plugin",
      blocks: [{
        type: "group",
        settings: [{
          key: "active", label: "Active",
          control: { type: "toggle", default: true },
        }],
      }],
    });

    ui.onAction(host.plugin.id, (action) => {
      host.log.info("user modified UI:", action.key, action.value);
    });

    await ui.updatePage(host.plugin.id, { restart_required: true });
    await ui.show();
  },
});
```

## State and editing

The renderer keeps the last parent value separately from an in-progress edit.
An unrelated update that does not change a value does not replace the draft. A
changed parent value is authoritative and replaces the draft. Replacing a
`select`'s options or adding a `status` never discards a draft in another
control. Programmatic updates and read-only elements never emit user edit
actions. Text a plugin supplies is data: renderers never interpret it as markup,
bindings or resource references.

## Presentation

Renderers follow the platform's design language and the user's appearance
settings ([ADR 0029](../design/decisions/0029-native-design-language.md)). They
define their own metrics, but must support light, dark and high-contrast
appearances, keyboard operation, and the platform's accessibility tree, exposing
each setting's `key` as its automation identifier where the platform has one.
