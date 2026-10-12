//! What JavaScript can see: the globals every context has, and the `host`
//! object of a plugin.
//!
//! Everything here is a plain function pointer, so the state the functions
//! share is the thread-local one of [`crate::engine`]. Work that blocks (the
//! page evaluating a script) runs on a helper thread and settles its promise
//! through the context's inbox; the context itself never blocks on the
//! outside world.

use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::object::{JsObject, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context as JsContext, JsArgs, JsError, JsNativeError, JsResult, JsString, JsValue,
    NativeFunction,
};
use parsec_orbit_plugin_sdk_std::services::WebViewClient;
use parsec_orbit_plugin_sdk_std::Level;
use serde_json::Value;

use crate::engine::{describe, log, with_state, Channel, Command, PluginInfo, Timer};
use crate::loader::Permission;

type Native = fn(&JsValue, &[JsValue], &mut JsContext) -> JsResult<JsValue>;

fn function(native: Native) -> NativeFunction {
    NativeFunction::from_fn_ptr(native)
}

fn type_error(message: &str) -> JsError {
    JsNativeError::typ()
        .with_message(message.to_string())
        .into()
}

// ---- logging ---------------------------------------------------------------------

/// The text of one log argument: strings as they are, objects as JSON.
fn display(value: &JsValue, context: &mut JsContext) -> String {
    if let Some(text) = value.as_string() {
        return text.to_std_string_escaped();
    }
    if value.is_object() {
        if let Ok(Some(json)) = value.to_json(context) {
            return json.to_string();
        }
    }
    value
        .to_string(context)
        .map(|text| text.to_std_string_escaped())
        .unwrap_or_else(|_| "<unprintable>".to_string())
}

fn write_log(level: Level, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let parts: Vec<String> = args.iter().map(|value| display(value, context)).collect();
    log(level, &parts.join(" "));
    Ok(JsValue::undefined())
}

macro_rules! level_fn {
    ($name:ident, $level:expr) => {
        fn $name(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
            write_log($level, args, context)
        }
    };
}

level_fn!(log_trace, Level::Trace);
level_fn!(log_debug, Level::Debug);
level_fn!(log_info, Level::Info);
level_fn!(log_warn, Level::Warn);
level_fn!(log_error, Level::Error);

fn log_object(context: &mut JsContext) -> JsObject {
    ObjectInitializer::new(context)
        .function(function(log_trace), js_string!("trace"), 0)
        .function(function(log_debug), js_string!("debug"), 0)
        .function(function(log_info), js_string!("info"), 0)
        .function(function(log_warn), js_string!("warn"), 0)
        .function(function(log_error), js_string!("error"), 0)
        .build()
}

// ---- timers ----------------------------------------------------------------------

fn schedule(args: &[JsValue], context: &mut JsContext, repeat: bool) -> JsResult<JsValue> {
    let callback = args
        .get_or_undefined(0)
        .as_object()
        .and_then(JsFunction::from_object)
        .ok_or_else(|| type_error("the first argument must be a function"))?;
    let milliseconds = args.get_or_undefined(1).to_number(context).unwrap_or(0.0);
    // NaN and negative delays run as soon as possible; a day is the longest
    // a timer is worth waiting for.
    let milliseconds = if milliseconds.is_finite() && milliseconds > 0.0 {
        milliseconds.min(86_400_000.0) as u64
    } else {
        0
    };
    let delay = Duration::from_millis(milliseconds);
    let extra = args.iter().skip(2).cloned().collect();
    let id = with_state(|state| {
        state.next_timer += 1;
        let id = state.next_timer;
        state.timers.push(Timer {
            id,
            due: Instant::now() + delay,
            // An interval never spins faster than a millisecond.
            repeat: repeat.then_some(delay.max(Duration::from_millis(1))),
            callback,
            args: extra,
        });
        id
    });
    Ok(JsValue::from(id as f64))
}

fn set_timeout(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    schedule(args, context, false)
}

fn set_interval(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    schedule(args, context, true)
}

fn clear_timer(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let id = args.get_or_undefined(0).to_number(context).unwrap_or(0.0);
    with_state(|state| state.timers.retain(|timer| timer.id as f64 != id));
    Ok(JsValue::undefined())
}

/// Calls the timers that are due.
pub fn run_due_timers(context: &mut JsContext) {
    let now = Instant::now();
    let due: Vec<(JsFunction, Vec<JsValue>)> = with_state(|state| {
        let mut due = Vec::new();
        state.timers.retain_mut(|timer| {
            if timer.due > now {
                return true;
            }
            due.push((timer.callback.clone(), timer.args.clone()));
            match timer.repeat {
                Some(interval) => {
                    timer.due = now + interval;
                    true
                }
                None => false,
            }
        });
        due
    });
    for (callback, args) in due {
        if let Err(error) = callback.call(&JsValue::undefined(), &args, context) {
            let text = describe(&error, context);
            log(Level::Error, &format!("uncaught error in a timer: {text}"));
        }
    }
}

// ---- globals ---------------------------------------------------------------------

/// `console` and the timers, in every context.
pub fn install_globals(context: &mut JsContext) {
    let console = ObjectInitializer::new(context)
        .function(function(log_info), js_string!("log"), 0)
        .function(function(log_info), js_string!("info"), 0)
        .function(function(log_debug), js_string!("debug"), 0)
        .function(function(log_warn), js_string!("warn"), 0)
        .function(function(log_error), js_string!("error"), 0)
        .build();
    let _ = context.register_global_property(js_string!("console"), console, Attribute::all());
    for (name, length, native) in [
        ("setTimeout", 2, set_timeout as Native),
        ("setInterval", 2, set_interval as Native),
        ("clearTimeout", 1, clear_timer as Native),
        ("clearInterval", 1, clear_timer as Native),
    ] {
        let _ = context.register_global_callable(JsString::from(name), length, function(native));
    }
}

// ---- promises settled from other threads ------------------------------------------

/// A pending promise and the id a helper thread settles it with.
fn deferred(context: &mut JsContext) -> (JsPromise, u64) {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let id = with_state(|state| {
        state.next_pending += 1;
        let id = state.next_pending;
        state.pending.insert(id, resolvers);
        id
    });
    (promise, id)
}

/// Runs `work` on a helper thread and settles the returned promise with its
/// outcome: JSON on success (empty means `undefined`), a message on failure.
///
/// Every call blocks a thread, so a script that fires calls without awaiting
/// them cannot spawn unbounded work: past `MAX_TASKS` in flight for this
/// context the promise is rejected instead. The count is per context, so one
/// plugin cannot exhaust another plugin's budget.
fn task(
    context: &mut JsContext,
    work: impl FnOnce() -> Result<String, String> + Send + 'static,
) -> JsResult<JsValue> {
    const MAX_TASKS: usize = 32;
    let tasks = with_state(|state| Arc::clone(&state.tasks));

    if tasks.fetch_add(1, Ordering::SeqCst) >= MAX_TASKS {
        tasks.fetch_sub(1, Ordering::SeqCst);
        return rejected("too many webview calls in flight", context);
    }
    let (promise, id) = deferred(context);
    let tx = with_state(|state| state.tx.clone());
    let counter = Arc::clone(&tasks);
    let spawned = std::thread::Builder::new()
        .name("js-task".to_string())
        .spawn(move || {
            let outcome = work();
            counter.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(Command::Settled { id, outcome });
        });
    if spawned.is_err() {
        tasks.fetch_sub(1, Ordering::SeqCst);
        settle(
            context,
            id,
            Err("could not start a helper thread".to_string()),
        );
    }
    Ok(promise.into())
}

/// Settles the promise `id` with what a helper thread produced.
pub fn settle(context: &mut JsContext, id: u64, outcome: Result<String, String>) {
    let Some(resolvers) = with_state(|state| state.pending.remove(&id)) else {
        return;
    };
    let undefined = JsValue::undefined();
    let result = match outcome {
        Ok(json) => {
            let value = if json.is_empty() {
                JsValue::undefined()
            } else {
                serde_json::from_str::<Value>(&json)
                    .ok()
                    .and_then(|value| JsValue::from_json(&value, context).ok())
                    .unwrap_or_default()
            };
            resolvers.resolve.call(&undefined, &[value], context)
        }
        Err(message) => {
            let error = JsNativeError::error()
                .with_message(message)
                .into_opaque(context);
            resolvers.reject.call(&undefined, &[error.into()], context)
        }
    };
    if let Err(error) = result {
        let text = describe(&error, context);
        log(Level::Error, &format!("settling a promise failed: {text}"));
    }
}

fn resolved(value: JsValue, context: &mut JsContext) -> JsResult<JsValue> {
    Ok(JsPromise::resolve(value, context)?.into())
}

fn rejected(message: impl Into<String>, context: &mut JsContext) -> JsResult<JsValue> {
    let error = JsNativeError::error().with_message(message.into());
    Ok(JsPromise::reject(error, context)?.into())
}

// ---- host.webview ---------------------------------------------------------------------

fn webview_client() -> Option<WebViewClient> {
    with_state(|state| state.spec.webview)
}

const NO_BRIDGE: &str = "parsec-orbit.webview is not available (is the webview bridge installed?)";

/// FNV-1a, for a script key that depends on the script.
fn fingerprint(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The key of a script: the caller's, or one derived from what it is.
fn script_key(options: &JsValue, default: &str, context: &mut JsContext) -> String {
    let key = options
        .as_object()
        .and_then(|options| options.get(js_string!("key"), context).ok())
        .and_then(|key| key.as_string().map(|key| key.to_std_string_escaped()))
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| default.to_string());
    let plugin = with_state(|state| state.spec.plugin.as_ref().map(|plugin| plugin.id.clone()));
    // Script keys are global in the bridge; the plugin id keeps them apart.
    format!("{}/{key}", plugin.unwrap_or_default())
}

/// `{ id, remove() }`
fn script_handle(id: i64, context: &mut JsContext) -> JsObject {
    let remove = NativeFunction::from_copy_closure_with_captures(
        |_, _, id: &i64, context| {
            let Some(webview) = webview_client() else {
                return rejected(NO_BRIDGE, context);
            };
            match webview.remove_script(*id) {
                Ok(()) => resolved(JsValue::undefined(), context),
                Err(error) => rejected(
                    format!("could not remove the script (code {})", error.code()),
                    context,
                ),
            }
        },
        id,
    );
    ObjectInitializer::new(context)
        .property(js_string!("id"), id as f64, Attribute::all())
        .function(remove, js_string!("remove"), 0)
        .build()
}

fn add_script(source: &str, key: &str, context: &mut JsContext) -> JsResult<JsValue> {
    let Some(webview) = webview_client() else {
        return rejected(NO_BRIDGE, context);
    };
    match webview.add_script(key, source) {
        Ok(id) => {
            let handle = script_handle(id, context);
            resolved(handle.into(), context)
        }
        Err(error) => rejected(
            format!("the bridge refused the script (code {})", error.code()),
            context,
        ),
    }
}

fn webview_inject(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let Some(source) = args.get_or_undefined(0).as_string() else {
        return rejected("inject needs the script source as a string", context);
    };
    let source = source.to_std_string_escaped();
    let key = script_key(
        args.get_or_undefined(1),
        &format!("inline-{:016x}", fingerprint(&source)),
        context,
    );
    add_script(&source, &key, context)
}

/// `path` below `dir`, or `None` when it points outside.
pub fn inside(dir: &Path, path: &str) -> Option<std::path::PathBuf> {
    let root = dir.canonicalize().ok()?;
    let target = root.join(path).canonicalize().ok()?;
    target.starts_with(&root).then_some(target)
}

fn webview_inject_file(
    _: &JsValue,
    args: &[JsValue],
    context: &mut JsContext,
) -> JsResult<JsValue> {
    let Some(path) = args.get_or_undefined(0).as_string() else {
        return rejected("injectFile needs a path as a string", context);
    };
    let path = path.to_std_string_escaped();
    let Some(dir) = with_state(|state| state.spec.plugin.as_ref().map(|plugin| plugin.dir.clone()))
    else {
        return rejected("this context has no plugin directory", context);
    };
    let Some(file) = inside(&dir, &path) else {
        return rejected(
            format!("{path}: no such file in the plugin directory"),
            context,
        );
    };
    let source = match std::fs::read_to_string(&file) {
        Ok(source) => source,
        Err(error) => return rejected(format!("{path}: {error}"), context),
    };
    let key = script_key(args.get_or_undefined(1), &path.replace('\\', "/"), context);
    add_script(&source, &key, context)
}

fn webview_evaluate(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let Some(expression) = args.get_or_undefined(0).as_string() else {
        return rejected("evaluate needs an expression as a string", context);
    };
    let expression = expression.to_std_string_escaped();
    let Some(webview) = webview_client() else {
        return rejected(NO_BRIDGE, context);
    };
    task(context, move || {
        let mut json = String::new();
        webview
            .evaluate(&expression, |result| json = result.to_string())
            .map(|()| json)
            .map_err(|error| match error.code() {
                code if code == parsec_orbit_plugin_sdk_std::ERR_NOT_READY => {
                    "the page is not ready".to_string()
                }
                code if code == parsec_orbit_plugin_sdk_std::ERR_TIMEOUT => {
                    "the page did not answer in time".to_string()
                }
                code if code == parsec_orbit_plugin_sdk_std::ERR_EXCEPTION => {
                    "the script threw in the page".to_string()
                }
                code => format!("evaluate failed (code {code})"),
            })
    })
}

fn webview_post(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let Some(channel) = args.get_or_undefined(0).as_string() else {
        return rejected("post needs a channel name as a string", context);
    };
    let channel = channel.to_std_string_escaped();
    let payload = match args.get_or_undefined(1).to_json(context) {
        Ok(Some(json)) => json.to_string(),
        Ok(None) => "null".to_string(),
        Err(error) => return rejected(describe(&error, context), context),
    };
    let Some(webview) = webview_client() else {
        return rejected(NO_BRIDGE, context);
    };
    task(context, move || {
        webview
            .post(&channel, &payload)
            .map(|()| String::new())
            .map_err(|error| match error.code() {
                code if code == parsec_orbit_plugin_sdk_std::ERR_NOT_READY => {
                    "the page is not ready".to_string()
                }
                code => format!("post failed (code {code})"),
            })
    })
}

/// What a page message needs to find its way to the context. Owned by the
/// channel it was made for, and freed once the bridge says it is done.
pub(crate) struct Subscriber {
    pub(crate) tx: Sender<Command>,
    pub(crate) channel: String,
}

/// Whether the bridge confirmed it dropped the subscription. "Not found" also
/// counts: a subscription the bridge does not know cannot call back. Any other
/// failure leaves it possibly live, so the caller leaks the subscriber instead
/// of freeing it under the bridge's feet.
fn unsubscribed(webview: &WebViewClient, id: i64) -> bool {
    match webview.unsubscribe(id) {
        Ok(()) => true,
        Err(error) => error.code() == parsec_orbit_plugin_sdk_std::ERR_NOT_FOUND,
    }
}

unsafe extern "system" fn on_page_message(user: *mut c_void, message: *const u8, len: usize) {
    // Safety: `user` is a leaked `Subscriber`, `message` is valid for the call.
    let subscriber = unsafe { &*(user as *const Subscriber) };
    let payload =
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(message, len) }).into_owned();
    let _ = subscriber.tx.send(Command::Message {
        channel: subscriber.channel.clone(),
        payload,
    });
}

fn webview_on(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let channel = args
        .get_or_undefined(0)
        .as_string()
        .ok_or_else(|| type_error("on needs a channel name as a string"))?
        .to_std_string_escaped();
    let handler = args
        .get_or_undefined(1)
        .as_object()
        .and_then(JsFunction::from_object)
        .ok_or_else(|| type_error("on needs a handler function"))?;
    let webview = webview_client().ok_or_else(|| type_error(NO_BRIDGE))?;

    let local = with_state(|state| {
        state.next_handler += 1;
        state.next_handler
    });
    let known = with_state(|state| state.channels.contains_key(&channel));
    if !known {
        // One subscription per channel; the handlers share it.
        let tx = with_state(|state| state.tx.clone());
        let user = Box::into_raw(Box::new(Subscriber {
            tx,
            channel: channel.clone(),
        })) as *mut c_void;
        // Safety: the callback only sends on a channel; the bridge holds the
        // pointer until `unsubscribe` returns, and the channel owns it.
        let bridge_id = match unsafe { webview.subscribe(&channel, on_page_message, user) } {
            Ok(bridge_id) => bridge_id,
            Err(error) => {
                // The bridge never took the pointer.
                drop(unsafe { Box::from_raw(user as *mut Subscriber) });
                return Err(type_error(&format!(
                    "subscribe failed (code {})",
                    error.code()
                )));
            }
        };
        with_state(|state| {
            state.channels.insert(
                channel.clone(),
                Channel {
                    bridge_id,
                    handlers: Vec::new(),
                    subscriber: user as *mut Subscriber,
                },
            )
        });
    }
    with_state(|state| {
        if let Some(entry) = state.channels.get_mut(&channel) {
            entry.handlers.push((local, handler));
        }
    });

    Ok(unsubscriber(&channel, local, context).into())
}

/// The function `on` returns: removes the handler, and the bridge
/// subscription with the last one.
fn unsubscriber(channel: &str, local: u64, context: &mut JsContext) -> JsObject {
    // The channel name travels as a JS string capture; GC-traced captures are
    // fine for `from_copy_closure_with_captures`.
    let function = NativeFunction::from_copy_closure_with_captures(
        |_, _, (channel, local): &(JsString, f64), _| {
            remove_handler(&channel.to_std_string_escaped(), *local as u64);
            Ok(JsValue::undefined())
        },
        (JsString::from(channel), local as f64),
    );
    function.to_js_function(context.realm()).into()
}

fn remove_handler(channel: &str, local: u64) {
    let (last, bridge_id, subscriber) = with_state(|state| {
        let Some(entry) = state.channels.get_mut(channel) else {
            return (false, 0, std::ptr::null_mut());
        };
        entry.handlers.retain(|(id, _)| *id != local);
        let last = entry.handlers.is_empty();
        let bridge_id = entry.bridge_id;
        let subscriber = entry.subscriber;
        if last {
            state.channels.remove(channel);
        }
        (last, bridge_id, subscriber)
    });
    if last {
        // Only free once the bridge confirmed it will not call back any more:
        // `unsubscribe` does not return while one of its callbacks is running.
        let confirmed = webview_client().is_some_and(|webview| unsubscribed(&webview, bridge_id));
        if confirmed {
            // Safety: the bridge no longer holds the subscription and the
            // channel is gone from the state, so nothing can reach this box.
            drop(unsafe { Box::from_raw(subscriber) });
        }
    }
}

/// Gives a page message to the handlers of its channel.
pub fn deliver(context: &mut JsContext, channel: &str, payload: &str) {
    let handlers: Vec<JsFunction> = with_state(|state| {
        state
            .channels
            .get(channel)
            .map(|entry| {
                entry
                    .handlers
                    .iter()
                    .map(|(_, handler)| handler.clone())
                    .collect()
            })
            .unwrap_or_default()
    });
    if handlers.is_empty() {
        return;
    }
    let value = serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| JsValue::from_json(&value, context).ok())
        .unwrap_or_default();
    for handler in handlers {
        if let Err(error) =
            handler.call(&JsValue::undefined(), std::slice::from_ref(&value), context)
        {
            let text = describe(&error, context);
            log(
                Level::Error,
                &format!("a handler for \"{channel}\" threw: {text}"),
            );
        }
    }
}

/// Drops the bridge subscriptions of a context that is going away.
pub fn unsubscribe_all() {
    let (webview, subscriptions) = with_state(|state| {
        let subscriptions: Vec<(i64, *mut Subscriber)> = state
            .channels
            .values()
            .map(|entry| (entry.bridge_id, entry.subscriber))
            .collect();
        state.channels.clear();
        (state.spec.webview, subscriptions)
    });
    for (id, subscriber) in subscriptions {
        let confirmed = webview
            .as_ref()
            .is_some_and(|webview| unsubscribed(webview, id));
        if confirmed {
            // Safety: as in `remove_handler`; the context is going away too,
            // so no callback can reach the inbox any more.
            drop(unsafe { Box::from_raw(subscriber) });
        }
    }
}

/// `host.webview.ready`: resolves once a page is attached. Asking is what
/// starts the bridge's session.
fn webview_ready(_: &JsValue, _: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    if let Some(promise) = with_state(|state| state.ready.clone()) {
        return Ok(promise.into());
    }
    let Some(webview) = webview_client() else {
        return rejected(NO_BRIDGE, context);
    };
    let (promise, id) = deferred(context);
    let (tx, stopped) = with_state(|state| (state.tx.clone(), state.stopped.clone()));
    let _ = std::thread::Builder::new()
        .name("js-ready".to_string())
        .spawn(move || {
            while !webview.is_ready() {
                if stopped.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = tx.send(Command::Settled {
                id,
                outcome: Ok(String::new()),
            });
        });
    with_state(|state| state.ready = Some(promise.clone()));
    Ok(promise.into())
}

fn webview_object(context: &mut JsContext) -> JsObject {
    let ready = function(webview_ready).to_js_function(context.realm());
    ObjectInitializer::new(context)
        .accessor(js_string!("ready"), Some(ready), None, Attribute::all())
        .function(function(webview_inject), js_string!("inject"), 2)
        .function(function(webview_inject_file), js_string!("injectFile"), 2)
        .function(function(webview_evaluate), js_string!("evaluate"), 1)
        .function(function(webview_on), js_string!("on"), 2)
        .function(function(webview_post), js_string!("post"), 2)
        .build()
}

// ---- host.config ---------------------------------------------------------------------

fn config_path() -> Option<std::path::PathBuf> {
    with_state(|state| {
        state
            .spec
            .plugin
            .as_ref()
            .and_then(|plugin| plugin.data_dir.as_ref())
            .map(|dir| dir.join("config.json"))
    })
}

fn read_config(path: &Path) -> Result<serde_json::Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(format!("{} does not hold an object", path.display())),
            Err(error) => Err(format!("{}: {error}", path.display())),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn config_get(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let Some(key) = args.get_or_undefined(0).as_string() else {
        return rejected("get needs a key as a string", context);
    };
    if let Some(client) = with_state(|state| state.spec.config) {
        let mut json = None;
        return match client.get(&key.to_std_string_escaped(), |value| {
            json = Some(value.to_string())
        }) {
            Ok(_) => {
                let value = json
                    .and_then(|json| serde_json::from_str::<Value>(&json).ok())
                    .and_then(|value| JsValue::from_json(&value, context).ok())
                    .unwrap_or_default();
                resolved(value, context)
            }
            Err(error) => rejected(
                format!("the config store failed (code {})", error.code()),
                context,
            ),
        };
    }
    let Some(path) = config_path() else {
        return rejected("this plugin has no data directory", context);
    };
    match read_config(&path) {
        Ok(map) => {
            let value = map
                .get(&key.to_std_string_escaped())
                .and_then(|value| JsValue::from_json(value, context).ok())
                .unwrap_or_default();
            resolved(value, context)
        }
        Err(message) => rejected(message, context),
    }
}

fn config_set(_: &JsValue, args: &[JsValue], context: &mut JsContext) -> JsResult<JsValue> {
    let Some(key) = args.get_or_undefined(0).as_string() else {
        return rejected("set needs a key as a string", context);
    };
    let value = match args.get_or_undefined(1).to_json(context) {
        Ok(value) => value,
        Err(error) => return rejected(describe(&error, context), context),
    };
    if let Some(client) = with_state(|state| state.spec.config) {
        let key = key.to_std_string_escaped();
        let outcome = match value {
            Some(value) => client
                .set(&key, &value.to_string())
                .map_err(|error| error.code()),
            None => client
                .remove(&key)
                .map(|_| ())
                .map_err(|error| error.code()),
        };
        return match outcome {
            Ok(()) => resolved(JsValue::undefined(), context),
            Err(code) => rejected(format!("the config store failed (code {code})"), context),
        };
    }
    let Some(path) = config_path() else {
        return rejected("this plugin has no data directory", context);
    };
    let outcome = (|| -> Result<(), String> {
        let mut map = read_config(&path)?;
        match value {
            Some(value) => map.insert(key.to_std_string_escaped(), value),
            None => map.remove(&key.to_std_string_escaped()),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, Value::Object(map).to_string())
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|error| format!("{}: {error}", path.display()))
    })();
    match outcome {
        Ok(()) => resolved(JsValue::undefined(), context),
        Err(message) => rejected(message, context),
    }
}

fn config_object(context: &mut JsContext) -> JsObject {
    ObjectInitializer::new(context)
        .function(function(config_get), js_string!("get"), 1)
        .function(function(config_set), js_string!("set"), 2)
        .build()
}

// ---- host ---------------------------------------------------------------------------

/// The `host` object a plugin's `setup` receives.
pub fn build_host(context: &mut JsContext, plugin: &PluginInfo) -> JsResult<JsObject> {
    let info = ObjectInitializer::new(context)
        .property(
            js_string!("id"),
            JsString::from(plugin.id.as_str()),
            Attribute::all(),
        )
        .property(
            js_string!("version"),
            JsString::from(plugin.version.as_str()),
            Attribute::all(),
        )
        .property(
            js_string!("dir"),
            JsString::from(plugin.dir.to_string_lossy().as_ref()),
            Attribute::all(),
        )
        .property(
            js_string!("dataDir"),
            JsString::from(
                plugin
                    .data_dir
                    .as_ref()
                    .map(|dir| dir.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .as_str(),
            ),
            Attribute::all(),
        )
        .build();
    let logger = log_object(context);

    let mut host = ObjectInitializer::new(context);
    host.property(js_string!("plugin"), info, Attribute::all());
    host.property(js_string!("log"), logger, Attribute::all());
    for (name, native) in [
        ("setTimeout", set_timeout as Native),
        ("setInterval", set_interval as Native),
        ("clearTimeout", clear_timer as Native),
        ("clearInterval", clear_timer as Native),
    ] {
        host.function(function(native), JsString::from(name), 2);
    }
    let host = host.build();

    // Capabilities are default-deny: the object is simply absent without the
    // permission in the manifest.
    if plugin.permits(Permission::Webview.as_str()) {
        let webview = webview_object(context);
        host.set(js_string!("webview"), webview, true, context)?;
    }
    if plugin.permits(Permission::Config.as_str()) {
        let config = config_object(context);
        host.set(js_string!("config"), config, true, context)?;
    }
    Ok(host)
}
