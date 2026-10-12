// Parsec Orbit: extend the WebView UI bandwidth-limit list beyond 50 Mbps.
//
// The UI builds the "Bandwidth Limit" dropdown from a hard-coded list that
// stops at 50 mbps, while the native config entry (`encoder_bitrate`) is a
// plain int with min 0 and max 2147483647, so any value can be stored. This
// script finds the settings array through the React fiber tree and appends
// higher entries, then refreshes the settings page state so the new entries
// show up in the open dropdown.
//
// It is written to be evaluated once per document (the loader injects it
// through the DevTools protocol). If the settings page is already mounted it
// re-renders it; otherwise the (single, module-level) array stays patched for
// the next time the page renders.
(() => {
  const MARK = '__orbitBitrateExtend';
  const existing = window[MARK];
  if (existing && existing.installed) return 'already installed';

  // Keep in sync with EXTRA_BANDWIDTH_MBPS in ../src/lib.rs.
  const EXTRA_MBPS = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];
  const state = (window[MARK] = {
    installed: true,
    applied: false,
    attempts: 0,
    extras: EXTRA_MBPS,
  });

  const rpc = (event, data, timeoutMs = 5000) =>
    new Promise(resolve => {
      if (typeof window.MTY_NativeSendText !== 'function') {
        resolve(null);
        return;
      }
      const request_id = 'orbit_extend_' + Math.random().toString(36).slice(2);
      const handler = evt => {
        clearTimeout(timer);
        window.removeEventListener(request_id, handler);
        resolve(evt.detail);
      };
      const timer = setTimeout(() => {
        window.removeEventListener(request_id, handler);
        resolve(null);
      }, timeoutMs);
      window.addEventListener(request_id, handler);
      const message = { protocol_version: 1, request_id, event };
      if (data !== undefined) message.data = data;
      window.MTY_NativeSendText(JSON.stringify(message));
    });

  const looksLikeBitrateValues = values =>
    Array.isArray(values) &&
    values.some(v => v && typeof v === 'object' && /mbps/i.test(String(v.display ?? '')));

  // Walks up the fiber tree from a DOM node looking for the props that carry
  // the dropdown's `values` array.
  const valuesFromElement = element => {
    if (!element) return null;
    const fiberKey = Object.keys(element).find(k => k.startsWith('__reactFiber$'));
    if (!fiberKey) return null;
    let fiber = element[fiberKey];
    for (let hops = 0; fiber && hops < 120; hops += 1, fiber = fiber.return) {
      const values = fiber.memoizedProps && fiber.memoizedProps.values;
      if (looksLikeBitrateValues(values)) return values;
    }
    return null;
  };

  const findValues = () => {
    const byId = valuesFromElement(document.getElementById('Bandwidth Limit'));
    if (byId) return byId;

    // Fallback if the row id ever changes: scan for any element whose fiber
    // chain points at a mbps list.
    const candidates = document.querySelectorAll('*');
    for (let i = 0; i < candidates.length; i += 1) {
      const values = valuesFromElement(candidates[i]);
      if (values) return values;
    }
    return null;
  };

  const apply = async () => {
    const values = findValues();
    if (!values) return false;

    let added = 0;
    for (const mbps of EXTRA_MBPS) {
      if (!values.some(v => String(v && v.value) === String(mbps))) {
        values.push({ value: mbps, display: `${mbps} mbps` });
        added += 1;
      }
    }

    state.applied = true;
    state.added = (state.added || 0) + added;
    if (added === 0) return true;

    // The settings page may already be mounted with the old list rendered.
    // Re-render it by replaying the app's own config-update event.
    const config = await rpc('cfg_get_');
    const data = config && config.data && config.data.data;
    if (data) {
      window.dispatchEvent(new CustomEvent('parsec_config_update', { detail: { data } }));
    }
    return true;
  };

  const tick = () => {
    if (state.applied) return;
    state.attempts += 1;
    apply().catch(() => {});
    if (state.attempts < 1200) window.setTimeout(tick, 500);
  };
  tick();

  return 'installed';
})();
