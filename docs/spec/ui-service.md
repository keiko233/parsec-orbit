# UI service (`parsec-orbit.ui`)

Status: Draft (version 1.0). The ABI and declarative schema implement
[ADR 0016](../design/decisions/0016-orbit-ui-winio.md); the independent renderer
process is selected by [ADR 0018](../design/decisions/0018-ui-companion-process.md).
See the [architecture walkthrough](../design/orbit-ui.md).

The `parsec-orbit.ui` service provides a native, external management window
rendered by `parsec-orbit-ui-companion[.exe]` using `compio-rs/winio`. The
official UI plugin remains in the client process, owns the authoritative
registry and callback pointers, and relays bounded JSON messages to the helper.
Plugins do not embed UI runtimes; they register declarative JSON schemas
defining configuration sections and controls, and listen for user action
events.

## Service header and table

Provider: `com.parsec-orbit.ui` (official plugin).
Interface name: `parsec-orbit.ui`.
Version: 1.0 (`(1 << 16) | 0`).

The helper runs only for the interactive client instance. On Windows, Parsec's
session-0 service instance still registers the service table for existing
hotkeys and CLI activation, but it does not launch a helper. Existing hotkeys
and CLI remain the host activation channel. The interactive instance launches
the helper hidden and returns from plugin `init` without waiting for its window.

```c
typedef struct ParsecOrbitUiV1 {
    ParsecOrbitInterfaceHeader header;
    int32_t (*show)(void);
    int32_t (*hide)(void);
    int32_t (*is_visible)(void);
    int32_t (*register_section)(const uint8_t *schema_json, size_t schema_len);
    int32_t (*unregister_section)(const uint8_t *section_id, size_t id_len);
    int32_t (*update_state)(const uint8_t *section_id, size_t id_len,
                           const uint8_t *state_json, size_t state_len);
    int64_t (*on_action)(const uint8_t *section_id, size_t id_len,
                         ParsecOrbitMessageCallback cb, void *user);
    int32_t (*off_action)(int64_t subscription);
} ParsecOrbitUiV1;
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

The protocol is version 1, newline-delimited JSON. The helper and plugin perform
a version handshake before requests. Each JSON frame, excluding its
terminating newline, is limited to 4 MiB. A peer that sends an oversized,
malformed or incompatible message is disconnected. The helper owns the main
thread `winio` event loop and receives only serializable schemas, state, desired
visibility and UI actions; native callback pointers never cross the process
boundary.

The plugin keeps section schemas, state, subscriptions, callbacks and desired
visibility authoritative. It mirrors renderable data to the helper. On helper
exit or protocol failure, the plugin retries automatically with a bounded
number of attempts and bounded backoff, then replays the registry and desired
visibility after a successful handshake. The supervisor makes at most three automatic launch attempts per explicit
show request, with 250 ms, 500 ms and 750 ms retry delays. An explicit show
request renews the exhausted budget; registry updates do not. Startup handshake
and stalled pipe writes each have a five-second deadline.

The plugin launches the helper without waiting for window creation. `show()`
and `hide()` update desired visibility and enqueue the request without waiting
for the UI; `is_visible()` reports the desired state. Closing the native window
hides it and updates desired visibility. Visibility requests carry a revision;
a delayed close event cannot override a newer show/hide request. When the plugin process closes its
inherited pipe, EOF tells the helper to exit.

Ordinary renderer and transport failures are isolated from the client process.
Native plugin faults and action callback faults still occur in the client
process and are not contained by the helper boundary.

## Functions

| Function | Behaviour |
| --- | --- |
| `show()` | Sets desired visibility to shown and queues the request without waiting for process startup, IPC or window creation. Returns `0` when accepted locally. |
| `hide()` | Sets desired visibility to hidden and queues the request without waiting for IPC. Returns `0` when accepted locally. |
| `is_visible()` | Returns `1` when desired visibility is shown and `0` when hidden. Answers from any thread, including while the helper reconnects. |
| `register_section(schema, len)` | Registers a declarative section schema. Returns `0`, `-1` for invalid JSON schema, `-2` if section ID is already registered. |
| `unregister_section(id, len)` | Removes a registered section and unmounts its controls. Returns `0`, or `-3` if not found. |
| `update_state(id, id_len, state, len)` | Pushes updated control values to the rendered UI. `state` is a JSON object mapping control keys to values. Returns `0`, `-1` invalid JSON, `-3` section not found. |
| `on_action(id, id_len, cb, user)` | Subscribes to user interactions (switch toggles, input edits, button clicks) within the specified section. Returns a subscription id `> 0` or negative error code. |
| `off_action(subscription)` | Cancels an action subscription. Returns `0` or `-3` if not found. |

## Activation

The helper starts hidden. Programmatic activation is available through the
service methods. On Windows, the existing host hotkeys and CLI activation also
request visibility through the service. macOS initially supports programmatic
show/hide only; no external CLI or hotkey activation is implemented unless a
renderer adds it.

`register_section`, `unregister_section` and `update_state` update the
plugin-owned registry first, then send the corresponding change to the helper.
The helper's rendered state is a replayable mirror, not the authority.

## Declarative UI schema

The schema passed to `register_section` is a JSON object defining one visual
panel or tab:

```json
{
  "id": "com.example.my-plugin",
  "title": "Bandwidth Tuning",
  "description": "Custom bitrate and frame pacing settings.",
  "order": 100,
  "items": [
    {
      "key": "enabled",
      "type": "toggle",
      "label": "Enable High Bitrate",
      "description": "Overrides client encoder limits.",
      "default": true
    },
    {
      "key": "max_bitrate",
      "type": "number_input",
      "label": "Max Bitrate (Mbps)",
      "min": 10,
      "max": 300,
      "step": 5,
      "default": 100
    },
    {
      "key": "codec",
      "type": "select",
      "label": "Preferred Codec",
      "options": [
        { "label": "H.264", "value": "h264" },
        { "label": "HEVC (H.265)", "value": "hevc" },
        { "label": "AV1", "value": "av1" }
      ],
      "default": "hevc"
    },
    {
      "key": "reset_defaults",
      "type": "button",
      "label": "Reset to Defaults"
    }
  ]
}
```

### Supported item types

| Item type | Description | Payload shape |
| --- | --- | --- |
| `toggle` | Boolean switch checkbox. | `boolean` |
| `text_input` | Single-line text input with optional `placeholder`. | `string` |
| `number_input` | Numeric input with optional `min`, `max`, `step`. | `number` |
| `slider` | Range slider with optional `min`, `max`, `step`. | `number` |
| `select` | Dropdown selector with `options: [{ label, value }]`. | `string` or `number` |
| `button` | Action button triggering a click event. | `null` |
| `json_editor` | Multiline JSON editor for complex configuration. | `object` or `array` |
| `readonly_text` | Read-only status value display. | `string` |
| `text` | The same as `readonly_text`, and what an unrecognized type falls back to. | `string` |

## Action events

When a user interacts with a control in the UI window, the UI service emits an
action payload to the section's registered `on_action` callback:

```json
{
  "section": "com.example.my-plugin",
  "key": "max_bitrate",
  "type": "change",
  "value": 150
}
```

For buttons, `"type"` is `"click"` and `"value"` is `null`.

## Rust SDK API

Plugins using `parsec-orbit-plugin-sdk` or `parsec-orbit-plugin-sdk-std` access
the service through `services::Ui`:

```rust
use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::Ui;

impl Plugin for MyPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        if let Some(ui) = cx.service::<Ui>() {
            // Register declarative section
            ui.register_section(SECTION_SCHEMA_JSON)?;

            // Listen for user actions
            ui.on_action("com.example.my-plugin", |action| {
                // handle change or click
            })?;
        }
        Ok(())
    }
}
```

## JavaScript / TypeScript SDK API

JavaScript plugins declare `"ui"` in `permissions` in `plugin.toml`:

```toml
permissions = ["ui"]
requires = ["com.parsec-orbit.js-runtime", "com.parsec-orbit.ui"]
```

In TypeScript:

```ts
import { definePlugin } from "@parsec-orbit/sdk";

export default definePlugin({
  async setup(host) {
    const { ui } = host;
    if (!ui) throw new Error('needs permission "ui"');

    await ui.registerSection({
      id: host.plugin.id,
      title: "My Plugin Settings",
      items: [
        { key: "active", type: "toggle", label: "Active", default: true },
        { key: "apply", type: "button", label: "Apply Now" }
      ]
    });

    ui.onAction((action) => {
      host.log.info("user modified UI:", action.key, action.value);
    });

    // Programmatic activation
    await ui.show();
  }
});
```

## Open questions

1. **Item types the follow-up work needs**: a read-only multiline view (for a
   latency table and a log tail) and dropdown options that can change while the
   window is open. Both are described in
   [design/orbit-ui.md](../design/orbit-ui.md) section 6 and will change this
   spec when they land.
