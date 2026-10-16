//! Owns the UI child, its private pipes and a separate action delivery thread.
//!
//! The service never waits for GUI work or a pipe write. A stalled child is
//! killed by its owned handle; bounded retries replay the authoritative registry.

use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parsec_orbit_ui_protocol::{read_message, write_message, ChildMessage, ParentMessage, VERSION};

use crate::registry::{PendingAction, Registry};

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
const MAX_ATTEMPTS: u32 = 3;

/// Diagnostic sink, owned by the plugin context or a disposable host.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

enum Event {
    Refresh,
    Activate,
    Received(u32, ChildMessage),
    Written(u32),
    Failed(u32, String),
}

/// The process lifetime of the UI renderer. Dropping it stops its owned child.
pub struct Companion {
    events: mpsc::SyncSender<Event>,
    visible: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    pid: Arc<AtomicU32>,
    pending: Arc<AtomicBool>,
    navigate: Arc<AtomicBool>,
    retry: Arc<AtomicBool>,
    revision: Arc<Mutex<u64>>,
    worker: Option<JoinHandle<()>>,
}

impl Companion {
    /// Starts supervision without blocking on child initialization.
    pub fn start(
        executable: PathBuf,
        registry: Arc<Registry>,
        visible: Arc<AtomicBool>,
        log: Log,
    ) -> io::Result<Self> {
        let (events, receiver) = mpsc::sync_channel(64);
        let stopped = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicU32::new(0));
        let pending = Arc::new(AtomicBool::new(true));
        let navigate = Arc::new(AtomicBool::new(false));
        let retry = Arc::new(AtomicBool::new(false));
        let revision = Arc::new(Mutex::new(0));
        let (actions, delivery) = mpsc::sync_channel::<PendingAction>(64);
        thread::Builder::new()
            .name("orbit-ui-actions".into())
            .spawn(move || {
                while let Ok(action) = delivery.recv() {
                    action.deliver();
                }
            })?;
        let worker = {
            let events = events.clone();
            let visible = visible.clone();
            let stopped = stopped.clone();
            let pid = pid.clone();
            let pending = pending.clone();
            let navigate = navigate.clone();
            let retry = retry.clone();
            let revision = revision.clone();
            thread::Builder::new()
                .name("orbit-ui-process".into())
                .spawn(move || {
                    supervise(
                        executable, registry, visible, stopped, pid, pending, navigate, retry,
                        revision, events, receiver, actions, log,
                    );
                })?
        };
        Ok(Self {
            events,
            visible,
            stopped,
            pid,
            pending,
            navigate,
            retry,
            revision,
            worker: Some(worker),
        })
    }

    /// Requests a registry snapshot; a full queue already guarantees a wakeup.
    pub fn refresh(&self) {
        self.pending.store(true, Ordering::SeqCst);
        let _ = self.events.try_send(Event::Refresh);
    }

    /// Asks the renderer to show the registry's selected page.
    pub fn navigate(&self) {
        *self.revision.lock().unwrap() += 1;
        self.navigate.store(true, Ordering::SeqCst);
        let _ = self.events.try_send(Event::Refresh);
    }

    /// Sets desired visibility and permits another bounded launch attempt.
    pub fn set_visible(&self, visible: bool) {
        {
            let mut revision = self.revision.lock().unwrap();
            *revision += 1;
            self.visible.store(visible, Ordering::SeqCst);
        }
        self.pending.store(true, Ordering::SeqCst);
        if visible {
            self.allow_foreground();
            self.retry.store(true, Ordering::SeqCst);
        }
        let _ = self.events.try_send(if visible {
            Event::Activate
        } else {
            Event::Refresh
        });
    }

    /// Toggles the requested visibility without touching GUI APIs.
    pub fn toggle(&self) {
        let visible = {
            let mut revision = self.revision.lock().unwrap();
            *revision += 1;
            !self.visible.fetch_xor(true, Ordering::SeqCst)
        };
        self.pending.store(true, Ordering::SeqCst);
        if visible {
            self.allow_foreground();
            self.retry.store(true, Ordering::SeqCst);
        }
        let _ = self.events.try_send(if visible {
            Event::Activate
        } else {
            Event::Refresh
        });
    }

    fn allow_foreground(&self) {
        #[cfg(windows)]
        {
            let pid = self.child_pid();
            if pid != 0 {
                // Safety: only grant the process identity of our currently owned
                // renderer. Windows may reject a caller without focus rights.
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
                }
            }
        }
    }

    /// Returns the owned child's PID, or zero while disconnected.
    pub fn child_pid(&self) -> u32 {
        self.pid.load(Ordering::SeqCst)
    }
}

impl Drop for Companion {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.refresh();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Session {
    child: Child,
    writer: mpsc::SyncSender<ParentMessage>,
    started: Instant,
    writing: Option<Instant>,
    ready: bool,
    generation: u32,
}

impl Session {
    fn launch(
        executable: &PathBuf,
        generation: u32,
        events: &mpsc::SyncSender<Event>,
    ) -> io::Result<Self> {
        let mut command = Command::new(executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Never inject the host loader into the renderer through inherited DYLD
        // configuration. The loader uses a bundle-path activation gate as well.
        command
            .env_remove("DYLD_INSERT_LIBRARIES")
            .env_remove("DYLD_LIBRARY_PATH");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW: pipes only.
        }
        let mut child = command.spawn()?;
        let mut input = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let events_read = events.clone();
        let reader = thread::Builder::new()
            .name("orbit-ui-read".into())
            .spawn(move || {
                let mut reader = BufReader::new(output);
                loop {
                    match read_message::<ChildMessage>(&mut reader) {
                        Ok(Some(message)) => {
                            if events_read
                                .send(Event::Received(generation, message))
                                .is_err()
                            {
                                break;
                            }
                        }
                        result => {
                            let message = match result {
                                Ok(None) => "UI pipe closed".into(),
                                Err(e) => e.to_string(),
                                _ => unreachable!(),
                            };
                            let _ = events_read.send(Event::Failed(generation, message));
                            break;
                        }
                    }
                }
            });
        if let Err(error) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let (writer, messages) = mpsc::sync_channel(1);
        let events_write = events.clone();
        let writer_thread = thread::Builder::new()
            .name("orbit-ui-write".into())
            .spawn(move || {
                while let Ok(message) = messages.recv() {
                    match write_message(&mut input, &message) {
                        Ok(()) => {
                            if events_write.send(Event::Written(generation)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = events_write.send(Event::Failed(generation, error.to_string()));
                            break;
                        }
                    }
                }
            });
        if let Err(error) = writer_thread {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            writer,
            started: Instant::now(),
            writing: None,
            ready: false,
            generation,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The handle names our child even if its PID would later be reused.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[allow(clippy::too_many_arguments)] // One actor owns its explicit dependencies.
fn supervise(
    executable: PathBuf,
    registry: Arc<Registry>,
    visible: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    pid: Arc<AtomicU32>,
    pending: Arc<AtomicBool>,
    navigate: Arc<AtomicBool>,
    retry: Arc<AtomicBool>,
    revision: Arc<Mutex<u64>>,
    events: mpsc::SyncSender<Event>,
    receiver: mpsc::Receiver<Event>,
    actions: mpsc::SyncSender<PendingAction>,
    log: Log,
) {
    let mut session: Option<Session> = None;
    let mut attempts = 0;
    let mut generation = 0;
    let mut next_launch = Instant::now();
    let mut dirty = true;
    let mut focus = false;
    let mut navigating = false;
    while !stopped.load(Ordering::SeqCst) {
        dirty |= pending.swap(false, Ordering::SeqCst);
        navigating |= navigate.swap(false, Ordering::SeqCst);
        if retry.swap(false, Ordering::SeqCst) {
            focus = true;
            attempts = 0;
            next_launch = Instant::now();
        }
        if session.is_none() && attempts < MAX_ATTEMPTS && Instant::now() >= next_launch {
            attempts += 1;
            generation += 1;
            match Session::launch(&executable, generation, &events) {
                Ok(new_session) => {
                    pid.store(new_session.child.id(), Ordering::SeqCst);
                    session = Some(new_session);
                    dirty = true;
                }
                Err(error) => {
                    log(&format!("UI companion launch failed: {error}"));
                    next_launch = Instant::now() + Duration::from_millis(250 * u64::from(attempts));
                }
            }
        }
        let mut failed = None;
        match receiver.recv_timeout(POLL) {
            Ok(Event::Refresh) => dirty = true,
            Ok(Event::Activate) => {
                focus = true;
                attempts = 0;
                next_launch = Instant::now();
                dirty = true;
            }
            Ok(Event::Received(id, message))
                if session.as_ref().is_some_and(|s| s.generation == id) =>
            {
                let current = session.as_mut().unwrap();
                match message {
                    ChildMessage::Ready { version } if version == VERSION && !current.ready => {
                        current.ready = true;
                        dirty = true;
                        // A new renderer starts on its first page; replay the selection.
                        navigating = true;
                    }
                    ChildMessage::Ready { .. } => {
                        failed = Some("UI protocol handshake mismatch".into())
                    }
                    _ if !current.ready => failed = Some("UI event before handshake".into()),
                    ChildMessage::Visibility {
                        visible: value,
                        revision: received_revision,
                    } => {
                        let current_revision = revision.lock().unwrap();
                        if received_revision == *current_revision
                            && visible.swap(value, Ordering::SeqCst) != value
                        {
                            dirty = true;
                        }
                    }
                    ChildMessage::Action(action) => {
                        if registry.accepts_action(&action) {
                            if let Some(action) = registry.prepare_action(&action) {
                                if actions.try_send(action).is_err() {
                                    log("UI action queue full; action dropped");
                                }
                            }
                        } else {
                            log("UI companion sent an invalid action");
                        }
                    }
                }
            }
            Ok(Event::Written(id)) if session.as_ref().is_some_and(|s| s.generation == id) => {
                session.as_mut().unwrap().writing = None
            }
            Ok(Event::Failed(id, error))
                if session.as_ref().is_some_and(|s| s.generation == id) =>
            {
                failed = Some(error)
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Some(current) = session.as_mut() {
            match current.child.try_wait() {
                Ok(Some(status)) => failed = Some(format!("UI companion exited: {status}")),
                Err(error) => failed = Some(error.to_string()),
                Ok(None) => {}
            }
            if (!current.ready && current.started.elapsed() > IO_TIMEOUT)
                || current
                    .writing
                    .is_some_and(|time| time.elapsed() > IO_TIMEOUT)
            {
                failed = Some("UI companion stopped responding".into());
            }
            if failed.is_none() && current.ready && current.writing.is_none() && !dirty && focus {
                let request_revision = revision.lock().unwrap();
                if current
                    .writer
                    .try_send(ParentMessage::Visibility {
                        visible: visible.load(Ordering::SeqCst),
                        revision: *request_revision,
                    })
                    .is_ok()
                {
                    current.writing = Some(Instant::now());
                    focus = false;
                }
            }
            if failed.is_none()
                && current.ready
                && current.writing.is_none()
                && !dirty
                && !focus
                && navigating
            {
                navigating = false;
                if let Some(page) = registry.selected() {
                    let request_revision = revision.lock().unwrap();
                    if current
                        .writer
                        .try_send(ParentMessage::Navigate {
                            page,
                            revision: *request_revision,
                        })
                        .is_ok()
                    {
                        current.writing = Some(Instant::now());
                    }
                }
            }
            if failed.is_none() && current.ready && current.writing.is_none() && dirty {
                let pages = registry.list_pages();
                let request_revision = revision.lock().unwrap();
                let message = ParentMessage::Snapshot {
                    pages,
                    visible: visible.load(Ordering::SeqCst),
                    revision: *request_revision,
                };
                drop(request_revision);
                // Serializing first rejects an oversized snapshot without a
                // partially written frame or an unbounded pipe write.
                let mut encoded = Vec::new();
                if let Err(error) = write_message(&mut encoded, &message) {
                    log(&format!("UI snapshot rejected: {error}"));
                    dirty = false;
                } else if current.writer.try_send(message).is_ok() {
                    current.writing = Some(Instant::now());
                    dirty = false;
                }
            }
        }
        if let Some(error) = failed {
            log(&error);
            session = None;
            pid.store(0, Ordering::SeqCst);
            next_launch = Instant::now() + Duration::from_millis(250 * u64::from(attempts.max(1)));
        }
    }
    drop(session);
    pid.store(0, Ordering::SeqCst);
}
