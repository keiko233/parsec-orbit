//! Real native event-loop regression. Requires an interactive desktop session.
//! Run explicitly: cargo test -p parsec-orbit-ui-companion --test pipe_lifecycle -- --ignored
#![cfg(any(windows, target_os = "macos"))]

use parsec_orbit_ui_protocol::{read_message, write_message, ChildMessage, ParentMessage, VERSION};
use std::io::BufReader;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn launch() -> OwnedChild {
    OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_parsec-orbit-ui-companion"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}
fn ready(child: &mut OwnedChild) {
    let stdout = child.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let _ = send.send(read_message::<ChildMessage>(&mut reader));
        // Keep draining until the child exits; never close its stdout early.
        while read_message::<ChildMessage>(&mut reader).is_ok_and(|value| value.is_some()) {}
    });
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        Some(ChildMessage::Ready { version: VERSION })
    );
}
fn exited(child: &mut OwnedChild) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "idle renderer did not process pipe command"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "requires an interactive desktop session"]
fn idle_run_loop_handles_shutdown_and_parent_eof() {
    for shutdown in [true, false] {
        let mut child = launch();
        ready(&mut child);
        std::thread::sleep(Duration::from_millis(200));
        if shutdown {
            write_message(child.0.stdin.as_mut().unwrap(), &ParentMessage::Shutdown).unwrap();
        } else {
            drop(child.0.stdin.take());
        }
        exited(&mut child);
    }
    // Parent exit before GUI construction must not strand an orphan window.
    let mut child = launch();
    drop(child.0.stdin.take());
    exited(&mut child);
}
