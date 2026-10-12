import assert from "node:assert/strict";
import test from "node:test";

import { definePlugin } from "../src/index.js";
import { createMockHost, loadPlugin } from "../src/testing.js";
import { orbit, rpc } from "../src/ui.js";

/** The tests grant the capabilities they exercise. */
function granted<T>(value: T | undefined): T {
  assert.ok(value !== undefined, "the mock grants this capability");
  return value;
}

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
  const allowed = createMockHost({ permissions: ["webview"] });
  assert.equal(typeof allowed.host.webview, "object");
  assert.equal(allowed.host.config, undefined);
});

test("inject registers a script once per key and remove marks it", async () => {
  const { mock, host } = await loadPlugin({});
  const webview = granted(host.webview);
  const first = await webview.inject("a()", { key: "main" });
  const again = await webview.inject("b()", { key: "main" });
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
  const webview = granted(host.webview);
  await webview.injectFile("ui.js");
  assert.equal(mock.scripts[0].source, "window.ui = 1;");
  assert.equal(mock.scripts[0].file, "ui.js");
  await assert.rejects(webview.injectFile("../secret.js"), /no such file/);
});

test("evaluate answers with JSON and records the expression", async () => {
  const { mock, host } = await loadPlugin(
    {},
    { evaluate: (expression) => (expression === "1+1" ? 2 : undefined) },
  );
  const webview = granted(host.webview);
  assert.equal(await webview.evaluate<number>("1+1"), 2);
  assert.equal(await webview.evaluate("nothing"), null);
  assert.deepEqual(mock.page.evaluated, ["1+1", "nothing"]);
});

test("a default key comes from the source, so injecting it again replaces it", async () => {
  const { mock, host } = await loadPlugin({});
  const webview = granted(host.webview);
  const first = await webview.inject("window.a = 1;");
  const again = await webview.inject("window.a = 1;");
  assert.equal(first.id, again.id, "the same source keeps its script");
  assert.deepEqual(
    mock.scripts.map((script) => [script.key, script.source]),
    [[mock.scripts[0].key, "window.a = 1;"]],
    "one script, not two",
  );
  assert.match(mock.scripts[0].key, /^inline-[0-9a-f]{16}$/);
});

test("ready waits for the page; post and evaluate refuse without one", async () => {
  const mock = createMockHost({ pageAttached: false });
  const webview = granted(mock.host.webview);
  let settled = false;
  const waiting = webview.ready.then(() => {
    settled = true;
  });
  await assert.rejects(webview.post("greeting", { hello: true }), /page is not ready/);
  await assert.rejects(webview.evaluate("1+1"), /page is not ready/);
  assert.equal(settled, false);
  mock.page.attach();
  await waiting;
  assert.equal(settled, true);
  await webview.post("greeting", { hello: true });
  assert.deepEqual(mock.page.posted, [{ channel: "greeting", payload: { hello: true } }]);
});

test("a client without the bridge rejects like the runtime does", async () => {
  const mock = createMockHost({ bridge: "missing" });
  const webview = granted(mock.host.webview);
  await assert.rejects(webview.inject("a()"), /webview bridge installed/);
  await assert.rejects(webview.post("c", {}), /webview bridge installed/);
  await assert.rejects(webview.evaluate("1"), /webview bridge installed/);
  await assert.rejects(webview.ready, /webview bridge installed/);
  assert.throws(() => webview.on("c", () => {}), /webview bridge installed/);
});

test("page messages reach the handlers until they unsubscribe", async () => {
  const { mock, host } = await loadPlugin({});
  const webview = granted(host.webview);
  const seen: unknown[] = [];
  const off = webview.on("news", (payload) => seen.push(payload));
  mock.page.emit("news", { n: 1 });
  mock.page.emit("other", { n: 2 });
  off();
  mock.page.emit("news", { n: 3 });
  assert.deepEqual(seen, [{ n: 1 }]);
});

test("config keeps JSON values and removes keys set to undefined", async () => {
  const { mock, host } = await loadPlugin({}, { config: { start: 1 } });
  const config = granted(host.config);
  await config.set("answer", { n: 42 });
  assert.deepEqual(await config.get("answer"), { n: 42 });
  assert.equal(await config.get("start"), 1);
  await config.set("answer", undefined);
  assert.equal(await config.get("answer"), undefined);
  assert.deepEqual([...mock.config.keys()], ["start"]);
});

test("config hands out copies, so changing one does not write it back", async () => {
  const { host } = await loadPlugin({}, { config: { ladder: { max: 50 } } });
  const config = granted(host.config);
  const fetched = await config.get<{ max: number }>("ladder");
  assert.deepEqual(fetched, { max: 50 });
  fetched!.max = 999;
  assert.deepEqual(await config.get("ladder"), { max: 50 });
});

test("timer ids are numbers, like the runtime's", () => {
  const mock = createMockHost();
  const timeout = mock.host.setTimeout(() => {}, 1);
  const interval = mock.host.setInterval(() => {}, 1);
  assert.equal(typeof timeout, "number");
  assert.ok(timeout > 0 && interval > timeout);
  mock.host.clearTimeout(timeout);
  mock.host.clearInterval(interval);
  mock.dispose();
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
