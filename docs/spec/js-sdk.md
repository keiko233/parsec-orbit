# JavaScript / TypeScript SDK

Status: implemented (`parsec-orbit-plugin-js-runtime`, `javascript-sdk`, `examples/parsec-orbit-plugin-bandwidth-ts`); `config` uses the config service when it is installed. Decisions: [ADR 0003](../design/decisions/0003-js-runtime-is-a-plugin.md), [ADR 0005](../design/decisions/0005-boa-as-js-engine.md).

## Platforms

The model below is written for Windows (WebView2). On macOS (draft) JS plugins and the `Host` API are the same; the page realm is the client's WKWebView page, whose URL and DOM are not known yet, so `host.webview` is available only once a web view is attached and `orbit.rpc` is unverified: see [macos-platform.md](macos-platform.md#parsec-orbitwebview-and-the-webview-bridge).

## Model

A JS plugin is a directory with a `plugin.toml` (`kind = "js"`, `requires = ["com.parsec-orbit.js-runtime"]`) and a bundled script. The JS runtime plugin is **not part of the default install**; without it the loader skips JS plugins with `skipped, needs com.parsec-orbit.js-runtime, not installed` ([loading.md](loading.md#3-ordering)). A plugin with the `webview` permission also lists `com.parsec-orbit.webview-bridge` in `requires`.

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
    const { webview } = host;
    if (!webview) throw new Error('this plugin needs permissions = ["webview"]');
    await webview.injectFile("ui.js");                 // persistent: every new document + current one
    webview.on("bandwidth:applied", (payload) => host.log.info("applied", payload));
  },
  teardown() {},
});
```

* The entry is an ES module whose default export is the result of `definePlugin` (an identity function that fixes the types). Bundlers produce a single file (`esbuild`/`tsup` recipe in the template), target ES2020. An entry larger than 8 MiB is refused before it is read, and the plugin fails to load.
* `setup(host)` may be async; the runtime waits for it before the plugin counts as *loaded*, for 30 seconds at most. A throw, a rejection or the timeout marks the plugin *failed* (`kind handler returned 1`) and the error is logged under the plugin id. The loader waits too, so `setup` registers things and returns: `inject` works before the page exists (the bridge installs the script when it attaches), while `await host.webview.ready` may wait forever.
* `teardown()` is reserved for the future unload (never called today).
* Unhandled rejections and exceptions in callbacks and timers are caught by the runtime and logged under the plugin id (`unhandled promise rejection: ...`, `uncaught error in a timer: ...`); they never reach the client. A loop of more than 20 million iterations or a recursion deeper than 400 calls throws a `RangeError`.

## `Host` API (plugin realm)

```ts
interface Host {
  plugin: { id: string; version: string; dir: string; dataDir: string };
  log: { trace(...a: unknown[]): void; debug(...): void; info(...): void; warn(...): void; error(...): void };
  config?: Config;            // requires permission "config"; kept by parsec-orbit.config when installed, else in <dataDir>/config.json
  webview?: WebView;          // requires permission "webview"
  setTimeout/clearTimeout/setInterval/clearInterval;   // runtime-provided timers (also global, with console.*)
}

Without a permission the property is absent (`typeof host.webview === "undefined"`): default-deny, enforced by the runtime. `permissions` entries other than `webview` and `config` are ignored with a warning.

interface WebView {
  readonly ready: Promise<void>;                                           // resolves once a page is attached; asking starts the connection
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

`orbit.rpc` implements the request/reply envelope (`request_id` as event channel, 10 s timeout by default, `protocol_version: 1`); unlike the inline helper of the first `bandwidth.js` it rejects on a missing bridge or a timeout instead of resolving `null`. Known event names and their payloads are typed in `@parsec-orbit/sdk/ui/events` as they are verified, starting with those `bandwidth.js` uses (`cfg_get_`, `cfg_seti`, `parsec_config_update`).

## Packages

| Package | Content |
| --- | --- |
| `@parsec-orbit/sdk` | Types (`Host`, `WebView`, `Config`, `definePlugin`), `./ui` page helper (`orbit`, typed `rpc`), `./ui/events` (known Parsec UI events), `./testing` (`createMockHost`, `loadPlugin`: a mock `Host` with an in-memory webview, runnable under the Node test runner or Vitest). Published as ESM with `.d.ts`. Lives in `javascript-sdk/`; built with `tsc`. |
| `create-parsec-orbit-plugin` (later) | Template generator: `plugin.toml`, `main.ts`, `ui.ts`, bundler config, test, packaging script. |

Versioning: the SDK's major tracks the `parsec-orbit.js` / `.webview` interface major. `plugin.toml` `api` and an `engines`-style field in the package state which runtime versions a plugin was built for.

## Runtime (JS runtime plugin)

* Boa 0.22, one `Context` per plugin, each on a dedicated thread (contexts are not `Send`) that owns an inbox of commands (`Load`, `Eval`, `Settled`, `Message`, `Stop`), the timers and the promise job queue - an actor, without `ractor` or an async runtime ([ADR 0004](../design/decisions/0004-loader-stays-light.md)). Blocking work (`evaluate`, `post`, `ready`) runs on short-lived helper threads and settles its promise through the inbox, so a context never blocks on the page; at most 32 such calls may be in flight per context, and further calls are rejected with `too many webview calls in flight` until one finishes.
* Registers `parsec-orbit.kind.js` (load: read `entry`, evaluate as module, call `setup`) and `parsec-orbit.js`.
* Gets WebView access from the `parsec-orbit.webview` service when a plugin is loaded (the runtime lists the bridge as `optional`, so it is initialised first when installed; a JS plugin that needs it lists it in `requires`).
* No fs/net/process access in the plugin realm: no `require`, `process`, `fetch`, `window`. `injectFile` reads below the plugin directory only (canonicalised; `..` and absolute paths are refused). `permissions` in the manifest are default-deny and enforced by the runtime.

## Testing and baselines

* Unit tests of plugin logic run on Node with `@parsec-orbit/sdk/testing` (mock `Host`).
* Page-realm scripts run in a fake browser under Node with the real `window.__orbit` helper (`examples/parsec-orbit-plugin-bandwidth-ts/test/ui.test.mjs`, derived from `crates/parsec-orbit-plugin-high-bitrate-webview/tests/bandwidth.test.js`, which keeps testing the original script).
* End-to-end: the e2e host (`tests/parsec-orbit-e2e/tests/js.rs`) loads the runtime, the bridge and the built TypeScript sample against a fake DevTools endpoint; the `native` plugin and this JS sample are the two baselines the SDKs are validated against.

## Open questions

* Node-style `require()` for multi-file plugins: not provided; plugins are bundled (decided for now).
* Source maps for stack traces in the log.
* A crash or panic inside the engine is a crash of the client (plugins are native code; the workspace builds with `panic = "abort"`).
