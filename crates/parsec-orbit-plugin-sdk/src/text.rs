//! Formatting without an allocator.

use core::ffi::CStr;
use core::fmt;

/// A fixed buffer that implements [`fmt::Write`]. Text that does not fit is
/// cut at a character boundary and ends in `...`; the buffer always holds
/// valid UTF-8 followed by a NUL byte, so it can also be handed out as a C
/// string.
pub struct StackBuf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    truncated: bool,
}

const ELLIPSIS: &str = "...";

impl<const N: usize> StackBuf<N> {
    /// Usable bytes: one is kept for the terminating NUL.
    pub const CAPACITY: usize = N - 1;

    pub const fn new() -> Self {
        StackBuf {
            bytes: [0; N],
            len: 0,
            truncated: false,
        }
    }

    pub fn as_str(&self) -> &str {
        // Only whole `&str`s are ever copied in, cut at character boundaries.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    /// Whether text was dropped.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The content as a C string, unless it contains a NUL.
    pub fn as_c_str(&self) -> Option<&CStr> {
        CStr::from_bytes_with_nul(&self.bytes[..=self.len]).ok()
    }

    fn mark_truncated(&mut self) {
        self.truncated = true;
        let mut len = self.len;
        if len + ELLIPSIS.len() > Self::CAPACITY {
            len = Self::CAPACITY.saturating_sub(ELLIPSIS.len());
            while len > 0 && !self.as_str().is_char_boundary(len) {
                len -= 1;
            }
        }
        // Everything past `len` is cleared so the NUL terminator stays intact.
        for byte in &mut self.bytes[len..] {
            *byte = 0;
        }
        let end = (len + ELLIPSIS.len()).min(Self::CAPACITY);
        self.bytes[len..end].copy_from_slice(&ELLIPSIS.as_bytes()[..end - len]);
        self.len = end;
    }
}

impl<const N: usize> Default for StackBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for StackBuf<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.truncated {
            return Ok(());
        }
        let room = Self::CAPACITY - self.len;
        if text.len() <= room {
            self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
            self.len += text.len();
            return Ok(());
        }
        let mut take = room;
        while take > 0 && !text.is_char_boundary(take) {
            take -= 1;
        }
        self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
        self.len += take;
        self.mark_truncated();
        Ok(())
    }
}
