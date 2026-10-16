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


/** Semantic icon names. Each renderer maps them to its platform symbol set. */
export type UiIconName =
  | "home" | "plugin" | "log" | "network" | "video" | "audio" | "input" | "gamepad"
  | "display" | "speed" | "security" | "server" | "settings" | "info" | "warning"
  | "error" | "success" | "refresh" | "folder" | "copy" | "play" | "stop" | "restart"
  | "download" | "delete" | "edit" | "link" | "clock" | "tools";

/** How serious a banner or a setting's status is. */
export type UiSeverity = "info" | "success" | "warning" | "error";

/** How prominent a command button is. Default `"standard"`. */
export type UiButtonStyle = "standard" | "accent" | "destructive";

/** One choice of a `select` control. `value` is a string or an integer. */
export interface UiOption {
  label: string;
  value: string | number;
  description?: string;
}

/** The control of a setting; its `type` fixes the value type. */
export type UiControl =
  /** A switch. Value: `boolean`. */
  | { type: "toggle"; default: boolean; on_text?: string; off_text?: string }
  /** A numeric field with spin buttons. Value: `number`. */
  | {
      type: "number";
      default: number;
      min?: number;
      max?: number;
      step?: number;
      unit?: string;
      integer?: boolean;
    }
  /** A range slider showing its value. Value: `number`. */
  | { type: "slider"; default: number; min: number; max: number; step?: number; unit?: string }
  /** A choice from a fixed list. Value: the selected option's `value`. */
  | { type: "select"; default: string | number; options: UiOption[] }
  /** Editable text. Value: `string`. `commit` defaults to `"submit"` (Enter or focus loss). */
  | {
      type: "text";
      default?: string;
      placeholder?: string;
      multiline?: boolean;
      rows?: number;
      monospace?: boolean;
      commit?: "submit" | "change";
    }
  /** A full-width editor for structured values. Value: object or array. */
  | { type: "json"; default?: Record<string, unknown> | unknown[] }
  /** A command button on the setting's row. Emits `click`; holds no value. */
  | { type: "button"; text: string; style?: UiButtonStyle; icon?: UiIconName }
  /** Read-only text. Value: `string`. Never emits an action. */
  | { type: "value"; default?: string; monospace?: boolean; copyable?: boolean }
  /** A read-only, scrolling, monospace region of at most 256 KiB. Value: `string`. */
  | { type: "log"; default?: string; rows?: number };

/** One thing the user can see or change. */
export interface UiSetting {
  /** Identity within the page, also the key in actions and patches. */
  key: string;
  label: string;
  description?: string;
  icon?: UiIconName;
  control: UiControl;
  /** Shown when the setting is expanded. One level only. */
  settings?: UiSetting[];
}

/** A command button in an `actions` block. */
export interface UiButton {
  key: string;
  text: string;
  icon?: UiIconName;
  style?: UiButtonStyle;
}

/** One block of a page. */
export type UiBlock =
  /** An inline notice. */
  | { type: "banner"; key: string; severity: UiSeverity; title?: string; message: string }
  /** A titled run of settings. */
  | { type: "group"; title?: string; description?: string; settings: UiSetting[] }
  /** A row of command buttons. */
  | { type: "actions"; buttons: UiButton[] };

/** One navigation destination registered with the UI service. */
export interface UiPage {
  schema: 2;
  /** 1-128 characters of `A-Z a-z 0-9 _ . -`. The `orbit.` prefix is reserved. */
  id: string;
  title: string;
  description?: string;
  icon?: UiIconName;
  /** Plugin pages always belong to the `plugins` group; `orbit` is reserved. */
  group?: "plugins";
  /** Lower comes first within a group. Default `0`. */
  order?: number;
  blocks: UiBlock[];
}

/** Status text shown with a setting, such as a validation error. */
export interface UiStatus {
  severity: UiSeverity;
  text: string;
}

/** Presentation changes for one element; absent members change nothing. */
export interface UiProps {
  enabled?: boolean;
  visible?: boolean;
  /** Shows progress and disables the control. */
  busy?: boolean;
  label?: string;
  description?: string;
  /** The text of a button. */
  text?: string;
  /** Replaces a `select`'s options. */
  options?: UiOption[];
  /** `null` clears the status. */
  status?: UiStatus | null;
  /** Banner only. */
  severity?: UiSeverity;
  title?: string;
  message?: string;
}

/** The argument of {@link Ui.updatePage}. A patch applies entirely or not at all. */
export interface UiPatch {
  /** New control values by setting key. */
  values?: Record<string, unknown>;
  /** Presentation changes by banner, setting or button key. */
  props?: Record<string, UiProps>;
  /** Whether this page has changes that apply only after a client restart. */
  restart_required?: boolean;
}

/** A user interaction with a control in the UI window. */
export interface UiAction {
  page: string;
  key: string;
  /** `"change"` carries the new value; `"click"` carries `null`. */
  type: "change" | "click";
  value?: unknown;
}

/**
 * Native companion window. Needs `permissions = ["ui"]` in `plugin.toml`;
 * without it `host.ui` is `undefined`.
 *
 * Everything maps onto the `parsec-orbit.ui` service, version 2, which has to
 * be installed (list `com.parsec-orbit.ui` in `requires`).
 */
export interface Ui {
  /** Whether the companion window is currently visible. */
  isVisible(): Promise<boolean>;
  /** Shows and brings the companion window to the foreground. */
  show(): Promise<void>;
  /** Hides the companion window without stopping its event loop. */
  hide(): Promise<void>;
  /** Selects a registered page in the window. Does not change visibility. */
  navigate(pageId: string): Promise<void>;
  /** Registers a declarative page. Rejects when the page breaks a schema rule. */
  registerPage(page: UiPage): Promise<void>;
  /** Removes a registered page and unmounts its controls. */
  unregisterPage(id: string): Promise<void>;
  /** Applies a patch of values, presentation properties and the restart flag. */
  updatePage(pageId: string, patch: UiPatch): Promise<void>;
  /** Calls `handler` for each user interaction with a control on the page. */
  onAction(pageId: string, handler: (action: UiAction) => void): Unsubscribe;
}

/** What `setup` receives. */
export interface Host {
  readonly plugin: PluginInfo;
  readonly log: Log;
  /** `undefined` without the `config` permission. */
  readonly config?: Config;
  /** `undefined` without the `webview` permission. */
  readonly webview?: WebView;
  /** `undefined` without the `ui` permission. */
  readonly ui?: Ui;
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
