// The page realm in a fake browser: a window, a document whose element carries
// a React fiber chain like the settings page, and the real `window.__orbit`
// helper of the webview bridge.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const SCRIPT = readFileSync(new URL("../dist/ui.js", import.meta.url), "utf8");
const ORBIT = readFileSync(
  new URL("../../../libs/parsec-orbit-lib-webview-bridge/assets/orbit.js", import.meta.url),
  "utf8",
);

const BUILT_IN = [3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50];
const EXTRAS = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];
const ladder = () => BUILT_IN.map((value) => ({ value, display: `${value} mbps` }));

function elementWith(values) {
  let fiber = { memoizedProps: { values }, return: null };
  for (let i = 0; i < 4; i += 1) fiber = { memoizedProps: { other: i }, return: fiber };
  return { ["__reactFiber$test"]: fiber };
}

function browser({ elements = {}, native = true } = {}) {
  const listeners = new Map();
  const timers = [];
  const emitted = [];
  const dispatched = [];
  const window = {
    setTimeout(fn, delay) {
      const timer = { fn, delay, cancelled: false };
      timers.push(timer);
      return timer;
    },
    clearTimeout(timer) {
      if (timer) timer.cancelled = true;
    },
    addEventListener(name, handler) {
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(handler);
    },
    removeEventListener(name, handler) {
      listeners.get(name)?.delete(handler);
    },
    dispatchEvent(event) {
      dispatched.push(event);
      for (const handler of [...(listeners.get(event.type) ?? [])]) handler(event);
    },
    // The CDP binding the bridge adds.
    __orbitSend(text) {
      emitted.push(JSON.parse(text));
    },
  };
  window.window = window;
  window.CustomEvent = class {
    constructor(type, init = {}) {
      this.type = type;
      this.detail = init.detail;
    }
  };
  window.document = { getElementById: (id) => elements[id] ?? null, querySelectorAll: () => [] };
  window.setTimeout = window.setTimeout.bind(window);
  if (native) {
    window.MTY_NativeSendText = (text) => {
      const message = JSON.parse(text);
      Promise.resolve().then(() =>
        window.dispatchEvent(
          new window.CustomEvent(message.request_id, {
            detail: { data: { data: { encoder_bitrate: 5 } } },
          }),
        ),
      );
    };
  }
  const context = vm.createContext(window);
  vm.runInContext(ORBIT, context);
  return {
    window,
    emitted,
    dispatched,
    run: () => vm.runInContext(SCRIPT, context),
    settle: async () => {
      for (let i = 0; i < 10; i += 1) await Promise.resolve();
      await new Promise((resolve) => setImmediate(resolve));
    },
  };
}

test("extends a mounted dropdown, replays the config update and tells the plugin", async () => {
  const values = ladder();
  const page = browser({ elements: { "Bandwidth Limit": elementWith(values) } });
  assert.equal(page.run(), "installed");
  await page.settle();

  assert.deepEqual(values.map((v) => Number(v.value)), [...BUILT_IN, ...EXTRAS]);
  assert.equal(values.at(-1).display, "500 mbps");
  const updates = page.dispatched.filter((event) => event.type === "parsec_config_update");
  assert.equal(updates.length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(updates[0].detail)), { data: { encoder_bitrate: 5 } });
  assert.deepEqual(page.emitted, [{ channel: "bandwidth:applied", payload: { added: EXTRAS.length } }]);
});

test("a second evaluation changes nothing", async () => {
  const values = ladder();
  const page = browser({ elements: { "Bandwidth Limit": elementWith(values) } });
  page.run();
  await page.settle();
  assert.equal(page.run(), "already installed");
  await page.settle();
  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);
  assert.equal(page.emitted.length, 1);
});

test("values that are already there are not duplicated", async () => {
  const values = ladder().concat([{ value: "60", display: "60 mbps" }]);
  const page = browser({ elements: { "Bandwidth Limit": elementWith(values) } });
  page.run();
  await page.settle();
  assert.equal(values.filter((v) => String(v.value) === "60").length, 1);
  assert.equal(page.emitted[0].payload.added, EXTRAS.length - 1);
});

test("still patches the list when the native bridge is missing", async () => {
  const values = ladder();
  const page = browser({ elements: { "Bandwidth Limit": elementWith(values) }, native: false });
  page.run();
  await page.settle();
  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);
  assert.equal(page.dispatched.filter((e) => e.type === "parsec_config_update").length, 0);
  assert.equal(page.emitted.length, 1);
});

