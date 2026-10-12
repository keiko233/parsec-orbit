//! The bridge: scripts, subscriptions and the DevTools session that serves
//! them.
//!
//! One thread owns the WebSocket (the "session actor"). Everything else talks
//! to it through [`Bridge`]: shared state behind a mutex (the scripts to keep
//! installed, the subscriptions) plus a command channel for what has to happen
//! on the socket (evaluate, post, "scripts changed"). When the socket drops -
//! the page was recreated, the client restarted its UI - the thread attaches
//! again and replays the scripts, so a plugin registers a script once.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use parsec_orbit_plugin_sdk_std::{
    Level, MessageCallback, ERR_EXCEPTION, ERR_INVALID, ERR_NOT_FOUND, ERR_NOT_READY, ERR_TIMEOUT,
};
use serde_json::Value;

use parsec_orbit_devtools::{attach, json_string, truncate, Message, Ws};

/// The page-side helper (`window.__orbit`).
pub const ORBIT_SCRIPT: &str = include_str!("../assets/orbit.js");

/// Name of the CDP binding the page calls to send a message to a plugin.
pub const BINDING: &str = "__orbitSend";

/// Where the session reports what it does.
pub type Logger = Box<dyn Fn(Level, &str) + Send + Sync>;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const EVALUATE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);
const RETRY: Duration = Duration::from_millis(1000);

struct Script {
    id: i64,
    key: String,
    source: String,
    /// Identifier of the registration in the current session.
    cdp: Option<String>,
    /// Has to be (re)registered in the current session.
    dirty: bool,
}

struct Subscription {
    id: i64,
    channel: String,
    callback: MessageCallback,
    user: usize,
    /// Set by `unsubscribe`; a delivery that sees it does nothing.
    cancelled: Arc<AtomicBool>,
}

/// One page message on its way to one subscriber.
struct Delivery {
    callback: MessageCallback,
    user: usize,
    payload: String,
    cancelled: Arc<AtomicBool>,
}

/// What the delivery thread and `unsubscribe` share: the queue callbacks run
/// from, and how many of them are running right now.
struct Dispatcher {
    /// Set once the delivery thread runs; without it nothing is delivered.
    /// (`mpsc::Sender` is not `Sync`, so it lives behind the mutex.)
    queue: Mutex<Option<Sender<Delivery>>>,
    in_flight: AtomicUsize,
    idle: Mutex<()>,
    idle_signal: Condvar,
}

impl Dispatcher {
    fn new() -> Dispatcher {
        Dispatcher {
            queue: Mutex::new(None),
            in_flight: AtomicUsize::new(0),
            idle: Mutex::new(()),
            idle_signal: Condvar::new(),
        }
    }
}

thread_local! {
    /// Whether this thread is inside a subscriber callback. `unsubscribe`
    /// called from one must not wait for the callback it is running in.
    static DISPATCHING: Cell<bool> = const { Cell::new(false) };
}

/// Runs the subscriber callbacks of a bridge, in the order the messages
/// arrived, away from the session thread.
fn delivery_loop(dispatcher: Arc<Dispatcher>, inbox: Receiver<Delivery>) {
    while let Ok(delivery) = inbox.recv() {
        if delivery.cancelled.load(Ordering::SeqCst) {
            continue;
        }
        // Counted before the second look at `cancelled`, so `unsubscribe`
        // that sees no callback running can rely on this one not starting.
        dispatcher.in_flight.fetch_add(1, Ordering::SeqCst);
        if !delivery.cancelled.load(Ordering::SeqCst) {
            DISPATCHING.with(|flag| flag.set(true));
            // Safety: the subscriber promised the callback and its user
            // pointer stay valid until it unsubscribes; `unsubscribe` does not
            // return while a callback of that subscription is running.
            unsafe {
                (delivery.callback)(
                    delivery.user as *mut c_void,
                    delivery.payload.as_ptr(),
                    delivery.payload.len(),
                )
            };
            DISPATCHING.with(|flag| flag.set(false));
        }
        if dispatcher.in_flight.fetch_sub(1, Ordering::SeqCst) == 1 {
            let _idle = dispatcher
                .idle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            dispatcher.idle_signal.notify_all();
        }
    }
}

#[derive(Default)]
struct State {
    next_id: i64,
    scripts: Vec<Script>,
    /// Registrations of replaced or removed scripts to undo.
    removed: Vec<String>,
    subscriptions: Vec<Subscription>,
}

enum Command {
    /// The scripts changed: bring the page in line.
    Sync,
    Evaluate {
        expression: String,
        reply: Sender<Result<String, i32>>,
    },
    Post {
        channel: String,
        payload: String,
        /// Answered once the message is on the socket.
        reply: Sender<i32>,
    },
}

/// How to start the session, kept until somebody needs it.
struct Launch {
    port: u16,
    name: String,
    log: Logger,
}

/// The shared half of the bridge.
pub struct Bridge {
    evaluate_timeout: Duration,
    ready: AtomicBool,
    state: Mutex<State>,
    commands: Mutex<Option<Sender<Command>>>,
    launch: Mutex<Option<Launch>>,
    dispatcher: OnceLock<Arc<Dispatcher>>,
}

impl Default for Bridge {
    fn default() -> Self {
        Self::new()
    }
}

impl Bridge {
    pub const fn new() -> Bridge {
        Bridge {
            evaluate_timeout: EVALUATE_TIMEOUT,
            ready: AtomicBool::new(false),
            state: Mutex::new(State {
                next_id: 0,
                scripts: Vec::new(),
                removed: Vec::new(),
                subscriptions: Vec::new(),
            }),
            commands: Mutex::new(None),
            launch: Mutex::new(None),
            dispatcher: OnceLock::new(),
        }
    }

    /// A bridge whose `evaluate` gives up after `timeout` (tests).
    pub const fn with_evaluate_timeout(mut self, timeout: Duration) -> Bridge {
        self.evaluate_timeout = timeout;
        self
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A callback that panicked must not take the bridge down with it.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send(&self, command: Command) -> bool {
        let guard = self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .as_ref()
            .is_some_and(|commands| commands.send(command).is_ok())
    }

    /// The thread that runs subscriber callbacks, started on first use. It is
    /// a thread of its own so a callback may call back into the bridge
    /// (`evaluate`, `post`) without stalling the session thread.
    fn dispatcher(&self) -> Arc<Dispatcher> {
        Arc::clone(self.dispatcher.get_or_init(|| {
            let dispatcher = Arc::new(Dispatcher::new());
            let (queue, inbox) = mpsc::channel();
            let worker = Arc::clone(&dispatcher);
            let running = std::thread::Builder::new()
                .name("orbit-bridge:deliveries".to_string())
                .spawn(move || delivery_loop(worker, inbox))
                .is_ok();
            if running {
                *dispatcher
                    .queue
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(queue);
            }
            dispatcher
        }))
    }

    /// Waits until no subscriber callback is running. Called from a callback
    /// it returns at once: the subscription is already gone, so no further
    /// callback starts, and the callback that is running is the caller.
    fn wait_for_deliveries(&self) {
        if DISPATCHING.with(Cell::get) {
            return;
        }
        let Some(dispatcher) = self.dispatcher.get() else {
            return;
        };
        let mut idle = dispatcher
            .idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while dispatcher.in_flight.load(Ordering::SeqCst) != 0 {
            idle = dispatcher
                .idle_signal
                .wait(idle)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Remembers how to reach the endpoint. The session only starts when
    /// somebody asks for something ([`Bridge::start_if_configured`]), so a
    /// bridge nobody uses never connects to the page.
    pub fn configure(&self, port: u16, name: &str, log: Logger) {
        *self
            .launch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Launch {
            port,
            name: name.to_string(),
            log,
        });
    }

    /// Starts the session thread if [`Bridge::configure`] was called and it
    /// has not been started yet.
    pub fn start_if_configured(&'static self) {
        let launch = self
            .launch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(Launch { port, name, log }) = launch {
            if let Err(error) = self.start(port, &name, log) {
                // Nothing to log through any more; the calls say "not ready".
                let _ = error;
            }
        }
    }

    /// Starts the session thread for the DevTools endpoint on `port`.
    pub fn start(&'static self, port: u16, name: &str, log: Logger) -> std::io::Result<()> {
        let (commands, inbox) = mpsc::channel();
        *self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(commands);
        std::thread::Builder::new()
            .name(format!("{name}:session"))
            .spawn(move || self.run(port, inbox, &*log))?;
        Ok(())
    }

    /// Whether a page is attached.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    // ---- scripts ---------------------------------------------------------------

    /// Keeps `source` installed in every document under `key`. Returns the
    /// script id, which stays the same when the key is registered again.
    pub fn add_script(&self, key: &str, source: &str) -> i64 {
        let id = {
            let mut state = self.state();
            let existing = state.scripts.iter().position(|script| script.key == key);
            let id = match existing {
                Some(index) => {
                    let old = state.scripts[index].cdp.take();
                    state.removed.extend(old);
                    let script = &mut state.scripts[index];
                    script.source = source.to_string();
                    script.dirty = true;
                    script.id
                }
                None => {
                    state.next_id += 1;
                    let id = state.next_id;
                    state.scripts.push(Script {
                        id,
                        key: key.to_string(),
                        source: source.to_string(),
                        cdp: None,
                        dirty: true,
                    });
                    id
                }
            };
            id
        };
        self.send(Command::Sync);
        id
    }

    pub fn remove_script(&self, id: i64) -> i32 {
        let found = {
            let mut state = self.state();
            match state.scripts.iter().position(|script| script.id == id) {
                Some(index) => {
                    let script = state.scripts.remove(index);
                    state.removed.extend(script.cdp);
                    true
                }
                None => false,
            }
        };
        if !found {
            return ERR_NOT_FOUND;
        }
        self.send(Command::Sync);
        0
    }

    // ---- evaluation and messages ------------------------------------------------

    /// Evaluates `expression` in the page; returns the JSON result.
    pub fn evaluate(&self, expression: &str) -> Result<String, i32> {
        if !self.is_ready() {
            return Err(ERR_NOT_READY);
        }
        let (reply, answer) = mpsc::channel();
        let sent = self.send(Command::Evaluate {
            expression: expression.to_string(),
            reply,
        });
        if !sent {
            return Err(ERR_NOT_READY);
        }
        match answer.recv_timeout(self.evaluate_timeout + Duration::from_secs(1)) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(ERR_TIMEOUT),
            Err(RecvTimeoutError::Disconnected) => Err(ERR_NOT_READY),
        }
    }

    /// Sends `payload` (JSON) to the page handlers of `channel`.
    pub fn post(&self, channel: &str, payload: &str) -> i32 {
        if channel.is_empty() || serde_json::from_str::<Value>(payload).is_err() {
            return ERR_INVALID;
        }
        if !self.is_ready() {
            return ERR_NOT_READY;
        }
        let (reply, sent) = mpsc::channel();
        let queued = self.send(Command::Post {
            channel: channel.to_string(),
            payload: payload.to_string(),
            reply,
        });
        if !queued {
            return ERR_NOT_READY;
        }
        // The answer comes once the message is on the socket, so a caller
        // that returns and exits has not left it in a queue.
        sent.recv_timeout(COMMAND_TIMEOUT).unwrap_or(ERR_TIMEOUT)
    }

    pub fn subscribe(&self, channel: &str, callback: MessageCallback, user: *mut c_void) -> i64 {
        if channel.is_empty() {
            return ERR_INVALID as i64;
        }
        let mut state = self.state();
        state.next_id += 1;
        let id = state.next_id;
        state.subscriptions.push(Subscription {
            id,
            channel: channel.to_string(),
            callback,
            user: user as usize,
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        id
    }

    /// Removes a subscription. It returns only once the callbacks that
    /// already started have returned, so the subscriber can free its `user`
    /// pointer as soon as this returns. From inside a callback it does not
    /// wait for that callback; after it returns no further callback starts.
    pub fn unsubscribe(&self, id: i64) -> i32 {
        let cancelled = {
            let mut state = self.state();
            match state
                .subscriptions
                .iter()
                .position(|subscription| subscription.id == id)
            {
                Some(index) => {
                    let subscription = state.subscriptions.remove(index);
                    // Set under the lock: a delivery that reads it after this
                    // point must not call the callback.
                    subscription.cancelled.store(true, Ordering::SeqCst);
                    Some(subscription.cancelled)
                }
                None => None,
            }
        };
        if cancelled.is_none() {
            return ERR_NOT_FOUND;
        }
        self.wait_for_deliveries();
        0
    }

    /// Hands a page message to the subscribers of its channel. The callbacks
    /// run on the delivery thread, never on the session thread, so a callback
    /// may call back in without stalling the session it is answering for.
    fn dispatch(&self, channel: &str, payload: &str) -> usize {
        let targets: Vec<(MessageCallback, usize, Arc<AtomicBool>)> = self
            .state()
            .subscriptions
            .iter()
            .filter(|subscription| subscription.channel == channel)
            .map(|subscription| {
                (
                    subscription.callback,
                    subscription.user,
                    Arc::clone(&subscription.cancelled),
                )
            })
            .collect();
        let count = targets.len();
        if count == 0 {
            return 0;
        }
        let dispatcher = self.dispatcher();
        let queue = dispatcher
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(queue) = queue else {
            // The delivery thread never started; nothing can run the callbacks.
            return 0;
        };
        for (callback, user, cancelled) in targets {
            let _ = queue.send(Delivery {
                callback,
                user,
                payload: payload.to_string(),
                cancelled,
            });
        }
        count
    }

    // ---- the delivery thread -----------------------------------------------------

    // ---- the session thread ------------------------------------------------------

    fn run(&self, port: u16, inbox: Receiver<Command>, log: &dyn Fn(Level, &str)) {
        let mut waiting_logged = false;
        let mut ids = 0u64;
        loop {
            match attach(port) {
                Ok((socket, target)) => {
                    waiting_logged = false;
                    let result = self.session(socket, &target, &inbox, &mut ids, log);
                    self.ready.store(false, Ordering::Release);
                    self.forget_registrations();
                    match result {
                        Ok(()) => return,
                        Err(reason) => {
                            log(Level::Info, &format!("devtools session ended: {reason}"))
                        }
                    }
                }
                Err(reason) => {
                    if !waiting_logged {
                        log(
                            Level::Info,
                            &format!("devtools endpoint not available yet: {reason}"),
                        );
                        waiting_logged = true;
                    }
                }
            }
            // Nobody can be served until the next attach; answer requests
            // instead of leaving them to time out.
            let until = Instant::now() + RETRY;
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match inbox.recv_timeout(left) {
                    Ok(Command::Evaluate { reply, .. }) => {
                        let _ = reply.send(Err(ERR_NOT_READY));
                    }
                    Ok(Command::Post { reply, .. }) => {
                        let _ = reply.send(ERR_NOT_READY);
                    }
                    Ok(Command::Sync) => {}
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }

    /// A new session starts with no registrations.
    fn forget_registrations(&self) {
        let mut state = self.state();
        state.removed.clear();
        for script in &mut state.scripts {
            script.cdp = None;
            script.dirty = true;
        }
    }

    /// Runs one attached session until the socket goes away. `Ok` means the
    /// bridge was asked to stop.
    fn session(
        &self,
        mut ws: Ws,
        target: &str,
        inbox: &Receiver<Command>,
        ids: &mut u64,
        log: &dyn Fn(Level, &str),
    ) -> Result<(), String> {
        let mut next = || {
            *ids += 1;
            *ids
        };
        ws.set_poll_interval(POLL);

        ws.call(next(), "Runtime.enable", "{}", COMMAND_TIMEOUT)?;
        ws.call(
            next(),
            "Runtime.addBinding",
            &format!("{{\"name\":{}}}", json_string(BINDING)),
            COMMAND_TIMEOUT,
        )?;
        self.install(&mut ws, ORBIT_SCRIPT, &mut next)?.1.ok();
        self.sync(&mut ws, &mut next, log)?;

        self.ready.store(true, Ordering::Release);
        log(
            Level::Info,
            &format!("devtools session attached ({target})"),
        );

        let mut pending: HashMap<u64, (Instant, Option<Sender<Result<String, i32>>>)> =
            HashMap::new();
        let outcome = loop {
            let mut work = || -> Result<(), String> {
                while let Ok(command) = inbox.try_recv() {
                    match command {
                        Command::Sync => self.sync(&mut ws, &mut next, log)?,
                        Command::Evaluate { expression, reply } => {
                            let id = next();
                            ws.command(id, "Runtime.evaluate", &evaluate_params(&expression))?;
                            pending
                                .insert(id, (Instant::now() + self.evaluate_timeout, Some(reply)));
                        }
                        Command::Post {
                            channel,
                            payload,
                            reply,
                        } => {
                            let expression = format!(
                                "window.__orbit && window.__orbit.__deliver({}, {})",
                                json_string(&channel),
                                payload
                            );
                            let sent = ws.command(
                                next(),
                                "Runtime.evaluate",
                                &evaluate_params(&expression),
                            );
                            let _ = reply.send(if sent.is_ok() { 0 } else { ERR_NOT_READY });
                            sent?;
                        }
                    }
                }
                for text in ws.take_backlog() {
                    self.incoming(&text, &mut pending, log);
                }
                match ws.read_message(Instant::now() + POLL)? {
                    Some(Message::Text(text)) => self.incoming(&text, &mut pending, log),
                    Some(Message::Close) => return Err("websocket closed".to_string()),
                    Some(Message::Ignored) | None => {}
                }
                let now = Instant::now();
                let expired: Vec<u64> = pending
                    .iter()
                    .filter(|(_, (deadline, _))| *deadline < now)
                    .map(|(id, _)| *id)
                    .collect();
                for id in expired {
                    if let Some((_, Some(reply))) = pending.remove(&id) {
                        let _ = reply.send(Err(ERR_TIMEOUT));
                    }
                }
                Ok(())
            };
            if let Err(reason) = work() {
                break Err(reason);
            }
        };
        for (_, (_, reply)) in pending {
            if let Some(reply) = reply {
                let _ = reply.send(Err(ERR_NOT_READY));
            }
        }
        outcome
    }

    /// Registers `source` for new documents and runs it in the current one.
    /// Returns the registration identifier and how the run went: the JSON
    /// value, or the description of what it threw.
    #[allow(clippy::type_complexity)]
    fn install(
        &self,
        ws: &mut Ws,
        source: &str,
        next: &mut impl FnMut() -> u64,
    ) -> Result<(String, Result<String, String>), String> {
        let reply = ws.call(
            next(),
            "Page.addScriptToEvaluateOnNewDocument",
            &format!("{{\"source\":{}}}", json_string(source)),
            COMMAND_TIMEOUT,
        )?;
        let identifier = serde_json::from_str::<Value>(&reply)
            .ok()
            .and_then(|value| value["result"]["identifier"].as_str().map(str::to_string))
            .unwrap_or_default();
        let reply = ws.call(
            next(),
            "Runtime.evaluate",
            &evaluate_params(source),
            COMMAND_TIMEOUT,
        )?;
        let outcome = match serde_json::from_str::<Value>(&reply) {
            Ok(value) => match exception_description(&value) {
                Some(description) => Err(truncate(&description, 300)),
                None => Ok(value["result"]["result"]
                    .get("value")
                    .unwrap_or(&Value::Null)
                    .to_string()),
            },
            Err(_) => Ok("null".to_string()),
        };
        Ok((identifier, outcome))
    }

    /// Brings the page in line with the registered scripts.
    fn sync(
        &self,
        ws: &mut Ws,
        next: &mut impl FnMut() -> u64,
        log: &dyn Fn(Level, &str),
    ) -> Result<(), String> {
        let (removed, dirty): (Vec<String>, Vec<(i64, String, String)>) = {
            let mut state = self.state();
            let removed = std::mem::take(&mut state.removed);
            let dirty = state
                .scripts
                .iter()
                .filter(|script| script.dirty)
                .map(|script| (script.id, script.key.clone(), script.source.clone()))
                .collect();
            (removed, dirty)
        };
        for identifier in removed {
            ws.call(
                next(),
                "Page.removeScriptToEvaluateOnNewDocument",
                &format!("{{\"identifier\":{}}}", json_string(&identifier)),
                COMMAND_TIMEOUT,
            )?;
        }
        for (id, key, source) in dirty {
            let (identifier, outcome) = self.install(ws, &source, next)?;
            {
                let mut state = self.state();
                if let Some(script) = state.scripts.iter_mut().find(|script| script.id == id) {
                    // Changed again while we were busy: it stays dirty and
                    // the next sync registers the new source.
                    if script.source == source {
                        script.cdp = Some(identifier);
                        script.dirty = false;
                    } else {
                        state.removed.push(identifier);
                    }
                }
            }
            match outcome {
                Ok(value) => log(Level::Info, &format!("script {key} installed ({value})")),
                // Registered for new documents; running it now failed. That
                // is the script's problem, not the session's.
                Err(thrown) => log(
                    Level::Warn,
                    &format!("script {key} failed to run now: {thrown}"),
                ),
            }
        }
        Ok(())
    }

    /// Handles one incoming message: a reply to a pending evaluate or an
    /// event.
    fn incoming(
        &self,
        text: &str,
        pending: &mut HashMap<u64, (Instant, Option<Sender<Result<String, i32>>>)>,
        log: &dyn Fn(Level, &str),
    ) {
        let Ok(message) = serde_json::from_str::<Value>(text) else {
            return;
        };
        if let Some(id) = message["id"].as_u64() {
            if let Some((_, Some(reply))) = pending.remove(&id) {
                let _ = reply.send(evaluation_result(&message, log));
            }
            return;
        }
        if message["method"] == "Runtime.bindingCalled" && message["params"]["name"] == BINDING {
            let payload = message["params"]["payload"].as_str().unwrap_or_default();
            let Ok(envelope) = serde_json::from_str::<Value>(payload) else {
                return;
            };
            let (Some(channel), payload) = (
                envelope["channel"].as_str(),
                envelope.get("payload").unwrap_or(&Value::Null),
            ) else {
                return;
            };
            let delivered = self.dispatch(channel, &payload.to_string());
            if delivered == 0 {
                log(
                    Level::Debug,
                    &format!("page message on \"{channel}\" has no subscriber"),
                );
            }
        }
    }
}

fn evaluate_params(expression: &str) -> String {
    format!(
        "{{\"expression\":{},\"returnByValue\":true,\"awaitPromise\":true}}",
        json_string(expression)
    )
}

/// What a `Runtime.evaluate` reply says the script threw, if it did.
fn exception_description(reply: &Value) -> Option<String> {
    let details = reply["result"].get("exceptionDetails")?;
    Some(
        details["exception"]["description"]
            .as_str()
            .or_else(|| details["text"].as_str())
            .unwrap_or("exception")
            .to_string(),
    )
}

/// The JSON value a `Runtime.evaluate` reply carries, or the error code.
fn evaluation_result(reply: &Value, log: &dyn Fn(Level, &str)) -> Result<String, i32> {
    if let Some(description) = exception_description(reply) {
        log(
            Level::Warn,
            &format!("page script threw: {}", truncate(&description, 300)),
        );
        return Err(ERR_EXCEPTION);
    }
    if reply.get("error").is_some() {
        return Err(ERR_EXCEPTION);
    }
    let value = reply["result"]["result"]
        .get("value")
        .unwrap_or(&Value::Null);
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "system" fn ignore(_user: *mut c_void, _message: *const u8, _len: usize) {}

    #[test]
    fn a_script_key_is_registered_once_and_keeps_its_id() {
        let bridge = Bridge::new();
        let first = bridge.add_script("a", "1");
        let second = bridge.add_script("b", "2");
        assert!(first > 0 && second > first);
        assert_eq!(bridge.add_script("a", "changed"), first);
        let state = bridge.state();
        assert_eq!(state.scripts.len(), 2);
        assert_eq!(state.scripts[0].source, "changed");
        assert!(state.scripts[0].dirty);
    }

    #[test]
    fn replacing_or_removing_a_script_queues_its_registration_for_removal() {
        let bridge = Bridge::new();
        let id = bridge.add_script("a", "1");
        bridge.state().scripts[0].cdp = Some("cdp-1".into());
        bridge.add_script("a", "2");
        assert_eq!(bridge.state().removed, ["cdp-1"]);
        assert!(bridge.state().scripts[0].cdp.is_none());

        bridge.state().scripts[0].cdp = Some("cdp-2".into());
        assert_eq!(bridge.remove_script(id), 0);
        assert_eq!(bridge.state().removed, ["cdp-1", "cdp-2"]);
        assert_eq!(bridge.remove_script(id), ERR_NOT_FOUND);
        assert!(bridge.state().scripts.is_empty());
    }

    #[test]
    fn nothing_works_before_a_page_is_attached() {
        let bridge = Bridge::new();
        assert!(!bridge.is_ready());
        assert_eq!(bridge.evaluate("1"), Err(ERR_NOT_READY));
        assert_eq!(bridge.post("channel", "{}"), ERR_NOT_READY);
    }

    #[test]
    fn post_validates_its_arguments_first() {
        let bridge = Bridge::new();
        assert_eq!(bridge.post("", "{}"), ERR_INVALID);
        assert_eq!(bridge.post("channel", "not json"), ERR_INVALID);
        assert_eq!(bridge.post("channel", ""), ERR_INVALID);
    }

    #[test]
    fn subscriptions_are_found_by_channel_and_removable() {
        use std::sync::atomic::AtomicUsize;
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn count(user: *mut c_void, message: *const u8, len: usize) {
            let text = unsafe { std::slice::from_raw_parts(message, len) };
            assert_eq!(text, b"{\"x\":1}");
            assert_eq!(user as usize, 77);
            SEEN.fetch_add(1, Ordering::SeqCst);
        }
        let bridge = Bridge::new();
        let subscription = bridge.subscribe("news", count, 77 as *mut c_void);
        let other = bridge.subscribe("other", ignore, std::ptr::null_mut());
        assert!(subscription > 0 && other > subscription);
        assert_eq!(
            bridge.subscribe("", ignore, std::ptr::null_mut()),
            ERR_INVALID as i64
        );

        assert_eq!(bridge.dispatch("news", "{\"x\":1}"), 1);
        assert_eq!(bridge.dispatch("nobody", "{}"), 0);
        // The callbacks run on the delivery thread.
        let deadline = Instant::now() + Duration::from_secs(5);
        while SEEN.load(Ordering::SeqCst) != 1 {
            assert!(Instant::now() < deadline, "the callback never ran");
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(bridge.unsubscribe(subscription), 0);
        assert_eq!(bridge.unsubscribe(subscription), ERR_NOT_FOUND);
        assert_eq!(bridge.dispatch("news", "{\"x\":1}"), 0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(SEEN.load(Ordering::SeqCst), 1, "one delivery, then stopped");
    }

    #[test]
    fn unsubscribe_waits_for_a_callback_that_is_already_running() {
        use std::sync::atomic::AtomicBool;
        static STARTED: AtomicBool = AtomicBool::new(false);
        static FINISHED: AtomicBool = AtomicBool::new(false);
        unsafe extern "system" fn slow(_: *mut c_void, _: *const u8, _: usize) {
            STARTED.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            FINISHED.store(true, Ordering::SeqCst);
        }
        let bridge = Bridge::new();
        let subscription = bridge.subscribe("slow", slow, std::ptr::null_mut());
        assert_eq!(bridge.dispatch("slow", "{}"), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !STARTED.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the callback never started");
            std::thread::sleep(Duration::from_millis(5));
        }

        // A subscriber frees what `user` points to once this returns, so it
        // has to wait for the callback that already started.
        assert_eq!(bridge.unsubscribe(subscription), 0);
        assert!(
            FINISHED.load(Ordering::SeqCst),
            "the callback was left behind"
        );
    }

    #[test]
    fn a_callback_may_unsubscribe_itself() {
        use std::sync::atomic::AtomicUsize;
        static BRIDGE: Bridge = Bridge::new();
        static ID: AtomicUsize = AtomicUsize::new(0);
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn once(_: *mut c_void, _: *const u8, _: usize) {
            let id = ID.load(Ordering::SeqCst) as i64;
            assert_eq!(
                BRIDGE.unsubscribe(id),
                0,
                "a callback unsubscribing itself must not wait for itself"
            );
            SEEN.fetch_add(1, Ordering::SeqCst);
        }
        let id = BRIDGE.subscribe("self", once, std::ptr::null_mut());
        ID.store(id as usize, Ordering::SeqCst);
        assert_eq!(BRIDGE.dispatch("self", "{}"), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while SEEN.load(Ordering::SeqCst) != 1 {
            assert!(Instant::now() < deadline, "the callback never ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(BRIDGE.unsubscribe(id), ERR_NOT_FOUND, "it is gone already");
    }

    #[test]
    fn a_new_session_starts_with_every_script_dirty() {
        let bridge = Bridge::new();
        bridge.add_script("a", "1");
        {
            let mut state = bridge.state();
            state.scripts[0].cdp = Some("old".into());
            state.scripts[0].dirty = false;
            state.removed.push("stale".into());
        }
        bridge.forget_registrations();
        let state = bridge.state();
        assert!(state.scripts[0].dirty && state.scripts[0].cdp.is_none());
        assert!(state.removed.is_empty());
    }

    #[test]
    fn evaluation_replies_become_json_or_error_codes() {
        // No host table here: the helper that logs is exercised in the
        // integration tests; these replies do not need it.
        let value = serde_json::json!({"id":1,"result":{"result":{"type":"number","value":42}}});
        assert_eq!(evaluate_result_without_log(&value), Ok("42".to_string()));
        let undefined = serde_json::json!({"id":1,"result":{"result":{"type":"undefined"}}});
        assert_eq!(
            evaluate_result_without_log(&undefined),
            Ok("null".to_string())
        );
        let object = serde_json::json!({"id":1,"result":{"result":{"value":{"a":[1,"x"]}}}});
        assert_eq!(
            evaluate_result_without_log(&object),
            Ok("{\"a\":[1,\"x\"]}".to_string())
        );
        let error = serde_json::json!({"id":1,"error":{"code":-32000,"message":"no"}});
        assert_eq!(evaluate_result_without_log(&error), Err(ERR_EXCEPTION));
    }

    /// `evaluation_result` minus the logging, for tests without a host.
    fn evaluate_result_without_log(reply: &Value) -> Result<String, i32> {
        if reply["result"].get("exceptionDetails").is_some() || reply.get("error").is_some() {
            return Err(ERR_EXCEPTION);
        }
        let value = reply["result"]["result"]
            .get("value")
            .unwrap_or(&Value::Null);
        Ok(value.to_string())
    }

    #[test]
    fn the_orbit_helper_defines_the_documented_api() {
        for needle in [
            "emit",
            "on",
            "off",
            "rpc",
            "__deliver",
            "__orbitSend",
            "MTY_NativeSendText",
        ] {
            assert!(ORBIT_SCRIPT.contains(needle), "{needle}");
        }
        assert!(ORBIT_SCRIPT.contains(&format!("window.{BINDING}")));
    }
}
