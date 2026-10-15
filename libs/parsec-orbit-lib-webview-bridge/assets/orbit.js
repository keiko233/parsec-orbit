// Parsec Orbit: the page side of the `parsec-orbit.webview` service.
//
// Installed by the webview bridge before any plugin script, in every document
// (Page.addScriptToEvaluateOnNewDocument) and in the current one. It defines
// `window.__orbit`:
//
//   emit(channel, payload)     page -> plugin   (host.webview.on / subscribe)
//   on(channel, handler)       plugin -> page   (host.webview.post / post)
//                              returns a function that removes the handler
//   off(channel, handler)
//   rpc(event, data, timeoutMs) the Parsec UI <-> client request/reply protocol
//                              of the web UI (protocol_version 1); resolves with
//                              the reply, rejects after `timeoutMs` (10 s)
//   version                    version of this helper
//
// `emit` goes through the CDP binding `__orbitSend` that the bridge adds with
// Runtime.addBinding; `post` arrives as a call of `__deliver`.
(() => {
  const VERSION = 1;
  const existing = window.__orbit;
  if (existing && existing.version >= VERSION) return 'already installed';

  const handlers = new Map();

  const emit = (channel, payload) => {
    const send = window.__orbitSend;
    if (typeof send !== 'function') return false;
    send(JSON.stringify({ channel: String(channel), payload: payload === undefined ? null : payload }));
    return true;
  };

  const off = (channel, handler) => {
    const set = handlers.get(String(channel));
    if (!set) return;
    set.delete(handler);
    if (set.size === 0) handlers.delete(String(channel));
  };

  const on = (channel, handler) => {
    if (typeof handler !== 'function') throw new TypeError('handler must be a function');
    const key = String(channel);
    if (!handlers.has(key)) handlers.set(key, new Set());
    handlers.get(key).add(handler);
    return () => off(key, handler);
  };

  const deliver = (channel, payload) => {
    const set = handlers.get(String(channel));
    if (!set) return 0;
    let called = 0;
    for (const handler of Array.from(set)) {
      called += 1;
      try {
        handler(payload);
      } catch (error) {
        console.error('[orbit] handler for "' + channel + '" threw', error);
      }
    }
    return called;
  };

  const rpc = (event, data, timeoutMs = 10000) =>
    new Promise((resolve, reject) => {
      if (typeof window.MTY_NativeSendText !== 'function') {
        reject(new Error('the Parsec UI bridge (MTY_NativeSendText) is not available'));
        return;
      }
      const request_id = 'orbit_' + Math.random().toString(36).slice(2);
      const handler = (evt) => {
        clearTimeout(timer);
        window.removeEventListener(request_id, handler);
        resolve(evt.detail);
      };
      const timer = setTimeout(() => {
        window.removeEventListener(request_id, handler);
        reject(new Error('rpc "' + event + '" timed out after ' + timeoutMs + ' ms'));
      }, timeoutMs);
      window.addEventListener(request_id, handler);
      const message = { protocol_version: 1, request_id, event };
      if (data !== undefined) message.data = data;
      window.MTY_NativeSendText(JSON.stringify(message));
    });

  Object.defineProperty(window, '__orbit', {
    value: { version: VERSION, emit, on, off, rpc, __deliver: deliver },
    configurable: true,
  });
  return 'installed';
})();
