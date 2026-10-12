//! Helpers shared by the unit tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh directory under the system temp directory, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "parsec-orbit-test-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the test directory");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `contents` to `relative`, creating parent directories.
    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the parent directory");
        }
        std::fs::write(&path, contents).expect("write the test file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize
}

fn u32_at(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

/// Names in the export table of a 64-bit PE file, in table order.
///
/// Reads the file instead of loading it, so the answer is the same whether or
/// not the module is mapped in this process.
pub fn pe_export_names(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).expect("read the PE file");
    assert_eq!(&bytes[..2], b"MZ");
    let nt = u32_at(&bytes, 0x3C);
    assert_eq!(&bytes[nt..nt + 4], b"PE\0\0");
    let section_count = u16_at(&bytes, nt + 6);
    let optional_size = u16_at(&bytes, nt + 20);
    let optional = nt + 24;
    assert_eq!(u16_at(&bytes, optional), 0x20B, "expected a PE32+ image");
    let export_rva = u32_at(&bytes, optional + 112);

    let sections = optional + optional_size;
    let to_offset = |rva: usize| -> usize {
        for index in 0..section_count {
            let header = sections + index * 40;
            let virtual_size = u32_at(&bytes, header + 8);
            let virtual_address = u32_at(&bytes, header + 12);
            let raw_size = u32_at(&bytes, header + 16);
            let raw_pointer = u32_at(&bytes, header + 20);
            if rva >= virtual_address && rva < virtual_address + virtual_size.max(raw_size) {
                return rva - virtual_address + raw_pointer;
            }
        }
        panic!("RVA 0x{rva:X} is in no section");
    };

    if export_rva == 0 {
        return Vec::new();
    }
    let directory = to_offset(export_rva);
    let name_count = u32_at(&bytes, directory + 24);
    let names = to_offset(u32_at(&bytes, directory + 32));
    (0..name_count)
        .map(|index| {
            let start = to_offset(u32_at(&bytes, names + index * 4));
            let end = bytes[start..].iter().position(|b| *b == 0).unwrap() + start;
            String::from_utf8(bytes[start..end].to_vec()).unwrap()
        })
        .collect()
}

/// `%SystemRoot%\System32\<name>`.
pub fn system32(name: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}
