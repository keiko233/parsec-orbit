import assert from "node:assert/strict";
import test from "node:test";

import { definePlugin } from "../src/index.js";
import { createMockHost, loadPlugin } from "../src/testing.js";
import { orbit, rpc } from "../src/ui.js";

test("definePlugin returns what it is given", () => {
  const definition = { setup() {} };
  assert.equal(definePlugin(definition), definition);
});

test("the mock host logs like the runtime does", async () => {
  const { mock } = await loadPlugin({
    setup(host) {
      host.log.info("hello", host.plugin.id, 1 + 1, { a: [1] });
      host.log.error("bad");
    },
  });
  assert.deepEqual(mock.logs, [
    { level: "info", message: 'hello com.example.test 2 {"a":[1]}' },
    { level: "error", message: "bad" },
  ]);
});

test("capabilities are absent without their permission", async () => {
  const { host } = await loadPlugin({}, { permissions: [] });
  assert.equal(host.webview, undefined);
  assert.equal(host.config, undefined);
  const granted = createMockHost({ permissions: ["webview"] });
  assert.equal(typeof granted.host.webview, "object");
  assert.equal(granted.host.config, undefined);
});

test("inject registers a script once per key and remove marks it", async () => {
  const { mock, host } = await loadPlugin({});
  const first = await host.webview.inject("a()", { key: "main" });
  const again = await host.webview.inject("b()", { key: "main" });
  assert.equal(first.id, again.id, "the same key replaces the script");
  assert.deepEqual(
    mock.scripts.map((script) => [script.key, script.source, script.removed]),
    [["main", "b()", false]],
  );
  await first.remove();
  assert.equal(mock.scripts[0].removed, true);
});

test("injectFile reads only what the plugin directory holds", async () => {
  const { mock, host } = await loadPlugin({}, { files: { "ui.js": "window.ui = 1;" } });
  await host.webview.injectFile("ui.js");
  assert.equal(mock.scripts[0].source, "window.ui = 1;");
  assert.equal(mock.scripts[0].file, "ui.js");
  await assert.rejects(host.webview.injectFile("../secret.js"), /no such file/);
});

test("evaluate answers with JSON and records the expression", async () => {
  const { mock, host } = await loadPlugin(
    {},
    { evaluate: (expression) => (expression === "1+1" ? 2 : undefined) },
  );
  assert.equal(await host.webview.evaluate<number>("1+1"), 2);
  assert.equal(await host.webview.evaluate("nothing"), null);
  assert.deepEqual(mock.page.evaluated, ["1+1", "nothing"]);
});

test("ready, post and evaluate wait for the page", async () => {
  const mock = createMockHost({ pageAttached: false });
  let settled = false;
  const posting = mock.host.webview.post("greeting", { hello: true }).then(() => {
    settled = true;
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(settled, false);
  mock.page.attach();
  await posting;
  assert.deepEqual(mock.page.posted, [{ channel: "greeting", payload: { hello: true } }]);
});

test("page messages reach the handlers until they unsubscribe", async () => {
  const { mock, host } = await loadPlugin({});
  const seen: unknown[] = [];
  const off = host.webview.on("news", (payload) => seen.push(payload));
  mock.page.emit("news", { n: 1 });
  mock.page.emit("other", { n: 2 });
  off();
  mock.page.emit("news", { n: 3 });
  assert.deepEqual(seen, [{ n: 1 }]);
});

test("config keeps JSON values and removes keys set to undefined", async () => {
  const { mock, host } = await loadPlugin({}, { config: { start: 1 } });
  await host.config.set("answer", { n: 42 });
  assert.deepEqual(await host.config.get("answer"), { n: 42 });
  assert.equal(await host.config.get("start"), 1);
  await host.config.set("answer", undefined);
  assert.equal(await host.config.get("answer"), undefined);
  assert.deepEqual([...mock.config.keys()], ["start"]);
});

test("timers can be disposed", async () => {
  const mock = createMockHost();
  let fired = 0;
  mock.host.setTimeout(() => (fired += 1), 5);
  mock.host.setInterval(() => (fired += 10), 5);
  mock.dispose();
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(fired, 0);
});

// ---- the page-side helper -----------------------------------------------------------

test("orbit throws a clear error outside the bridge", () => {
  assert.throws(() => orbit.emit("x"), /window\.__orbit is missing/);
});

test("orbit and rpc delegate to window.__orbit", async () => {
  const calls: unknown[][] = [];
  const fake = {
    version: 1,
    emit: (...args: unknown[]) => (calls.push(["emit", ...args]), true),
    on: (...args: unknown[]) => (calls.push(["on", ...args]), () => {}),
    off: (...args: unknown[]) => void calls.push(["off", ...args]),
    rpc: async (...args: unknown[]) => (calls.push(["rpc", ...args]), { data: { data: { a: 1 } } }),
  };
  (globalThis as any).window = { __orbit: fake };
  try {
    assert.equal(orbit.version, 1);
    assert.equal(orbit.emit("c", { x: 1 }), true);
    const handler = () => {};
    orbit.on("c", handler);
    orbit.off("c", handler);
    assert.deepEqual(await rpc("cfg_get_"), { data: { data: { a: 1 } } });
    await rpc("cfg_seti", { name: "encoder_bitrate", value: 100 });
    assert.deepEqual(calls[0], ["emit", "c", { x: 1 }]);
    assert.deepEqual(calls[3], ["rpc", "cfg_get_", undefined, undefined]);
    assert.deepEqual(calls[4], ["rpc", "cfg_seti", { name: "encoder_bitrate", value: 100 }, undefined]);
  } finally {
    delete (globalThis as any).window;
  }
});
