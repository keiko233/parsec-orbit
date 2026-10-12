// WebView bridge spy for the Parsec client UI.
//
// Paste this whole file into the DevTools console of the WebView2 page (see
// docs/webview-bridge.md for how to attach), or evaluate it over the remote
// debugging endpoint. It replaces the two bridge entry points with logging
// wrappers:
//
//   window.MTY_NativeSendText   UI -> native
//   window.MTY_NativeListener   native -> UI
//
// Console helpers afterwards:
//   __bridge.stats()              counters and buffer size
//   __bridge.dump(20)             print the last 20 frames
//   __bridge.export()             all captured frames as JSON
//   __bridge.pause(true|false)    stop / start console printing
//   __bridge.filter(/appversn/)   only print frames matching a regex
//   __bridge.call('appversn')     send a raw RPC and await its reply
//   __bridge.restore()            put the original functions back
//
// The wrappers never alter traffic: the originals are always called, and any
// logging failure is swallowed so it cannot break the UI's own calls.
(() => {
  if (window.__bridge) {
    console.warn('[bridge] already installed - call __bridge.restore() first');
    return 'already installed';
  }

  const MAX_FRAMES = 1000;
  const state = { frames: [], in: 0, out: 0, paused: false, filter: null };

  const origSend = window.MTY_NativeSendText;
  const origRecv = window.MTY_NativeListener;

  if (typeof origSend !== 'function') {
    console.warn('[bridge] window.MTY_NativeSendText is missing - is this the WebView2 UI?');
  }
  if (typeof origRecv !== 'function') {
    console.warn('[bridge] window.MTY_NativeListener is missing - is the UI still loading?');
  }

  const parse = (value) => {
    try {
      return JSON.parse(value);
    } catch {
      return null;
    }
  };

  const record = (dir, raw) => {
    const text = typeof raw === 'string' ? raw : String(raw);
    const obj = parse(text);
    const frame = { time: new Date().toISOString(), dir, raw: text, json: obj };

    state.frames.push(frame);
    if (state.frames.length > MAX_FRAMES) state.frames.shift();
    if (dir === 'out') state.out += 1;
    else state.in += 1;

    if (state.paused) return;
    if (state.filter && !state.filter.test(text)) return;

    const label = dir === 'out' ? 'JS  \u2192 native' : 'native \u2192 JS  ';
    const color = dir === 'out' ? '#2ecc71' : '#3498db';
    try {
      console.log('%c' + label, 'color:' + color + ';font-weight:bold',
        obj ? obj.event ?? obj.request_id ?? obj : text);
      if (obj) console.log(obj);
    } catch {
      console.log(label, text);
    }
  };

  window.MTY_NativeSendText = function (text) {
    try {
      record('out', text);
    } catch (e) { /* never break the real call */ }
    return origSend.apply(this, arguments);
  };

  window.MTY_NativeListener = function (data) {
    try {
      record('in', data);
    } catch (e) { /* never break the real call */ }
    return origRecv.apply(this, arguments);
  };

  window.__bridge = {
    frames: state.frames,
    stats: () => ({ sent: state.out, received: state.in, buffered: state.frames.length,
                    paused: state.paused, filter: state.filter ? String(state.filter) : null }),
    pause: (value = true) => { state.paused = !!value; return state.paused; },
    filter: (pattern) => { state.filter = pattern ? new RegExp(pattern) : null;
                           return state.filter ? String(state.filter) : null; },
    dump: (count = 50) => {
      for (const frame of state.frames.slice(-count)) {
        console.log(frame.time, frame.dir === 'out' ? 'JS -> native' : 'native -> JS',
          frame.json ?? frame.raw);
      }
      return state.frames.length;
    },
    export: () => JSON.stringify(state.frames, null, 2),
    call: (event, data, timeoutMs = 10000) => new Promise((resolve, reject) => {
      const request_id = 'parsec_orbit_' + Math.random().toString(36).slice(2);
      const timer = setTimeout(() => {
        window.removeEventListener(request_id, onMessage);
        reject(new Error('timeout waiting for ' + event));
      }, timeoutMs);
      const onMessage = (evt) => {
        clearTimeout(timer);
        window.removeEventListener(request_id, onMessage);
        const msg = evt.detail;
        if (msg && msg.error) reject(Object.assign(new Error(msg.error.message || 'rpc error'), msg.error));
        else resolve(msg);
      };
      window.addEventListener(request_id, onMessage);
      window.MTY_NativeSendText(JSON.stringify({ protocol_version: 1, request_id, event, data }));
    }),
    restore: () => {
      window.MTY_NativeSendText = origSend;
      window.MTY_NativeListener = origRecv;
      delete window.__bridge;
      console.log('[bridge] restored');
      return 'restored';
    },
  };

  console.log('%c[bridge] spying on MTY_NativeSendText / MTY_NativeListener',
    'color:#f39c12;font-weight:bold');
  return 'installed';
})();
