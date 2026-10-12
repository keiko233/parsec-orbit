# Write a TypeScript plugin

A JavaScript/TypeScript plugin is a folder with a `plugin.toml` and a bundled script. The **JavaScript runtime plugin** (`com.parsec-orbit.js-runtime`) runs it; it is not part of the default install, so install it first (the loader skips your plugin with `needs com.parsec-orbit.js-runtime, not installed` otherwise). The reference is [spec/js-sdk.md](../spec/js-sdk.md); a complete sample is [`examples/parsec-orbit-plugin-bandwidth-ts`](../../examples/parsec-orbit-plugin-bandwidth-ts).

Two scripts, two worlds:

* the **plugin realm** (`main.ts`) runs in the runtime, in the client process: it has `host` (logging, timers, settings, the webview) and no DOM;
* the **page realm** (`ui.ts`) runs in the client's web UI page: it has the DOM and the app, and talks to the plugin through `orbit` from `@parsec-orbit/sdk/ui`. The plugin delivers it with `host.webview.injectFile`.

## Project

```text
my-plugin/
  package.json
  plugin.toml
  build.mjs
  src/main.ts
  src/ui.ts        (only if you change the web UI)
```

`package.json`:

```json
{
  "name": "my-plugin",
  "private": true,
  "type": "module",
  "scripts": { "build": "node build.mjs" },
  "devDependencies": {
    "@parsec-orbit/sdk": "^0.1.0",
    "esbuild": "^0.25.0",
    "typescript": "^5.6.0"
  }
}
```

`plugin.toml`:

```toml
id = "com.example.my-plugin"
name = "My Plugin"
version = "0.1.0"
kind = "js"
entry = "dist/main.js"
requires = ["com.parsec-orbit.js-runtime", "com.parsec-orbit.webview-bridge"]
permissions = ["webview"]
```

`permissions` are default-deny: `host.webview` exists only with `"webview"`, `host.config` only with `"config"`. Both are optional in the TypeScript types, so a plugin checks them once at the top of `setup` (below) instead of failing later with a `TypeError`. A plugin that needs the webview also requires the bridge (`com.parsec-orbit.webview-bridge`), which provides it.

## The plugin realm

```ts
import { definePlugin } from "@parsec-orbit/sdk";

export default definePlugin({
  async setup(host) {
    host.log.info("loaded", host.plugin.version);

    const { webview } = host;
    if (!webview) {
      throw new Error('this plugin needs permissions = ["webview"]');
    }
    webview.on("hello:from-page", (payload) => host.log.info("page says", payload));
    await webview.injectFile("dist/ui.js", { key: "ui" });
  },
});
```

`setup` runs while the loader waits for it (at most 30 seconds, then the plugin counts as failed), so register things and return. `inject`/`injectFile` work before the page exists: the bridge installs the script when the page attaches, and again if the page is recreated. Do not `await host.webview.ready` in `setup`.

`host` offers `log`, `plugin` (`id`, `version`, `dir`, `dataDir`), `setTimeout`/`setInterval`, `config.get/set` (permission `config`: JSON in your data directory) and `webview` (`inject`, `injectFile`, `evaluate`, `on`, `post`, `ready`). Exceptions in callbacks and unhandled rejections are logged under your plugin id and never reach the client.

## The page realm

```ts
import { orbit } from "@parsec-orbit/sdk/ui";

orbit.emit("hello:from-page", { at: Date.now() });      // -> host.webview.on
orbit.on("hello:from-plugin", (payload) => { /* ... */ });   // <- host.webview.post
const config = await orbit.rpc("cfg_get_");               // the Parsec UI <-> client protocol
```

`orbit` only exists in a script that the bridge delivered. A page script should be idempotent: it runs in the current document and again in each new one.

## Build

`build.mjs` (esbuild; two bundles, the page one as a classic script):

```js
import { build } from "esbuild";
const common = { bundle: true, target: "es2020" };
await build({ ...common, entryPoints: ["src/main.ts"], outfile: "dist/main.js", format: "esm" });
await build({ ...common, entryPoints: ["src/ui.ts"], outfile: "dist/ui.js", format: "iife" });
```

Target ES2020 and bundle everything: there is no `require`, no `import` of other files at runtime, and no `fetch`, `process` or file system in the plugin realm (the engine is Boa).

## Test

```ts
import { loadPlugin } from "@parsec-orbit/sdk/testing";
import plugin from "./dist/main.js";

const { mock } = await loadPlugin(plugin, { files: { "dist/ui.js": "/* ... */" } });
assert.deepEqual(mock.scripts.map((s) => s.key), ["ui"]);
mock.page.emit("hello:from-page", { at: 1 });           // as if the page said it
assert.ok(mock.logs.some((line) => line.message.includes("page says")));
```

`createMockHost` / `loadPlugin` give you an in-memory webview (`mock.scripts`, `mock.page.posted`, `mock.page.evaluated`), a `config` store and the log. Page scripts are tested in a fake browser under Node; see the sample's `test/ui.test.mjs`.

## Install

Copy `plugin.toml` and `dist/` to `<Parsec client directory>\plugins\my-plugin\` (or `%APPDATA%\ParsecOrbit\plugins\`), next to the runtime and bridge plugins. The loader log shows

```text
[info] loaded com.example.my-plugin through the "js" handler
[info] ready: 3 loaded, 0 skipped, 0 failed
```

and your `host.log` lines appear as `com.parsec-orbit.js-runtime: com.example.my-plugin: ...`.
