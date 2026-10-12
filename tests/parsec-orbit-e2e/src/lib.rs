//! Helpers shared by the end-to-end tests and the `loader-host` binary.

use std::path::Path;

/// Names in the export table of a 64-bit PE file, in table order.
///
/// Reads the file instead of mapping it, so the answer does not depend on what
/// is loaded in this process.
pub fn export_names(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let u16_at = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    let u32_at =
        |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;

    assert_eq!(&bytes[..2], b"MZ", "{} is not a PE file", path.display());
    let nt = u32_at(0x3C);
    assert_eq!(&bytes[nt..nt + 4], b"PE\0\0");
    assert_eq!(u16_at(nt + 24), 0x20B, "expected a PE32+ image");
    let sections = nt + 24 + u16_at(nt + 20);
    let count = u16_at(nt + 6);

    let to_offset = |rva: usize| {
        (0..count)
            .find_map(|index| {
                let header = sections + index * 40;
                let size = u32_at(header + 8).max(u32_at(header + 16));
                let (va, raw) = (u32_at(header + 12), u32_at(header + 20));
                (rva >= va && rva < va + size).then(|| rva - va + raw)
            })
            .unwrap_or_else(|| panic!("RVA 0x{rva:X} is in no section"))
    };

    let export_rva = u32_at(nt + 24 + 112);
    if export_rva == 0 {
        return Vec::new();
    }
    let directory = to_offset(export_rva);
    let names = to_offset(u32_at(directory + 32));
    (0..u32_at(directory + 24))
        .map(|index| {
            let start = to_offset(u32_at(names + index * 4));
            let end = start + bytes[start..].iter().position(|b| *b == 0).unwrap();
            String::from_utf8(bytes[start..end].to_vec()).unwrap()
        })
        .collect()
}
