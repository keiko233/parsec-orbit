# @parsec-orbit/sdk

SDK for [Parsec Orbit](https://github.com/keiko233/parsec-orbit) JavaScript and
TypeScript plugins.

```ts
import { definePlugin } from "@parsec-orbit/sdk";

export default definePlugin({
  async setup(host) {
    host.log.info("loaded", host.plugin.version);
    await host.webview.injectFile("dist/ui.js"); // needs permissions = ["webview"]
  },
});
```

* `@parsec-orbit/sdk` - the types (`Host`, `WebView`, `Config`) and `definePlugin`.
* `@parsec-orbit/sdk/ui` - for scripts delivered to the client's web UI:
  `orbit.emit/on/off/rpc`, and a typed `rpc`.
* `@parsec-orbit/sdk/ui/events` - the Parsec UI events known to work with `rpc`.
* `@parsec-orbit/sdk/testing` - `createMockHost` / `loadPlugin`: run `setup`
  under Node with an in-memory webview.

The runtime that runs the plugins is the `com.parsec-orbit.js-runtime` plugin;
see the [guide](https://github.com/keiko233/parsec-orbit/blob/main/docs/guides/typescript-plugin.md)
and the [specification](https://github.com/keiko233/parsec-orbit/blob/main/docs/spec/js-sdk.md).

MIT license.
