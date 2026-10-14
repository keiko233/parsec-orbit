//! One Boa context on a thread of its own.
//!
//! A Boa context is not `Send`, so each JavaScript plugin (and each context
//! made through `parsec-orbit.js`) gets a thread that owns it and an inbox of
//! [`Command`]s - an actor, without an async runtime. The thread runs one loop:
//! due timers, pending promise jobs, progress of a plugin load or an
//! evaluation, then a wait for the next command or the next timer.
//!
//! Everything JavaScript can reach about the outside world is in
//! [`crate::host`]; the state those functions share lives here, in a
//! thread-local, because they are plain function pointers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use boa_engine::builtins::promise::{OperationType, PromiseState, ResolvingFunctions};
use boa_engine::context::{ContextBuilder, HostHooks};
use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::object::JsObject;
use boa_engine::{Context as JsContext, JsError, JsResult, JsValue, Module, Source};
use parsec_orbit_plugin_sdk::services::{ConfigClient, UiClient, WebViewClient};
use parsec_orbit_plugin_sdk::{Level, ERR_EXCEPTION, ERR_TIMEOUT};

/// Where a context reports what happens in it.
pub type Logger = Arc<dyn Fn(Level, &str) + Send + Sync>;

/// How long loading a plugin (evaluating the module and awaiting `setup`) may
/// take before it counts as failed. `setup` runs while the loader waits, so
/// it should register things and return.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Iterations a single loop may run, and nested calls, before the engine
/// throws.
const LOOP_LIMIT: u64 = 20_000_000;
const RECURSION_LIMIT: usize = 400;

/// How long an evaluation through `parsec-orbit.js` may take.
pub const EVAL_TIMEOUT: Duration = Duration::from_secs(10);

/// A JavaScript plugin: what the `host` object is built from.
#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
    pub dir: PathBuf,
    pub data_dir: Option<PathBuf>,
    pub entry: PathBuf,
    /// Capabilities the manifest asked for; anything else is refused.
    pub permissions: Vec<String>,
}

impl PluginInfo {
    pub fn permits(&self, permission: &str) -> bool {
        self.permissions.iter().any(|granted| granted == permission)
    }
}

/// What to start a context with.
#[derive(Clone)]
pub struct Spec {
    /// Names the thread.
    pub name: String,
    /// `Some` for a JavaScript plugin (it gets `host`), `None` for a context
    /// made through `parsec-orbit.js` (timers and `console` only).
    pub plugin: Option<PluginInfo>,
    pub log: Logger,
    /// The `parsec-orbit.webview` service, if the bridge is loaded.
    pub webview: Option<WebViewClient>,
    /// The `parsec-orbit.config` service, if the config plugin is loaded;
    /// otherwise `host.config` keeps a file of its own.
    pub config: Option<ConfigClient>,
    /// The parsec-orbit.ui service, if the UI plugin is loaded.
    pub ui: Option<UiClient>,
}

/// Why an evaluation failed, with the ABI code that goes with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalError {
    pub code: i32,
    pub message: String,
}

impl EvalError {
    fn exception(message: impl Into<String>) -> Self {
        EvalError {
            code: ERR_EXCEPTION,
            message: message.into(),
        }
    }
    fn timeout() -> Self {
        EvalError {
            code: ERR_TIMEOUT,
            message: "timed out".to_string(),
        }
    }
}

/// What the owner of a context can ask of it, and what its helpers report.
pub enum Command {
    /// Evaluate the entry module and run `setup`.
    Load {
        reply: Sender<Result<(), String>>,
    },
    /// Evaluate a script; a promise result is awaited. Replies with JSON.
    Eval {
        source: String,
        reply: Sender<Result<String, EvalError>>,
    },
    /// A helper thread finished work a promise is waiting for.
    Settled {
        id: u64,
        outcome: Result<String, String>,
    },
    /// The page sent a message on a channel this context subscribed to.
    Message {
        channel: String,
        payload: String,
    },
    /// An action event delivered from the UI service for a registered section.
    UiAction {
        section_id: String,
        action_json: String,
    },
    Stop,
}

/// The owner's handle on a running context.
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    /// Loads the plugin and waits for `setup`.
    pub fn load(&self) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        self.tx
            .send(Command::Load { reply })
            .map_err(|_| "the context is gone".to_string())?;
        match answer.recv_timeout(LOAD_TIMEOUT + Duration::from_secs(5)) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err("timed out loading the plugin".to_string()),
            Err(RecvTimeoutError::Disconnected) => Err("the context is gone".to_string()),
        }
    }

    /// Evaluates a script and returns the result as JSON.
    pub fn eval(&self, source: &str) -> Result<String, EvalError> {
        let (reply, answer) = mpsc::channel();
        self.tx
            .send(Command::Eval {
                source: source.to_string(),
                reply,
            })
            .map_err(|_| EvalError::exception("the context is gone"))?;
        match answer.recv_timeout(EVAL_TIMEOUT + Duration::from_secs(5)) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(EvalError::timeout()),
            Err(RecvTimeoutError::Disconnected) => Err(EvalError::exception("the context is gone")),
        }
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }

    /// Sends a command to the context; `false` if it is gone.
    pub fn send(&self, command: Command) -> bool {
        self.tx.send(command).is_ok()
    }
}

// ---- the state the natives share ------------------------------------------------

pub(crate) struct Timer {
    pub id: u64,
    pub due: Instant,
    pub repeat: Option<Duration>,
    pub callback: JsFunction,
    pub args: Vec<JsValue>,
}

/// A subscription to a page channel through the bridge.
pub(crate) struct Channel {
    pub bridge_id: i64,
    /// `(local id, handler)`
    pub handlers: Vec<(u64, JsFunction)>,
    /// The `user` pointer of the bridge subscription; freed once
    /// `unsubscribe` has returned, which the bridge makes safe.
    pub subscriber: *mut crate::host::Subscriber,
}

/// A subscription to actions of a UI section through parsec-orbit.ui.
pub(crate) struct UiSectionChannel {
    pub subscription_id: i64,
    pub handlers: Vec<(u64, JsFunction)>,
    pub subscriber: *mut crate::host::UiSubscriber,
}

pub(crate) struct State {
    pub spec: Spec,
    pub tx: Sender<Command>,
    pub stopped: Arc<AtomicBool>,
    /// Helper threads this context has in flight, capped by `host::task`.
    /// Per context, so one plugin cannot reject another plugin's calls.
    pub tasks: Arc<AtomicUsize>,
    pub timers: Vec<Timer>,
    pub next_timer: u64,
    pub pending: HashMap<u64, ResolvingFunctions>,
    pub next_pending: u64,
    pub channels: HashMap<String, Channel>,
    pub next_handler: u64,
    pub ui_sections: HashMap<String, UiSectionChannel>,
    pub next_ui_handler: u64,
    /// Rejections nobody handled (yet), reported after the jobs ran.
    pub rejected: Vec<JsObject<boa_engine::builtins::promise::Promise>>,
    /// The promise behind `host.webview.ready`, once asked for.
    pub ready: Option<JsPromise>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` on this thread's state. Never call JavaScript from inside `f`: it
/// may call back into a native that needs the state.
pub(crate) fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| {
        f(state
            .borrow_mut()
            .as_mut()
            .expect("the context state exists on its thread"))
    })
}

pub(crate) fn log(level: Level, message: &str) {
    let logger = with_state(|state| state.spec.log.clone());
    logger(level, message);
}

/// Turns an error into the text that goes into the log.
pub(crate) fn describe(error: &JsError, context: &mut JsContext) -> String {
    match error
        .clone()
        .into_opaque(context)
        .ok()
        .and_then(|value| value.as_object())
    {
        Some(object) => {
            let get = |name: &str, context: &mut JsContext| {
                object
                    .get(boa_engine::JsString::from(name), context)
                    .ok()
                    .filter(|value| !value.is_undefined())
                    .and_then(|value| value.to_string(context).ok())
                    .map(|text| text.to_std_string_escaped())
            };
            let name = get("name", context).unwrap_or_else(|| "Error".to_string());
            match get("message", context) {
                Some(message) if !message.is_empty() => format!("{name}: {message}"),
                _ => name,
            }
        }
        None => error.to_string(),
    }
}

/// Reports promise rejections nobody handled.
struct Hooks;

impl HostHooks for Hooks {
    fn promise_rejection_tracker(
        &self,
        promise: &JsObject<boa_engine::builtins::promise::Promise>,
        operation: OperationType,
        _context: &mut JsContext,
    ) {
        with_state(|state| match operation {
            OperationType::Reject => state.rejected.push(promise.clone()),
            OperationType::Handle => state.rejected.retain(|other| other != promise),
        });
    }
}

/// Logs the rejections that are still unhandled after the jobs ran.
fn report_rejections(context: &mut JsContext) {
    let rejected = with_state(|state| std::mem::take(&mut state.rejected));
    for promise in rejected {
        let reason = match JsPromise::from_object(promise.upcast()) {
            Ok(promise) => match promise.state() {
                PromiseState::Rejected(reason) => reason,
                _ => continue,
            },
            Err(_) => continue,
        };
        let text = describe(&JsError::from_opaque(reason), context);
        log(
            Level::Error,
            &format!("unhandled promise rejection: {text}"),
        );
    }
}

// ---- a load in progress -----------------------------------------------------------

enum Load {
    /// The module is evaluating (it may have top-level `await`).
    Module {
        module: Module,
        promise: JsPromise,
        reply: Sender<Result<(), String>>,
        deadline: Instant,
    },
    /// `setup` returned a promise.
    Setup {
        promise: JsPromise,
        reply: Sender<Result<(), String>>,
        deadline: Instant,
    },
}

struct EvalWait {
    promise: JsPromise,
    reply: Sender<Result<String, EvalError>>,
    deadline: Instant,
}

/// Starts a context on a new thread.
pub fn spawn(spec: Spec) -> std::io::Result<Handle> {
    let (tx, inbox) = mpsc::channel();
    let handle = Handle { tx: tx.clone() };
    std::thread::Builder::new()
        .name(thread_name(&spec.name))
        .spawn(move || run(spec, tx, inbox))?;
    Ok(handle)
}

/// A thread name from whatever the caller had: `Builder::name` panics on a
/// control character, and the names travel into logs.
fn thread_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    if cleaned.is_empty() {
        "js-context".to_string()
    } else {
        cleaned
    }
}

fn run(spec: Spec, tx: Sender<Command>, inbox: Receiver<Command>) {
    let mut context = ContextBuilder::new()
        .host_hooks(Rc::new(Hooks))
        .build()
        .expect("a context can always be built");
    // A script that loops forever or recurses without end ends with an error
    // instead of taking its thread (and the plugin) with it.
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(LOOP_LIMIT);
    context
        .runtime_limits_mut()
        .set_recursion_limit(RECURSION_LIMIT);
    let stopped = Arc::new(AtomicBool::new(false));
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            spec: spec.clone(),
            tx,
            stopped: stopped.clone(),
            tasks: Arc::new(AtomicUsize::new(0)),
            timers: Vec::new(),
            next_timer: 0,
            pending: HashMap::new(),
            next_pending: 0,
            channels: HashMap::new(),
            next_handler: 0,
            ui_sections: HashMap::new(),
            next_ui_handler: 0,
            rejected: Vec::new(),
            ready: None,
        });
    });
    crate::host::install_globals(&mut context);

    let mut load: Option<Load> = None;
    let mut evals: Vec<EvalWait> = Vec::new();

    loop {
        crate::host::run_due_timers(&mut context);
        pump(&mut context);
        advance_load(&mut load, &mut context);
        advance_evals(&mut evals, &mut context);

        let now = Instant::now();
        let mut wake = now + Duration::from_secs(1);
        if let Some(next) = with_state(|state| state.timers.iter().map(|timer| timer.due).min()) {
            wake = wake.min(next);
        }
        for deadline in load_deadline(&load)
            .into_iter()
            .chain(evals.iter().map(|wait| wait.deadline))
        {
            wake = wake.min(deadline);
        }
        let wait = wake.saturating_duration_since(now);

        match inbox.recv_timeout(wait) {
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Command::Load { reply }) => {
                if load.is_some() {
                    let _ = reply.send(Err("a load is already in progress".to_string()));
                } else {
                    load = begin_load(&mut context, reply);
                }
            }
            Ok(Command::Eval { source, reply }) => {
                if let Some(wait) = begin_eval(&mut context, &source, reply) {
                    evals.push(wait);
                }
            }
            Ok(Command::Settled { id, outcome }) => {
                crate::host::settle(&mut context, id, outcome);
            }
            Ok(Command::Message { channel, payload }) => {
                crate::host::deliver(&mut context, &channel, &payload);
            }
            Ok(Command::UiAction {
                section_id,
                action_json,
            }) => {
                crate::host::deliver_ui_action(&mut context, &section_id, &action_json);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }

    stopped.store(true, Ordering::Release);
    // A load or evaluation that is still waiting hears that we are going.
    match load.take() {
        Some(Load::Module { reply, .. }) | Some(Load::Setup { reply, .. }) => {
            let _ = reply.send(Err("the context was stopped".to_string()));
        }
        None => {}
    }
    for wait in evals {
        let _ = wait
            .reply
            .send(Err(EvalError::exception("the context was stopped")));
    }
    crate::host::unsubscribe_all();
    crate::host::unsubscribe_ui_all();
    STATE.with(|state| *state.borrow_mut() = None);
}

fn load_deadline(load: &Option<Load>) -> Option<Instant> {
    match load {
        Some(Load::Module { deadline, .. }) | Some(Load::Setup { deadline, .. }) => Some(*deadline),
        None => None,
    }
}

/// Runs the promise jobs and reports what failed in them.
fn pump(context: &mut JsContext) {
    if let Err(error) = context.run_jobs() {
        let text = describe(&error, context);
        log(Level::Error, &format!("uncaught error: {text}"));
    }
    report_rejections(context);
}

// ---- loading a plugin -------------------------------------------------------------

/// Evaluates the entry module. Returns the load to keep polling, or `None` if
/// it was answered already.
fn begin_load(context: &mut JsContext, reply: Sender<Result<(), String>>) -> Option<Load> {
    let plugin = with_state(|state| state.spec.plugin.clone());
    let Some(plugin) = plugin else {
        let _ = reply.send(Err("this context has no plugin".to_string()));
        return None;
    };
    // The entry is downloaded content: read it only up to a size the client
    // can afford, so a huge file is a load failure, not a large allocation.
    const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
    if let Ok(metadata) = std::fs::metadata(&plugin.entry) {
        if metadata.len() > MAX_ENTRY_BYTES {
            let _ = reply.send(Err(format!(
                "{}: the entry is larger than {} MiB",
                plugin.entry.display(),
                MAX_ENTRY_BYTES / (1024 * 1024)
            )));
            return None;
        }
    }
    let bytes = match std::fs::read(&plugin.entry) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = reply.send(Err(format!(
                "cannot read {}: {error}",
                plugin.entry.display()
            )));
            return None;
        }
    };
    let source = Source::from_bytes(&bytes).with_path(&plugin.entry);
    let module = match Module::parse(source, None, context) {
        Ok(module) => module,
        Err(error) => {
            let text = describe(&error, context);
            let _ = reply.send(Err(format!("{}: {text}", plugin.entry.display())));
            return None;
        }
    };
    let promise = module.load_link_evaluate(context);
    Some(Load::Module {
        module,
        promise,
        reply,
        deadline: Instant::now() + LOAD_TIMEOUT,
    })
}

fn advance_load(load: &mut Option<Load>, context: &mut JsContext) {
    let Some(current) = load.take() else {
        return;
    };
    *load = match current {
        Load::Module {
            module,
            promise,
            reply,
            deadline,
        } => match promise.state() {
            PromiseState::Pending => {
                if Instant::now() > deadline {
                    let _ = reply.send(Err("the module did not finish evaluating".to_string()));
                    None
                } else {
                    Some(Load::Module {
                        module,
                        promise,
                        reply,
                        deadline,
                    })
                }
            }
            PromiseState::Rejected(reason) => {
                let text = describe(&JsError::from_opaque(reason), context);
                let _ = reply.send(Err(format!("the module failed: {text}")));
                None
            }
            PromiseState::Fulfilled(_) => start_setup(&module, reply, deadline, context),
        },
        Load::Setup {
            promise,
            reply,
            deadline,
        } => match promise.state() {
            PromiseState::Pending => {
                if Instant::now() > deadline {
                    let _ = reply.send(Err("setup did not finish in time".to_string()));
                    None
                } else {
                    Some(Load::Setup {
                        promise,
                        reply,
                        deadline,
                    })
                }
            }
            PromiseState::Rejected(reason) => {
                let text = describe(&JsError::from_opaque(reason), context);
                let _ = reply.send(Err(format!("setup failed: {text}")));
                None
            }
            PromiseState::Fulfilled(_) => {
                let _ = reply.send(Ok(()));
                None
            }
        },
    };
}

/// The module ran: call `setup(host)` on its default export.
fn start_setup(
    module: &Module,
    reply: Sender<Result<(), String>>,
    deadline: Instant,
    context: &mut JsContext,
) -> Option<Load> {
    let plugin = with_state(|state| state.spec.plugin.clone())?;
    let default = module
        .namespace(context)
        .get(boa_engine::js_string!("default"), context);
    let default = match default {
        Ok(value) => value,
        Err(error) => {
            let text = describe(&error, context);
            let _ = reply.send(Err(format!("no default export: {text}")));
            return None;
        }
    };
    let Some(export) = default.as_object() else {
        let _ = reply.send(Err(
            "the entry has no default export (export default definePlugin({ ... }))".to_string(),
        ));
        return None;
    };
    let setup = match export.get(boa_engine::js_string!("setup"), context) {
        Ok(value) => value,
        Err(error) => {
            let text = describe(&error, context);
            let _ = reply.send(Err(format!("reading setup: {text}")));
            return None;
        }
    };
    if setup.is_undefined() {
        // A plugin may have no setup; there is nothing to wait for.
        let _ = reply.send(Ok(()));
        return None;
    }
    let Some(setup) = setup.as_callable() else {
        let _ = reply.send(Err("`setup` is not a function".to_string()));
        return None;
    };
    let host = match crate::host::build_host(context, &plugin) {
        Ok(host) => host,
        Err(error) => {
            let text = describe(&error, context);
            let _ = reply.send(Err(format!("building the host object: {text}")));
            return None;
        }
    };
    match setup.call(&default, &[host.into()], context) {
        Err(error) => {
            let text = describe(&error, context);
            let _ = reply.send(Err(format!("setup threw: {text}")));
            None
        }
        Ok(value) => match value
            .as_object()
            .and_then(|object| JsPromise::from_object(object).ok())
        {
            Some(promise) => Some(Load::Setup {
                promise,
                reply,
                deadline,
            }),
            None => {
                let _ = reply.send(Ok(()));
                None
            }
        },
    }
}

// ---- evaluating scripts -------------------------------------------------------------

fn to_json(value: &JsValue, context: &mut JsContext) -> Result<String, EvalError> {
    match value.to_json(context) {
        Ok(Some(json)) => Ok(json.to_string()),
        Ok(None) => Ok("null".to_string()),
        Err(error) => Err(EvalError::exception(describe(&error, context))),
    }
}

fn begin_eval(
    context: &mut JsContext,
    source: &str,
    reply: Sender<Result<String, EvalError>>,
) -> Option<EvalWait> {
    let result: JsResult<JsValue> = context.eval(Source::from_bytes(source));
    match result {
        Err(error) => {
            let _ = reply.send(Err(EvalError::exception(describe(&error, context))));
            None
        }
        Ok(value) => match value
            .as_object()
            .and_then(|object| JsPromise::from_object(object).ok())
        {
            Some(promise) => Some(EvalWait {
                promise,
                reply,
                deadline: Instant::now() + EVAL_TIMEOUT,
            }),
            None => {
                let _ = reply.send(to_json(&value, context));
                None
            }
        },
    }
}

fn advance_evals(evals: &mut Vec<EvalWait>, context: &mut JsContext) {
    let mut keep = Vec::new();
    for wait in evals.drain(..) {
        match wait.promise.state() {
            PromiseState::Pending => {
                if Instant::now() > wait.deadline {
                    let _ = wait.reply.send(Err(EvalError::timeout()));
                } else {
                    keep.push(wait);
                }
            }
            PromiseState::Fulfilled(value) => {
                let _ = wait.reply.send(to_json(&value, context));
            }
            PromiseState::Rejected(reason) => {
                let text = describe(&JsError::from_opaque(reason), context);
                let _ = wait.reply.send(Err(EvalError::exception(text)));
            }
        }
    }
    *evals = keep;
}
