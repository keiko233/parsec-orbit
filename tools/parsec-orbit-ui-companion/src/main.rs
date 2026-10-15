mod maintenance;
mod platform;
mod window;

use std::io::{self, BufReader, BufWriter};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use parsec_orbit_ui_protocol::{
    read_message, write_message, ChildMessage, ParentMessage, SectionState,
};
use winio::prelude::*;

use platform::WakeTarget;
use window::{OrbitUi, OrbitUiMessage};

pub(crate) enum Inbound {
    Frame(ParentMessage),
    Eof,
}

enum WriterMessage {
    Frame(ChildMessage),
    Stop,
}

struct UiConnection {
    sender: Option<ComponentSender<OrbitUi>>,
    wake: Option<WakeTarget>,
    pending: PendingInput,
    input_queued: bool,
}

#[derive(Default)]
struct PendingInput {
    sequence: u64,
    snapshot: Option<(u64, Vec<SectionState>, bool, u64)>,
    visibility: Option<(u64, bool, u64)>,
    exit: bool,
}

impl PendingInput {
    fn push(&mut self, message: Inbound) {
        match message {
            Inbound::Frame(ParentMessage::Snapshot {
                sections,
                visible,
                revision,
            }) => {
                self.sequence = self.sequence.wrapping_add(1);
                self.snapshot = Some((self.sequence, sections, visible, revision));
            }
            Inbound::Frame(ParentMessage::Visibility { visible, revision }) => {
                self.sequence = self.sequence.wrapping_add(1);
                self.visibility = Some((self.sequence, visible, revision));
            }
            Inbound::Frame(ParentMessage::Shutdown) | Inbound::Eof => self.exit = true,
        }
    }

    fn into_messages(self) -> Vec<Inbound> {
        if self.exit {
            return vec![Inbound::Eof];
        }
        let mut messages = Vec::with_capacity(2);
        let snapshot_order = self.snapshot.as_ref().map(|snapshot| snapshot.0);
        if let Some((_, sections, visible, revision)) = self.snapshot {
            messages.push(Inbound::Frame(ParentMessage::Snapshot {
                sections,
                visible,
                revision,
            }));
        }
        if let Some((order, visible, revision)) = self.visibility {
            if snapshot_order.is_none_or(|snapshot| order > snapshot) {
                messages.push(Inbound::Frame(ParentMessage::Visibility {
                    visible,
                    revision,
                }));
            }
        }
        messages
    }
}

pub(crate) struct Shared {
    connection: Mutex<UiConnection>,
    writer: SyncSender<WriterMessage>,
}

impl Shared {
    fn new(writer: SyncSender<WriterMessage>) -> Self {
        Self {
            connection: Mutex::new(UiConnection {
                sender: None,
                wake: None,
                pending: PendingInput::default(),
                input_queued: false,
            }),
            writer,
        }
    }

    fn connect(&self, sender: &ComponentSender<OrbitUi>, wake: WakeTarget) {
        let mut connection = self.connection.lock().unwrap();
        connection.wake = Some(wake);
        connection.sender = Some(sender.clone());
        connection.input_queued = true;
        sender.post(OrbitUiMessage::InputAvailable);
        if let Some(wake) = &connection.wake {
            wake.wake();
        }
    }

    fn disconnect(&self) {
        let mut connection = self.connection.lock().unwrap();
        connection.sender = None;
        connection.wake = None;
    }

    fn post(&self, message: Inbound) {
        let mut connection = self.connection.lock().unwrap();
        connection.pending.push(message);
        if !connection.input_queued {
            if let Some(sender) = &connection.sender {
                sender.post(OrbitUiMessage::InputAvailable);
                connection.input_queued = true;
            }
        }
        if let Some(wake) = &connection.wake {
            wake.wake();
        }
    }

    fn take_input(&self) -> Vec<Inbound> {
        let mut connection = self.connection.lock().unwrap();
        connection.input_queued = false;
        std::mem::take(&mut connection.pending).into_messages()
    }

    fn write(&self, message: ChildMessage) {
        match self.writer.try_send(WriterMessage::Frame(message)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                eprintln!("parsec-orbit-ui-companion: stdout queue is full; exiting");
                self.post(Inbound::Eof);
            }
            Err(TrySendError::Disconnected(_)) => {
                eprintln!("parsec-orbit-ui-companion: stdout writer has stopped; exiting");
                self.post(Inbound::Eof);
            }
        }
    }

    fn stop_writer(&self) {
        let _ = self.writer.try_send(WriterMessage::Stop);
    }
}

fn main() {
    if let Some(result) = maintenance::run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        if let Err(error) = result {
            eprintln!("Restart and apply failed: {error}");
            #[cfg(windows)]
            {
                let text: Vec<u16> = format!("Restart and apply failed:\n{error}")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                let title: Vec<u16> = "Parsec Orbit".encode_utf16().chain(Some(0)).collect();
                // Safety: terminated UTF-16 buffers remain valid for the modal dialog.
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                        std::ptr::null_mut(),
                        text.as_ptr(),
                        title.as_ptr(),
                        0x10,
                    );
                }
            }
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run() {
        eprintln!("parsec-orbit-ui-companion: {error}");
        std::process::exit(1);
    }
}

fn run() -> std::result::Result<(), String> {
    let (writer, output) = mpsc::sync_channel(64);
    let shared = Arc::new(Shared::new(writer));
    let writer_context = Arc::downgrade(&shared);
    thread::Builder::new()
        .name("orbit-ui-stdout".to_string())
        .spawn(move || write_output(output, writer_context))
        .map_err(|error| format!("could not start stdout writer: {error}"))?;

    let reader_context = Arc::clone(&shared);
    thread::Builder::new()
        .name("orbit-ui-stdin".to_string())
        .spawn(move || read_input(reader_context))
        .map_err(|error| format!("could not start stdin reader: {error}"))?;

    let app = App::builder()
        .name("Parsec Orbit")
        .build()
        .map_err(|error| format!("could not initialize winio: {error:?}"))?;
    #[cfg(target_os = "macos")]
    platform::finish_launching();
    let result = app
        .block_on(OrbitUi::run_until_event(Arc::clone(&shared)))
        .map_err(|error| format!("winio event loop failed: {error:?}"));

    shared.disconnect();
    shared.stop_writer();
    result
}

fn read_input(shared: Arc<Shared>) {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    loop {
        match read_message::<ParentMessage>(&mut reader) {
            Ok(Some(message @ ParentMessage::Shutdown)) => {
                shared.post(Inbound::Frame(message));
                break;
            }
            Ok(Some(message)) => shared.post(Inbound::Frame(message)),
            Ok(None) => {
                shared.post(Inbound::Eof);
                break;
            }
            Err(error) => {
                eprintln!("parsec-orbit-ui-companion: invalid stdin frame: {error}");
                shared.post(Inbound::Eof);
                break;
            }
        }
    }
}

fn write_output(messages: mpsc::Receiver<WriterMessage>, shared: Weak<Shared>) {
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    while let Ok(message) = messages.recv() {
        match message {
            WriterMessage::Frame(message) => {
                if let Err(error) = write_message(&mut writer, &message) {
                    eprintln!("parsec-orbit-ui-companion: could not write stdout frame: {error}");
                    if let Some(shared) = shared.upgrade() {
                        shared.post(Inbound::Eof);
                    }
                    break;
                }
            }
            WriterMessage::Stop => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_bursts_coalesce_without_losing_later_show() {
        let mut pending = PendingInput::default();
        for revision in 0..100 {
            pending.push(Inbound::Frame(ParentMessage::Snapshot {
                sections: vec![],
                visible: false,
                revision,
            }));
        }
        pending.push(Inbound::Frame(ParentMessage::Visibility {
            visible: true,
            revision: 100,
        }));
        let messages = pending.into_messages();
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages[0],
            Inbound::Frame(ParentMessage::Snapshot { revision: 99, .. })
        ));
        assert!(matches!(
            messages[1],
            Inbound::Frame(ParentMessage::Visibility {
                visible: true,
                revision: 100
            })
        ));
    }

    #[test]
    fn eof_takes_priority_over_pending_render_work() {
        let mut pending = PendingInput::default();
        pending.push(Inbound::Frame(ParentMessage::Snapshot {
            sections: vec![],
            visible: true,
            revision: 1,
        }));
        pending.push(Inbound::Eof);
        let messages = pending.into_messages();
        assert!(matches!(messages.as_slice(), [Inbound::Eof]));
    }
}
