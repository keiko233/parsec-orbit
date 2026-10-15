//! Headless pipe peer for process-supervision integration tests. Never installed.
use parsec_orbit_ui_protocol::{
    read_message, write_message, ChildMessage, ParentMessage, UiAction, VERSION,
};
use std::io::{self, BufReader};

fn main() -> io::Result<()> {
    let mut input = BufReader::new(io::stdin());
    let mut output = io::stdout();
    write_message(&mut output, &ChildMessage::Ready { version: VERSION })?;
    while let Some(message) = read_message::<ParentMessage>(&mut input)? {
        if let ParentMessage::Snapshot {
            pages, revision, ..
        } = message
        {
            for page in pages {
                if page.page.id == "fixture" {
                    let stale_close =
                        page.values.get("stale_close").and_then(|v| v.as_bool()) == Some(true);
                    if let Some(marker) = page.values.get("crash_once").and_then(|v| v.as_str()) {
                        if !std::path::Path::new(marker).exists() {
                            std::fs::write(marker, b"crashed")?;
                            std::process::exit(23);
                        }
                    }
                    write_message(
                        &mut output,
                        &ChildMessage::Action(UiAction {
                            page: page.page.id,
                            key: "observed".into(),
                            action_type: "change".into(),
                            value: page.values.get("observed").cloned(),
                        }),
                    )?;
                    if stale_close
                        || page.values.get("report_hidden").and_then(|v| v.as_bool()) == Some(true)
                    {
                        write_message(
                            &mut output,
                            &ChildMessage::Visibility {
                                visible: false,
                                revision: if stale_close {
                                    revision.saturating_sub(1)
                                } else {
                                    revision
                                },
                            },
                        )?;
                    }
                }
            }
        } else if let ParentMessage::Navigate { page, .. } = message {
            // Echo the selection as an ordinary edit so a test can observe it.
            write_message(
                &mut output,
                &ChildMessage::Action(UiAction {
                    page: "fixture".into(),
                    key: "route".into(),
                    action_type: "change".into(),
                    value: Some(page.into()),
                }),
            )?;
        } else if matches!(message, ParentMessage::Shutdown) {
            break;
        }
    }
    Ok(())
}
