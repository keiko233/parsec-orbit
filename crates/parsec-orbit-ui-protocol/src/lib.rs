//! Private, versioned pipe protocol. No plugin pointers cross this boundary.

pub mod schema;
pub use schema::{UiAction, UiItem, UiOption, UiSection};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

/// Both bundled artifacts must speak this protocol version.
pub const VERSION: u32 = 1;
/// Maximum encoded frame, excluding the terminating newline.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// A section and its authoritative values in the plugin process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionState {
    pub schema: UiSection,
    pub values: HashMap<String, Value>,
}

/// Commands from the service plugin to its owned child process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ParentMessage {
    Snapshot {
        sections: Vec<SectionState>,
        visible: bool,
        revision: u64,
    },
    Visibility {
        visible: bool,
        revision: u64,
    },
    Shutdown,
}

/// Events from the UI process; callbacks remain in the plugin process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ChildMessage {
    Ready { version: u32 },
    Action(UiAction),
    Visibility { visible: bool, revision: u64 },
}

/// Reads one bounded JSON line. EOF before a frame returns `None`.
pub fn read_message<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
) -> io::Result<Option<T>> {
    let mut bytes = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated UI frame",
                ))
            };
        }
        let count = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buffer.len(), |i| i + 1);
        if bytes.len() + count > MAX_FRAME_BYTES + 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "UI frame exceeds limit",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
}

/// Writes and flushes one bounded JSON line without interleaving frames.
pub fn write_message(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(message).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UI frame exceeds limit",
        ));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn escaped_newlines_unicode_and_multiple_frames_round_trip() {
        let action = ChildMessage::Action(UiAction {
            section: "中文\nsection".into(),
            key: "x".into(),
            action_type: "change".into(),
            value: Some(Value::String("line1\nline2".into())),
        });
        let mut stream = Vec::new();
        write_message(&mut stream, &action).unwrap();
        write_message(&mut stream, &ChildMessage::Ready { version: VERSION }).unwrap();
        let mut reader = BufReader::with_capacity(7, Cursor::new(stream));
        assert_eq!(
            read_message::<ChildMessage>(&mut reader).unwrap(),
            Some(action)
        );
        assert_eq!(
            read_message::<ChildMessage>(&mut reader).unwrap(),
            Some(ChildMessage::Ready { version: VERSION })
        );
        assert_eq!(read_message::<ChildMessage>(&mut reader).unwrap(), None);
    }

    #[test]
    fn oversized_invalid_and_truncated_input_is_rejected() {
        let bytes = vec![b'x'; MAX_FRAME_BYTES + 2];
        assert_eq!(
            read_message::<ChildMessage>(&mut Cursor::new(bytes))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_message::<ChildMessage>(&mut Cursor::new(b"{\n"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_message::<ChildMessage>(&mut Cursor::new(b"{"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn oversized_output_is_rejected_before_any_write() {
        let action = ChildMessage::Action(UiAction {
            section: "s".into(),
            key: "x".into(),
            action_type: "change".into(),
            value: Some(Value::String("x".repeat(MAX_FRAME_BYTES))),
        });
        let mut output = Vec::new();
        assert!(write_message(&mut output, &action).is_err());
        assert!(output.is_empty());
    }
}
