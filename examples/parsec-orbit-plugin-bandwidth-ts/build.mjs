// Bundles the two scripts of the plugin:
//   dist/main.js  the plugin realm, an ES module (what `entry` points at)
//   dist/ui.js    the page realm, a classic script the plugin injects; its last
//                 expression is the status, like the original bandwidth.js
import { build } from "esbuild";

const common = { bundle: true, target: "es2020", logLevel: "info" };

await build({ ...common, entryPoints: ["src/main.ts"], outfile: "dist/main.js", format: "esm" });
await build({
  ...common,
  entryPoints: ["src/ui.ts"],
  outfile: "dist/ui.js",
  format: "iife",
  globalName: "__orbitBandwidthUi",
  footer: { js: "__orbitBandwidthUi.status;" },
});
