/**
 * Runs a plugin's `setup` under Node, without a client: a mock {@link Host}
 * with an in-memory webview.
 *
 * ```ts
 * import { loadPlugin } from "@parsec-orbit/sdk/testing";
 * import plugin from "../src/main.js";
 *
 * const { mock } = await loadPlugin(plugin);
 * assert.deepEqual(mock.scripts.map((s) => s.key), ["ui"]);
 * mock.page.emit("bandwidth:applied", { added: 10 });   // as if the page said so
 * ```
 */

import type {
  Config,
  Host,
  Log,
  PluginDefinition,
  PluginInfo,
  ScriptHandle,
  ScriptOptions,
  Unsubscribe,
  WebView,
} from "./index.js";

export type Level = "trace" | "debug" | "info" | "warn" | "error";

export interface LogLine {
  level: Level;
  message: string;
}

export interface MockScript {
  id: number;
  /** The key as the plugin gave it, or the default derived by the mock. */
  key: string;
  source: string;
  /** The file it was read from, for `injectFile`. */
  file?: string;
  removed: boolean;
}

export interface MockOptions {
  /** Granted permissions; defaults to `["webview", "config"]`. */
  permissions?: Array<"webview" | "config">;
  plugin?: Partial<PluginInfo>;
  /** Contents of files `injectFile` may read, by relative path. */
  files?: Record<string, string>;
  /** Answers `evaluate`; the default returns the expression as a string. */
  evaluate?: (expression: string) => unknown | Promise<unknown>;
  /** Whether `host.webview.ready` is already resolved. Default `true`. */
  pageAttached?: boolean;
  /**
   * Whether the webview bridge is installed. `"missing"` behaves like a
   * client without the bridge plugin: every webview call rejects, and so does
   * `ready`. Default `"installed"`.
   */
  bridge?: "installed" | "missing";
  /** Initial contents of `host.config`. */
  config?: Record<string, unknown>;
}

/** The page, as far as the plugin can tell. */
export interface MockPage {
  /** Attaches the page: resolves `host.webview.ready`. */
  attach(): void;
  /** What the page's `orbit.emit(channel, payload)` would deliver. */
  emit(channel: string, payload?: unknown): void;
  /** Messages the plugin sent with `host.webview.post`. */
  readonly posted: Array<{ channel: string; payload: unknown }>;
  /** Expressions the plugin evaluated. */
  readonly evaluated: string[];
}

export interface MockHost {
  readonly host: Host;
  readonly logs: LogLine[];
  readonly scripts: MockScript[];
  readonly page: MockPage;
  /** Current contents of `host.config`. */
  readonly config: Map<string, unknown>;
  /** Pending timers are cleared; call it at the end of a test. */
  dispose(): void;
}

const LEVELS: Level[] = ["trace", "debug", "info", "warn", "error"];

function show(value: unknown): string {
  if (typeof value === "string") return value;
  if (value !== null && typeof value === "object") {
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }
  return String(value);
}

/** The message the runtime uses when `requires` is missing the bridge. */
const NO_BRIDGE =
  "parsec-orbit.webview is not available (is the webview bridge installed?)";

/** FNV-1a 64, the same default script key the runtime derives. */
function fingerprint(text: string): string {
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(text)) {
    hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
  }
  return hash.toString(16).padStart(16, "0");
}

/** Builds a mock host. */
export function createMockHost(options: MockOptions = {}): MockHost {
  const permissions = options.permissions ?? ["webview", "config"];
  const bridged = (options.bridge ?? "installed") === "installed";
  const logs: LogLine[] = [];
  const scripts: MockScript[] = [];
  const files = options.files ?? {};
  const store = new Map<string, unknown>(Object.entries(options.config ?? {}));
  const timers = new Map<number, ReturnType<typeof setTimeout>>();
  const handlers = new Map<string, Set<(payload: unknown) => void>>();
  const posted: MockPage["posted"] = [];
  const evaluated: string[] = [];
  let nextScript = 0;
  let nextTimer = 0;
  let attached = options.pageAttached ?? true;
  let resolveReady: () => void = () => {};
  const ready = new Promise<void>((resolve) => {
    resolveReady = resolve;
  });
  if (attached) resolveReady();

  const requireBridge = (): void => {
    if (!bridged) throw new Error(NO_BRIDGE);
  };
  const requirePage = (): void => {
    requireBridge();
    if (!attached) throw new Error("the page is not ready");
  };

  const log = Object.fromEntries(
    LEVELS.map((level) => [
      level,
      (...args: unknown[]) => logs.push({ level, message: args.map(show).join(" ") }),
    ]),
  ) as unknown as Log;

  const plugin: PluginInfo = {
    id: "com.example.test",
    version: "0.0.0",
    dir: "/mock/plugin",
    dataDir: "/mock/data",
    ...options.plugin,
  };

  const add = (source: string, key: string, file?: string): ScriptHandle => {
    const existing = scripts.find((script) => script.key === key && !script.removed);
    if (existing) {
      existing.source = source;
      existing.file = file;
      return handleFor(existing);
    }
    const script: MockScript = { id: ++nextScript, key, source, file, removed: false };
    scripts.push(script);
    return handleFor(script);
  };
  const handleFor = (script: MockScript): ScriptHandle => ({
    id: script.id,
    remove: async () => {
      requireBridge();
      script.removed = true;
    },
  });
  const keyOf = (opts: ScriptOptions | undefined, fallback: string) =>
    opts?.key ? opts.key : fallback;

  const webview: WebView = {
    // A rejected promise only when asked for: a mock nothing reads must not
    // report an unhandled rejection.
    get ready() {
      return bridged ? ready : Promise.reject(new Error(NO_BRIDGE));
    },
    inject: async (source, opts) => {
      requireBridge();
      return add(source, keyOf(opts, `inline-${fingerprint(source)}`));
    },
    injectFile: async (path, opts) => {
      requireBridge();
      const normalised = path.replace(/\\/g, "/");
      if (!(normalised in files)) {
        throw new Error(`${path}: no such file in the plugin directory`);
      }
      return add(files[normalised], keyOf(opts, normalised), normalised);
    },
    evaluate: async <T>(expression: string) => {
      requirePage();
      evaluated.push(expression);
      const answer = options.evaluate ? await options.evaluate(expression) : expression;
      return JSON.parse(JSON.stringify(answer === undefined ? null : answer)) as T;
    },
    on: (channel, handler): Unsubscribe => {
      requireBridge();
      if (!handlers.has(channel)) handlers.set(channel, new Set());
      handlers.get(channel)!.add(handler);
      return () => handlers.get(channel)?.delete(handler);
    },
    post: async (channel, payload) => {
      requirePage();
      posted.push({ channel, payload: payload === undefined ? null : payload });
    },
  };

  const config: Config = {
    get: async <T>(key: string) => {
      const value = store.get(key);
      // The runtime reads its file afresh: a caller that mutates what it got
      // does not change what is stored.
      return (value === undefined
        ? undefined
        : JSON.parse(JSON.stringify(value))) as T | undefined;
    },
    set: async (key, value) => {
      if (value === undefined) store.delete(key);
      else store.set(key, JSON.parse(JSON.stringify(value)));
    },
  };

  const track =
    (original: typeof setTimeout | typeof setInterval) =>
    (callback: (...args: any[]) => void, delay?: number, ...args: any[]): number => {
      const id = ++nextTimer;
      timers.set(id, (original as typeof setTimeout)(callback, delay, ...args));
      return id;
    };
  const clear = (id: number, original: typeof clearTimeout | typeof clearInterval) => {
    const timer = timers.get(id);
    if (timer === undefined) return;
    timers.delete(id);
    original(timer);
  };

  const host: Host = {
    plugin,
    log,
    config: (permissions.includes("config") ? config : undefined) as unknown as Config,
    webview: (permissions.includes("webview") ? webview : undefined) as unknown as WebView,
    setTimeout: track(setTimeout),
    setInterval: track(setInterval),
    clearTimeout: (id) => clear(id, clearTimeout),
    clearInterval: (id) => clear(id, clearInterval),
  };

  const page: MockPage = {
    attach() {
      attached = true;
      resolveReady();
    },
    emit(channel, payload) {
      for (const handler of [...(handlers.get(channel) ?? [])]) {
        handler(payload === undefined ? null : JSON.parse(JSON.stringify(payload)));
      }
    },
    posted,
    evaluated,
  };

  return {
    host,
    logs,
    scripts,
    page,
    config: store,
    dispose() {
      for (const timer of timers.values()) {
        clearTimeout(timer);
        clearInterval(timer);
      }
      timers.clear();
    },
  };
}

/** Builds a mock host and runs `setup` on it. */
export async function loadPlugin(
  definition: PluginDefinition,
  options: MockOptions = {},
): Promise<{ mock: MockHost; host: Host }> {
  const mock = createMockHost(options);
  await definition.setup?.(mock.host);
  return { mock, host: mock.host };
}
