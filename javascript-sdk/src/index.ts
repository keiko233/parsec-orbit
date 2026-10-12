/**
 * Types and helpers for Parsec Orbit JavaScript / TypeScript plugins.
 *
 * A plugin is an ES module whose default export is the result of
 * {@link definePlugin}; the runtime calls `setup(host)` and waits for it.
 * Code that runs in the client's web UI (the "page realm") is a separate
 * script, delivered with `host.webview`; its helper is `@parsec-orbit/sdk/ui`.
 */

/** One log method; arguments are joined with spaces, objects as JSON. */
export type LogMethod = (...args: unknown[]) => void;

export interface Log {
  trace: LogMethod;
  debug: LogMethod;
  info: LogMethod;
  warn: LogMethod;
  error: LogMethod;
}

/** What the runtime knows about the plugin. */
export interface PluginInfo {
  /** The manifest `id`. */
  readonly id: string;
  /** The manifest `version`, `"-"` when there is none. */
  readonly version: string;
  /** Absolute plugin directory. */
  readonly dir: string;
  /** Where the plugin may keep data; may not exist yet. Empty when unknown. */
  readonly dataDir: string;
}

export type Unsubscribe = () => void;

/** A script installed in the page. */
export interface ScriptHandle {
  readonly id: number;
  /** Stops the script from running in new documents. */
  remove(): Promise<void>;
}

export interface ScriptOptions {
  /**
   * Identifies the script. Injecting the same key again replaces the earlier
   * script; the default key depends on the source (or the file path).
   */
  key?: string;
}

/**
 * The client's web UI. Needs `permissions = ["webview"]` in `plugin.toml`;
 * without it `host.webview` is `undefined`.
 *
 * Everything maps onto the `parsec-orbit.webview` service of the webview
 * bridge plugin, which has to be installed (list it in `requires`).
 */
export interface WebView {
  /**
   * Resolves once a page is attached. Asking starts the connection, so do not
   * `await` it in `setup`: the loader waits for `setup`, and the page may not
   * exist yet. `inject` works before the page does.
   */
  readonly ready: Promise<void>;
  /** Runs `source` in the current document and in every new one. */
  inject(source: string, options?: ScriptOptions): Promise<ScriptHandle>;
  /** Like {@link inject}, with the source read from the plugin directory. */
  injectFile(path: string, options?: ScriptOptions): Promise<ScriptHandle>;
  /** Evaluates an expression in the page; the result must be JSON-compatible. */
  evaluate<T = unknown>(expression: string): Promise<T>;
  /** Calls `handler` for each `orbit.emit(channel, payload)` of the page. */
  on(channel: string, handler: (payload: unknown) => void): Unsubscribe;
  /** Delivers `payload` to the page's `orbit.on(channel, ...)` handlers. */
  post(channel: string, payload?: unknown): Promise<void>;
}

/**
 * Settings that survive restarts. Needs `permissions = ["config"]`. Stored as
 * JSON in the plugin's data directory.
 */
export interface Config {
  get<T = unknown>(key: string): Promise<T | undefined>;
  /** Setting `undefined` removes the key. */
  set(key: string, value: unknown): Promise<void>;
}

/** What `setup` receives. */
export interface Host {
  readonly plugin: PluginInfo;
  readonly log: Log;
  /** `undefined` without the `config` permission. */
  readonly config?: Config;
  /** `undefined` without the `webview` permission. */
  readonly webview?: WebView;
  setTimeout(callback: (...args: any[]) => void, delay?: number, ...args: any[]): number;
  clearTimeout(id: number): void;
  setInterval(callback: (...args: any[]) => void, delay?: number, ...args: any[]): number;
  clearInterval(id: number): void;
}

/** The default export of a plugin. */
export interface PluginDefinition {
  /**
   * Called once. May be async; the plugin counts as loaded when it settles
   * (a rejection fails the plugin) or after 30 seconds. Register things and
   * return: later plugins wait for this.
   */
  setup?(host: Host): void | Promise<void>;
  /** Reserved for a future unload; never called today. */
  teardown?(): void | Promise<void>;
}

/**
 * Declares a plugin. It returns its argument; it exists so that editors
 * type-check `setup`.
 *
 * ```ts
 * export default definePlugin({
 *   async setup(host) {
 *     host.log.info("loaded", host.plugin.version);
 *   },
 * });
 * ```
 */
export function definePlugin(definition: PluginDefinition): PluginDefinition {
  return definition;
}
