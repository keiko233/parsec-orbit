# @parsec-orbit/sdk

SDK for [Parsec Orbit](https://github.com/keiko233/parsec-orbit) JavaScript and
TypeScript plugins.

```ts
import { definePlugin } from "@parsec-orbit/sdk";

export default definePlugin({
  async setup(host) {
    host.log.info("loaded", host.plugin.version);
    const { webview } = host;
    if (!webview) throw new Error('this plugin needs permissions = ["webview"]');
    await webview.injectFile("dist/ui.js");
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

## Releasing

`@parsec-orbit/sdk` shares the workspace version and is published by the
Release workflow. Actions -> Publish -> Run workflow with the new version
bumps `Cargo.toml`, `Cargo.lock` and this `package.json`, commits, tags
`v<version>`, and pushes; the tag then runs Release, which builds the GitHub
release and runs `pnpm publish --provenance` for `@parsec-orbit/sdk`. The
repository needs an npm automation token with publish rights on the
`@parsec-orbit` scope in the `NPM_TOKEN` secret; npm trusted publishing can
replace the secret once the package exists.

To test the tarball locally:

```powershell
pnpm --filter @parsec-orbit/sdk build
pnpm --filter @parsec-orbit/sdk pack
```

MIT license.
