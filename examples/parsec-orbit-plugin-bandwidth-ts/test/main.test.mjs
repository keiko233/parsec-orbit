import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { loadPlugin } from "@parsec-orbit/sdk/testing";

import plugin from "../dist/main.js";

const uiFile = fileURLToPath(new URL("../dist/ui.js", import.meta.url));

test("setup hands the page script to the bridge under one key", async () => {
  const { mock } = await loadPlugin(plugin, {
    plugin: { id: "com.example.bandwidth-ts", version: "0.1.0" },
    files: { "dist/ui.js": readFileSync(uiFile, "utf8") },
  });
  assert.deepEqual(
    mock.scripts.map((script) => [script.key, script.file, script.removed]),
    [["ui", "dist/ui.js", false]],
  );
  assert.match(mock.scripts[0].source, /__orbitBitrateExtendTs/);
  assert.ok(mock.logs.some((line) => line.message.includes("starting")));
});

test("what the page reports ends up in the log", async () => {
  const { mock } = await loadPlugin(plugin, { files: { "dist/ui.js": "x" } });
  mock.page.emit("bandwidth:applied", { added: 10 });
  assert.ok(
    mock.logs.some((line) => line.message === 'the page extended the list: {"added":10}'),
    JSON.stringify(mock.logs),
  );
});

test("without the webview permission setup fails clearly instead of half working", async () => {
  await assert.rejects(
    loadPlugin(plugin, { permissions: [] }),
    /needs permissions = \["webview"\]/,
  );
});

test("the manifest matches the build", () => {
  const manifest = readFileSync(new URL("../plugin.toml", import.meta.url), "utf8");
  assert.match(manifest, /kind = "js"/);
  assert.match(manifest, /entry = "dist\/main.js"/);
  assert.match(manifest, /permissions = \["webview"\]/);
  assert.match(manifest, /com\.parsec-orbit\.webview-bridge/);
});
