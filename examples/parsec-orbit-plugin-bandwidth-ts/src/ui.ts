// The page realm: widens the "Bandwidth Limit" list of the web UI beyond 50
// Mbps. A TypeScript port of the official plugin's assets/bandwidth.js.
//
// The UI builds the dropdown from a hard-coded list, while the native config
// entry (`encoder_bitrate`) is a plain int that accepts any value. This script
// finds the list through the React fiber tree, appends higher entries, and
// replays the app's own config-update event so an open settings page shows
// them. It is idempotent and tells the plugin what it did with orbit.emit.

import { CONFIG_UPDATE_EVENT, type ConfigReply } from "@parsec-orbit/sdk/ui/events";
import { orbit } from "@parsec-orbit/sdk/ui";

const MARK = "__orbitBitrateExtendTs";

export const EXTRA_MBPS = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];

interface Option {
  value: unknown;
  display?: unknown;
}

interface State {
  installed: boolean;
  applied: boolean;
  attempts: number;
  added?: number;
}

const looksLikeBitrateValues = (values: unknown): values is Option[] =>
  Array.isArray(values) &&
  values.some((v) => v && typeof v === "object" && /mbps/i.test(String((v as Option).display ?? "")));

/** Walks up the fiber tree from a DOM node looking for the dropdown's `values`. */
function valuesFromElement(element: unknown): Option[] | null {
  if (!element) return null;
  const record = element as Record<string, any>;
  const fiberKey = Object.keys(record).find((k) => k.startsWith("__reactFiber$"));
  if (!fiberKey) return null;
  let fiber = record[fiberKey];
  for (let hops = 0; fiber && hops < 120; hops += 1, fiber = fiber.return) {
    const values = fiber.memoizedProps && fiber.memoizedProps.values;
    if (looksLikeBitrateValues(values)) return values;
  }
  return null;
}

function findValues(): Option[] | null {
  const byId = valuesFromElement(document.getElementById("Bandwidth Limit"));
  if (byId) return byId;
  // The row id may change: scan for any element whose fiber chain points at a
  // mbps list.
  const candidates = document.querySelectorAll("*");
  for (let i = 0; i < candidates.length; i += 1) {
    const values = valuesFromElement(candidates[i]);
    if (values) return values;
  }
  return null;
}

/** The app's config, or `null` when the native bridge does not answer. */
async function currentConfig(): Promise<Record<string, unknown> | null> {
  try {
    const reply = await orbit.rpc<ConfigReply>("cfg_get_", undefined, 5000);
    return reply?.data?.data ?? null;
  } catch {
    return null;
  }
}

async function apply(state: State): Promise<boolean> {
  const values = findValues();
  if (!values) return false;

  let added = 0;
  for (const mbps of EXTRA_MBPS) {
    if (!values.some((v) => String(v && v.value) === String(mbps))) {
      values.push({ value: mbps, display: `${mbps} mbps` });
      added += 1;
    }
  }

  state.applied = true;
  state.added = (state.added ?? 0) + added;
  if (added === 0) return true;

  // The settings page may already be mounted with the old list rendered:
  // re-render it by replaying the app's own config-update event.
  const data = await currentConfig();
  if (data) {
    window.dispatchEvent(new CustomEvent(CONFIG_UPDATE_EVENT, { detail: { data } }));
  }
  orbit.emit("bandwidth:applied", { added });
  return true;
}

function install(): string {
  const w = window as unknown as Record<string, State | undefined>;
  const existing = w[MARK];
  if (existing && existing.installed) return "already installed";

  const state: State = { installed: true, applied: false, attempts: 0 };
  w[MARK] = state;

  const tick = (): void => {
    if (state.applied) return;
    state.attempts += 1;
    apply(state).catch(() => {});
    if (state.attempts < 1200) window.setTimeout(tick, 500);
  };
  tick();
  return "installed";
}

/** What the page realm reports to whoever evaluates this script. */
export const status = install();
