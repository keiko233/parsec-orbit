# JavaScript / TypeScript SDK

Status: draft. Decisions: [ADR 0003](../design/decisions/0003-js-runtime-is-a-plugin.md), [ADR 0005](../design/decisions/0005-boa-as-js-engine.md).

## Model

A JS plugin is a directory with a `plugin.toml` (`kind = "js"`, `requires = ["com.parsec-orbit.js-runtime"]`) and a bundled script. The JS runtime plugin is **not part of the default install**; without it the loader skips JS plugins with `needs com.parsec-orbit.js-runtime, not installed` ([loading.md](loading.md#3-ordering)).

Two realms exist and the SDK keeps them apart:

| Realm | Runs | Has | Written as |
| --- | --- | --- | --- |
| **Plugin realm** | in the JS runtime plugin (Boa), one context per plugin, in the client process | `Host` API (log, config, paths, webview, timers), no DOM, no `fetch`/fs/net unless permitted | `main.ts` -> `main.js` (`entry`) |
| **Page realm** | in the client's WebView2 UI page | the DOM, the app's React tree, the Parsec bridge (`window.MTY_NativeSendText`), `window.__orbit` | `ui.ts` -> `ui.js`, delivered by the plugin realm via `host.webview` |

"Operating in the JS layer and injecting features" means: the plugin realm decides *what* and *when*; the page realm is where UI changes happen. `assets/bandwidth.js` of the webview plugin is page-realm code and becomes `ui.ts`.

## Plugin lifecycle

```ts
import { definePlugin } from "@parsec-orbit/sdk";

export default definePlugin({
  async setup(host) {
    host.log.info("loaded", host.plugin.version);
    await host.webview.injectFile("ui.js");            // persistent: every new document + current one
    host.webview.on("bandwidth:applied", (payload) => host.log.info("applied", payload));
  },
  teardown() {},
});
```

* The entry is an ES module whose default export is the result of `definePlugin` (an identity function that fixes the types). Bundlers produce a single file (`esbuild`/`tsup` recipe in the template), target ES2020.
* `setup(host)` may be async; the runtime waits for it before the plugin counts as *loaded*. A rejection marks the plugin *failed* and the error is logged with the stack.
* `teardown()` is reserved for the future unload (never called today).
* Unhandled rejections and exceptions in callbacks are caught by the runtime, logged under the plugin id, and never reach the client.

## `Host` API (plugin realm)

```ts
interface Host {
  plugin: { id: string; version: string; dir: string; dataDir: string };
  log: { trace(...a: unknown[]): void; debug(...): void; info(...): void; warn(...): void; error(...): void };
  config: { get<T>(key: string): Promise<T | undefined>; set(key: string, value: unknown): Promise<void> };   // Phase 5 service; typed now
  webview: WebView;           // requires permission "webview"
  setTimeout/clearTimeout/setInterval/clearInterval;   // runtime-provided timers
}

interface WebView {
  readonly ready: Promise<void>;
  inject(source: string, opts?: { key?: string }): Promise<ScriptHandle>;  // add_script
  injectFile(path: string, opts?: { key?: string }): Promise<ScriptHandle>;
  evaluate<T = unknown>(expression: string): Promise<T>;                   // evaluate, JSON in/out
  on(channel: string, handler: (payload: unknown) => void): Unsubscribe;   // subscribe
  post(channel: string, payload?: unknown): Promise<void>;
}
```

All of these map one-to-one to the native `parsec-orbit.webview` service ([services.md](services.md#parsec-orbitwebview-provided-by-the-webview-bridge-plugin-version-10)); the runtime is a thin binding. `injectFile` reads relative to the plugin directory only (no path traversal).

## Page-realm helper (`@parsec-orbit/sdk/ui`)

```ts
import { orbit } from "@parsec-orbit/sdk/ui";

orbit.emit("bandwidth:applied", { values });                 // -> host.webview.on in the plugin realm
orbit.on("bandwidth:set", (p) => { /* from host.webview.post */ });
const info = await orbit.rpc("appversn");                    // Parsec UI<->client RPC (protocol_version 1 envelope)
await orbit.rpc("cfg_seti", { name: "encoder_bitrate", value: 100 });
```

`orbit.rpc` implements the request/reply envelope documented in the research notes (`request_id` as event channel, 10 s timeout, `protocol_version: 1`). Known event names and their payloads are typed in `@parsec-orbit/sdk/ui/events` as they are verified, starting with those `bandwidth.js` uses (`cfg_get_`, `cfg_seti`, `parsec_config_update`).

## Packages

| Package | Content |
| --- | --- |
| `@parsec-orbit/sdk` | Types (`Host`, `WebView`, `definePlugin`), `./ui` page helper, `./testing` (a mock `Host` with an in-memory webview, runnable under Node/Vitest). Published as ESM with `.d.ts`. Lives in `sdk/js/`. |
| `create-parsec-orbit-plugin` (later) | Template generator: `plugin.toml`, `main.ts`, `ui.ts`, bundler config, test, packaging script. |

Versioning: the SDK's major tracks the `parsec-orbit.js` / `.webview` interface major. `plugin.toml` `api` and an `engines`-style field in the package state which runtime versions a plugin was built for.

## Runtime (JS runtime plugin)

* Boa, one `Context` per plugin, each driven by its own `ractor` actor on a dedicated thread (contexts are not `Send`). Messages in: `Load`, `Callback`, `Timer`; the actor loop owns the job queue.
* Registers `parsec-orbit.kind.js` (load: read `entry`, evaluate as module, call `setup`) and `parsec-orbit.js`.
* Gets WebView access from the `parsec-orbit.webview` service, so the JS runtime plugin itself `requires` the webview-bridge plugin when any JS plugin has `permissions = ["webview"]` (declared in the runtime's own manifest as `optional`).
* No fs/net/process access in the plugin realm. `permissions` in the manifest are default-deny and enforced by the runtime.

## Testing and baselines

* Unit tests of plugin logic run on Node with `@parsec-orbit/sdk/testing` (mock `Host`).
* Page-realm scripts keep the existing approach: run in a fake browser under Node (see `plugins/parsec-orbit-plugin-high-bitrate-webview/tests/bandwidth.test.js`).
* End-to-end: the e2e host loads the runtime plugin plus the TS port of the bandwidth sample; the native `native` plugin and this JS sample are the two baselines the SDKs are validated against.

## Open questions

* Whether to expose Node-style `require()` inside the plugin realm for multi-file plugins, or insist on bundling (leaning: bundling only).
* Source maps for stack traces in the log.
* Where `config` lands first: runtime-local storage (JSON under `data_dir`) before the `parsec-orbit.config` service exists.
