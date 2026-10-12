/**
 * The page-realm helper: use it in scripts that the plugin delivers to the
 * client's web UI with `host.webview.inject*`. The webview bridge installs
 * `window.__orbit` before any plugin script runs; this module is a typed face
 * for it.
 *
 * ```ts
 * import { orbit } from "@parsec-orbit/sdk/ui";
 *
 * orbit.emit("bandwidth:applied", { added: 10 });      // -> host.webview.on
 * orbit.on("bandwidth:set", (payload) => { ... });      // <- host.webview.post
 * const config = await orbit.rpc("cfg_get_");           // Parsec UI <-> client
 * ```
 */

import type { UiRpc } from "./events.js";

export type Unsubscribe = () => void;

export interface Orbit {
  /** Version of the helper the bridge installed. */
  readonly version: number;
  /** Sends a message to the plugin. `false` if the bridge binding is missing. */
  emit(channel: string, payload?: unknown): boolean;
  /** Listens for what the plugin sends with `host.webview.post`. */
  on(channel: string, handler: (payload: unknown) => void): Unsubscribe;
  off(channel: string, handler: (payload: unknown) => void): void;
  /**
   * The Parsec web UI's request/reply protocol (`window.MTY_NativeSendText`).
   * Resolves with the reply; rejects if the bridge is missing or nothing
   * answers within `timeoutMs` (10 s by default).
   */
  rpc<T = unknown>(event: string, data?: unknown, timeoutMs?: number): Promise<T>;
}

declare global {
  interface Window {
    __orbit?: Orbit;
  }
}

function installed(): Orbit {
  const helper = typeof window === "undefined" ? undefined : window.__orbit;
  if (!helper) {
    throw new Error(
      "window.__orbit is missing: this script has to be delivered by the Parsec Orbit webview bridge",
    );
  }
  return helper;
}

/** Delegates to `window.__orbit` at call time, so importing is always safe. */
export const orbit: Orbit = {
  get version() {
    return installed().version;
  },
  emit: (channel, payload) => installed().emit(channel, payload),
  on: (channel, handler) => installed().on(channel, handler),
  off: (channel, handler) => installed().off(channel, handler),
  rpc: <T>(event: string, data?: unknown, timeoutMs?: number) =>
    installed().rpc(event, data, timeoutMs) as Promise<T>,
};

/** Typed `orbit.rpc` for the events listed in `@parsec-orbit/sdk/ui/events`. */
export function rpc<E extends keyof UiRpc>(
  event: E,
  ...data: UiRpc[E][0] extends undefined ? [] : [UiRpc[E][0]]
): Promise<UiRpc[E][1]> {
  return orbit.rpc(event, data[0]) as Promise<UiRpc[E][1]>;
}
