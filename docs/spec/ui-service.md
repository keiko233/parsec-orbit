# UI service (`parsec-orbit.ui`)

Status: Draft (version 1.0). Implements [ADR 0016](../design/decisions/0016-orbit-ui-winio.md);
architectural walkthrough in [design/orbit-ui.md](../design/orbit-ui.md).

The `parsec-orbit.ui` service provides a native, external management window
powered by `compio-rs/winio`. Plugins do not embed UI runtimes; they register
declarative JSON schemas defining configuration sections and controls, and
listen for user action events.

## Service header and table

Provider: `com.parsec-orbit.ui` (official plugin).
Interface name: `parsec-orbit.ui`.
Version: 1.0 (`(1 << 16) | 0`).

The companion window belongs to the instance the user started. Parsec runs its
client twice - once as the service (`LocalSystem`, session 0) and once as the
logged-in user - and a session-0 window is invisible, so the service instance
registers the service table without creating a window and answers the three
visibility calls from the requested state alone. Everything else in this spec
behaves the same in both instances.

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

All strings are UTF-8 `(ptr, len)`. All functions are thread-safe. Callbacks
passed to `on_action` execute on the UI delivery thread; they must not block the
UI thread.

## Functions

| Function | Behaviour |
| --- | --- |
| `show()` | Shows and brings the companion window to the foreground, and makes later `is_visible()` calls answer `1`. The window exists from plugin start-up, so the call never waits on creating one. Returns `0`. |
| `hide()` | Hides the companion window without terminating the background event loop. Returns `0`. |
| `is_visible()` | Returns `1` if the companion window should be visible, `0` if hidden. Answers from any thread. |
| `register_section(schema, len)` | Registers a declarative section schema. Returns `0`, `-1` for invalid JSON schema, `-2` if section ID is already registered. |
| `unregister_section(id, len)` | Removes a registered section and unmounts its controls. Returns `0`, or `-3` if not found. |
| `update_state(id, id_len, state, len)` | Pushes updated control values to the rendered UI. `state` is a JSON object mapping control keys to values. Returns `0`, `-1` invalid JSON, `-3` section not found. |
| `on_action(id, id_len, cb, user)` | Subscribes to user interactions (switch toggles, input edits, button clicks) within the specified section. Returns a subscription id `> 0` or negative error code. |
| `off_action(subscription)` | Cancels an action subscription. Returns `0` or `-3` if not found. |

## Activation

The window starts hidden and three ways bring it up:

* The global hotkeys `Ctrl+Alt+O` and `Ctrl+F12`. Whichever another program has
  not already claimed still works.
* `show()`, `hide()` or the plugin's toggle, called by a plugin running in the
  client instance.
* `scripts/orbit-manager.ps1 ui` from outside Parsec. The script posts the
  registered message `PARSEC_ORBIT_UI_TOGGLE` to the hidden activation window of
  class `ParsecOrbitUiActivation`, which runs the same toggle as the hotkeys.
  The class name and the message name are part of this contract.

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

Plugins using `parsec-orbit-plugin-sdk` access
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

1. **System tray icon placement**: Should the UI service create a system tray
   icon by default, or remain hotkey-only (`Ctrl+Alt+O`) until configured?
2. **IPC for out-of-process companion**: When Orbit runs as an external tool
   rather than an injected DLL, should the UI communicate via named pipes
   with the same JSON action protocol?
3. **Item types the follow-up work needs**: a read-only multiline view (for a
   latency table and a log tail) and dropdown options that can change while the
   window is open. Both are described in
   [design/orbit-ui.md](../design/orbit-ui.md) section 6 and will change this
   spec when they land.
