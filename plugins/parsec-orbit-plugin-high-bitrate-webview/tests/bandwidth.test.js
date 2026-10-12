// Behaviour tests for assets/bandwidth.js, run by tests/bandwidth_js.rs with
// `node --test`. The script is evaluated in a fresh vm context against a tiny
// fake browser: a window with a controllable clock and event bus, and a
// document whose elements carry a React fiber chain like the real settings
// page does.

'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const SCRIPT = fs.readFileSync(path.join(__dirname, '..', 'assets', 'bandwidth.js'), 'utf8');

const BUILT_IN = [3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50];
const EXTRAS = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];

const ladder = (numbers = BUILT_IN) =>
  numbers.map(value => ({ value, display: `${value} mbps` }));

// An element whose fiber chain reaches a node carrying `values` after a few
// unrelated hops, like the dropdown inside the settings row.
function elementWithValues(values, hops = 5) {
  let fiber = { memoizedProps: { values }, return: null };
  for (let i = 0; i < hops; i += 1) fiber = { memoizedProps: { other: i }, return: fiber };
  return { ['__reactFiber$test']: fiber };
}

function elementWithoutFiber() {
  return {};
}

function createBrowser({ elements = {}, all = [], rpc = 'reply' } = {}) {
  const listeners = new Map();
  const timers = [];
  const sent = [];
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
  };
  window.window = window;
  window.CustomEvent = class CustomEvent {
    constructor(type, init = {}) {
      this.type = type;
      this.detail = init.detail;
    }
  };
  window.document = {
    getElementById: id => elements[id] ?? null,
    querySelectorAll: () => all,
  };
  // The script calls the bare identifiers too.
  window.setTimeout = window.setTimeout.bind(window);

  if (rpc !== 'absent') {
    window.MTY_NativeSendText = text => {
      const message = JSON.parse(text);
      sent.push(message);
      if (rpc === 'reply') {
        // The app answers asynchronously with the current configuration.
        Promise.resolve().then(() =>
          window.dispatchEvent(
            new window.CustomEvent(message.request_id, {
              detail: { data: { data: { encoder_bitrate: 5, marker: 'config' } } },
            })
          )
        );
      }
    };
  }

  const context = vm.createContext(window);
  return {
    window,
    timers,
    sent,
    dispatched,
    run: () => vm.runInContext(SCRIPT, context),
    listenerCount: name => listeners.get(name)?.size ?? 0,
    /** Fires a timer once, the way the browser would. */
    fire(timer) {
      timers.splice(timers.indexOf(timer), 1);
      timer.fn();
    },
    /** Lets promise continuations run. */
    async settle() {
      for (let i = 0; i < 10; i += 1) await Promise.resolve();
      await new Promise(resolve => setImmediate(resolve));
    },
    pending: () => timers.filter(timer => !timer.cancelled),
  };
}

const displays = values => values.map(v => v.display);
const numbers = values => values.map(v => Number(v.value));

test('the extra list matches the documented ladder', () => {
  const match = SCRIPT.match(/const EXTRA_MBPS = \[([^\]]*)\]/);
  assert.ok(match, 'EXTRA_MBPS literal not found');
  assert.deepEqual(
    match[1].split(',').map(s => Number(s.trim())),
    EXTRAS
  );
});

test('extends a mounted dropdown and replays the config update', async () => {
  const values = ladder();
  const browser = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(values) } });

  assert.equal(browser.run(), 'installed');
  await browser.settle();

  assert.deepEqual(numbers(values), [...BUILT_IN, ...EXTRAS]);
  assert.equal(values[12].display, '60 mbps');
  assert.equal(values.at(-1).display, '500 mbps');

  // It asked the app for the config using the app's own protocol...
  assert.equal(browser.sent.length, 1);
  assert.equal(browser.sent[0].protocol_version, 1);
  assert.equal(browser.sent[0].event, 'cfg_get_');
  assert.match(browser.sent[0].request_id, /^orbit_extend_/);
  assert.equal('data' in browser.sent[0], false);

  // ...and fed the answer back as the update event that re-renders the page.
  const updates = browser.dispatched.filter(event => event.type === 'parsec_config_update');
  assert.equal(updates.length, 1);
  // (Compared as JSON: the object was created inside the vm context.)
  assert.deepEqual(JSON.parse(JSON.stringify(updates[0].detail)), {
    data: { encoder_bitrate: 5, marker: 'config' },
  });

  const state = browser.window.__orbitBitrateExtend;
  assert.equal(state.installed, true);
  assert.equal(state.applied, true);
  assert.equal(state.added, EXTRAS.length);
});

test('a second evaluation changes nothing', async () => {
  const values = ladder();
  const browser = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(values) } });
  assert.equal(browser.run(), 'installed');
  await browser.settle();

  const before = JSON.stringify(values);
  const sentBefore = browser.sent.length;
  assert.equal(browser.run(), 'already installed');
  await browser.settle();

  assert.equal(JSON.stringify(values), before);
  assert.equal(browser.sent.length, sentBefore);
});

test('values that are already present are not duplicated, whatever their type', async () => {
  const values = ladder();
  values.push({ value: '100', display: '100 mbps' }, { value: 250, display: '250 mbps' });
  const browser = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(values) } });
  browser.run();
  await browser.settle();

  const all = numbers(values);
  assert.equal(new Set(all).size, all.length, `duplicates in ${all}`);
  assert.equal(all.length, BUILT_IN.length + EXTRAS.length);
});

test('a list that already holds everything is left alone and not re-rendered', async () => {
  const values = ladder([...BUILT_IN, ...EXTRAS]);
  const browser = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(values) } });
  browser.run();
  await browser.settle();

  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);
  assert.equal(browser.sent.length, 0);
  assert.equal(browser.window.__orbitBitrateExtend.applied, true);
});

test('waits for the settings page and applies once it mounts', async () => {
  const elements = {};
  const browser = createBrowser({ elements });
  browser.run();
  await browser.settle();

  const state = browser.window.__orbitBitrateExtend;
  assert.equal(state.applied, false);
  assert.equal(state.attempts, 1);
  const retry = browser.pending();
  assert.equal(retry.length, 1);
  assert.equal(retry[0].delay, 500, 'polls twice a second');

  // The page mounts later.
  const values = ladder();
  elements['Bandwidth Limit'] = elementWithValues(values);
  browser.fire(retry[0]);
  await browser.settle();

  assert.equal(state.applied, true);
  assert.equal(state.attempts, 2);
  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);

  // One more tick is already queued; it must notice the work is done and not
  // count an attempt or queue anything further.
  const [last] = browser.pending();
  assert.ok(last);
  browser.fire(last);
  await browser.settle();
  assert.equal(state.attempts, 2, 'stops polling once applied');
  assert.equal(browser.pending().length, 0);
});

test('gives up after 1200 attempts', async () => {
  const browser = createBrowser();
  browser.run();
  for (let guard = 0; guard < 5000; guard += 1) {
    const [next] = browser.pending();
    if (!next) break;
    browser.fire(next);
    await browser.settle();
  }
  assert.equal(browser.window.__orbitBitrateExtend.attempts, 1200);
  assert.equal(browser.window.__orbitBitrateExtend.applied, false);
  assert.equal(browser.pending().length, 0);
});

test('falls back to scanning every element when the row id changes', async () => {
  const values = ladder();
  const browser = createBrowser({
    elements: {},
    all: [elementWithoutFiber(), elementWithValues([{ value: 1, display: 'unrelated' }]), elementWithValues(values)],
  });
  browser.run();
  await browser.settle();
  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);
});

test('ignores value lists that are not bandwidth ladders', async () => {
  const other = [{ value: 'a', display: 'English' }, { value: 'b', display: 'Deutsch' }];
  const browser = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(other) } });
  browser.run();
  await browser.settle();

  assert.equal(other.length, 2);
  assert.equal(browser.window.__orbitBitrateExtend.applied, false);
});

test('does not walk further than 120 fibers up', async () => {
  const values = ladder();
  const deep = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(values, 130) } });
  deep.run();
  await deep.settle();
  assert.equal(values.length, BUILT_IN.length, 'too deep to be found');

  const near = ladder();
  const ok = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(near, 100) } });
  ok.run();
  await ok.settle();
  assert.equal(near.length, BUILT_IN.length + EXTRAS.length);
});

test('still patches the list when the native bridge is missing', async () => {
  const values = ladder();
  const browser = createBrowser({
    elements: { 'Bandwidth Limit': elementWithValues(values) },
    rpc: 'absent',
  });
  assert.equal(browser.run(), 'installed');
  await browser.settle();

  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);
  assert.equal(browser.dispatched.filter(e => e.type === 'parsec_config_update').length, 0);
  assert.equal(browser.window.__orbitBitrateExtend.applied, true);
});

test('gives up on a native bridge that never answers after 5 seconds', async () => {
  const values = ladder();
  const browser = createBrowser({
    elements: { 'Bandwidth Limit': elementWithValues(values) },
    rpc: 'silent',
  });
  browser.run();
  await browser.settle();

  const timeout = browser.pending().find(timer => timer.delay === 5000);
  assert.ok(timeout, 'a 5 second timeout guards the request');
  assert.equal(browser.sent.length, 1);

  timeout.fn();
  await browser.settle();
  assert.equal(browser.dispatched.filter(e => e.type === 'parsec_config_update').length, 0);
  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);
});

test('does not replay anything when the app answers without config data', async () => {
  const values = ladder();
  const browser = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(values) } });
  // The app answers, but without any config data.
  browser.window.MTY_NativeSendText = text => {
    const message = JSON.parse(text);
    Promise.resolve().then(() =>
      browser.window.dispatchEvent(new browser.window.CustomEvent(message.request_id, { detail: { data: {} } }))
    );
  };
  browser.run();
  await browser.settle();

  assert.equal(values.length, BUILT_IN.length + EXTRAS.length);
  assert.equal(browser.dispatched.filter(e => e.type === 'parsec_config_update').length, 0);
});

test('the per-request listener and its timeout are cleaned up after an answer', async () => {
  const values = ladder();
  const browser = createBrowser({ elements: { 'Bandwidth Limit': elementWithValues(values) } });
  browser.run();
  await browser.settle();

  const requestId = browser.sent[0].request_id;
  assert.equal(browser.listenerCount(requestId), 0, 'no listener is left behind');
  const timeout = browser.timers.find(timer => timer.delay === 5000);
  assert.ok(timeout && timeout.cancelled, 'the 5 second timeout was cancelled');
});

test('the script is a single expression that returns its status', () => {
  assert.match(SCRIPT.trim(), /^\/\/[\s\S]*\(\(\) => \{[\s\S]*\}\)\(\);$/);
});
