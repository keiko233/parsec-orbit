//! Native (libmatoya) UI patch: extend the bandwidth limit dropdown.
//!
//! The native UI builds its dropdowns from static option tables: an array of
//! records, each `0x144` bytes, holding a NUL-terminated label at offset `0`
//! and the numeric value as an `int32` at offset `0x40`. The bandwidth table
//! has twelve entries (`3 Mbps` .. `50 Mbps`), and the two code sites that use
//! it load the table base with a RIP-relative `lea` and carry the entry count
//! as a separate immediate.
//!
//! The patch keeps the original table untouched and instead:
//!
//! 1. allocates a copy extended with the configured Mbps options,
//! 2. rewrites the two `lea` displacements to point at the copy (the
//!    displacement is relative, so the copy has to live within 2 GB of the
//!    code - the allocator asks for addresses near the module), and
//! 3. patches the two count immediates from `12` to the new length.
//!
//! Every patched location is read back afterwards, and the report goes to the
//! loader log. All edits are in memory only.

use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
};
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_READWRITE,
};

/// Size of one option record.
const RECORD_STRIDE: usize = 0x144;
/// Offset of the numeric value inside a record.
const VALUE_OFFSET: usize = 0x40;
/// Number of records in the built-in bandwidth table.
const BUILT_IN_COUNT: usize = 12;
/// Search window after a table reference for its count immediate.
const COUNT_SEARCH_WINDOW: usize = 0x60;
/// RIP-relative displacements reach 2 GB; keep a little margin.
const MAX_DISPLACEMENT: i64 = 0x7FFF_0000;

const BUILT_IN_OPTIONS: [(&str, i32); BUILT_IN_COUNT] = [
    ("3 Mbps", 3),
    ("5 Mbps", 5),
    ("7 Mbps", 7),
    ("10 Mbps", 10),
    ("15 Mbps", 15),
    ("20 Mbps", 20),
    ("25 Mbps", 25),
    ("30 Mbps", 30),
    ("35 Mbps", 35),
    ("40 Mbps", 40),
    ("45 Mbps", 45),
    ("50 Mbps", 50),
];

pub struct Report {
    pub module: String,
    pub table_offset: usize,
    pub reference_sites: usize,
    pub patched_references: usize,
    pub patched_counts: usize,
    pub options: usize,
}

struct ModuleInfo {
    name: String,
    base: usize,
}

struct Section {
    name: String,
    va: usize,
    size: usize,
}

struct Image {
    sections: Vec<Section>,
}

impl Image {
    /// Parses the PE headers of the module mapped at `base`.
    unsafe fn parse(base: usize) -> Option<Image> {
        if read_u16(base) != 0x5A4D {
            return None;
        }
        let nt = base + read_u32(base + 0x3C) as usize;
        if read_u32(nt) != 0x0000_4550 {
            return None;
        }
        let number_of_sections = read_u16(nt + 6) as usize;
        let optional_size = read_u16(nt + 20) as usize;
        let optional = nt + 24;
        if read_u16(optional) != 0x20B {
            return None;
        }

        let mut sections = Vec::with_capacity(number_of_sections);
        for index in 0..number_of_sections {
            let header = optional + optional_size + index * 40;
            let name = read_name(header);
            let size = read_u32(header + 8) as usize;
            let va = read_u32(header + 12) as usize;
            sections.push(Section { name, va, size });
        }
        Some(Image { sections })
    }

    fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.name == name)
    }
}

/// Extends the native bandwidth dropdown. Returns `None` when the table could
/// not be found (unknown client build) or nothing was patched.
pub fn extend_bandwidth<F: Fn(&str)>(log: F) -> Option<Report> {
    extend_bandwidth_with(crate::default_extras, log)
}

/// Same as [`extend_bandwidth`], but asks `extras` for the options to append
/// once the client module is there so it uses the persisted options.
pub fn extend_bandwidth_with<G: Fn() -> Vec<i32>, F: Fn(&str)>(
    extras: G,
    log: F,
) -> Option<Report> {
    extend_bandwidth_with_when_ready(&extras, &log, Duration::from_secs(10))
}

/// Same as [`extend_bandwidth`], but waits for the client module to be loaded
/// first: the native UI lives in `parsecd-<version>.dll`, which the executable
/// maps after start-up.
pub fn extend_bandwidth_when_ready<F: Fn(&str)>(log: &F, timeout: Duration) -> Option<Report> {
    extend_bandwidth_with_when_ready(&crate::default_extras, log, timeout)
}

/// [`extend_bandwidth_when_ready`] with a caller-chosen extension of the ladder.
pub fn extend_bandwidth_with_when_ready<G: Fn() -> Vec<i32>, F: Fn(&str)>(
    extras: &G,
    log: &F,
    timeout: Duration,
) -> Option<Report> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(module) = find_client_module() {
            return try_extend(&module, &extras(), log);
        }
        if Instant::now() >= deadline {
            log("legacy ui patch: the client module never loaded, skipped");
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn try_extend<F: Fn(&str)>(module: &ModuleInfo, extras: &[i32], log: &F) -> Option<Report> {
    log(&format!(
        "legacy ui patch: client module {} at 0x{:X}",
        module.name, module.base
    ));

    if extras.is_empty() {
        log("legacy ui patch: the extra ladder is empty, nothing to do");
        return None;
    }

    let image = unsafe { Image::parse(module.base) }?;
    let rdata = image.section(".rdata")?;
    let text = image.section(".text")?;

    let table = find_table(module.base, rdata)?;
    log(&format!(
        "legacy ui patch: bandwidth table at +0x{:X}",
        table - module.base
    ));

    let sites = find_reference_sites(module.base, text, table);
    if sites.is_empty() {
        log("legacy ui patch: no code references the bandwidth table, skipped");
        return None;
    }

    let new_count = BUILT_IN_COUNT + extras.len();
    let new_table = allocate_near(module.base, new_count * RECORD_STRIDE, &sites)?;
    unsafe {
        std::ptr::copy_nonoverlapping(
            table as *const u8,
            new_table as *mut u8,
            BUILT_IN_COUNT * RECORD_STRIDE,
        );
        for (index, mbps) in extras.iter().enumerate() {
            let record = new_table + (BUILT_IN_COUNT + index) * RECORD_STRIDE;
            std::ptr::write_bytes(record as *mut u8, 0, RECORD_STRIDE);
            let label = format!("{mbps} Mbps");
            debug_assert!(label.len() < 64);
            std::ptr::copy_nonoverlapping(label.as_ptr(), record as *mut u8, label.len());
            std::ptr::write_unaligned((record + VALUE_OFFSET) as *mut i32, *mbps);
        }
    }

    let mut patched_references = 0usize;
    let mut patched_counts = 0usize;
    let mut verify_failures = Vec::new();

    for (site, target) in &sites {
        let new_target = new_table + (target - table);
        let displacement = new_target as i64 - (*site as i64 + 7);
        if displacement.abs() > MAX_DISPLACEMENT {
            log(&format!(
                "legacy ui patch: reference at +0x{:X} is out of range for the new table",
                site - module.base
            ));
            continue;
        }
        let disp_offset = site + 3;
        if unsafe { write_code_u32(disp_offset, displacement as i32 as u32) }
            && unsafe { read_u32(disp_offset) } == displacement as i32 as u32
        {
            patched_references += 1;
        } else {
            verify_failures.push(site - module.base);
            continue;
        }

        let text_start = module.base + text.va;
        let search_from = (*site - text_start) + 7;
        let text_bytes = unsafe { std::slice::from_raw_parts(text_start as *const u8, text.size) };
        for imm_offset in find_count_immediates(text_bytes, search_from) {
            let va = text_start + imm_offset;
            if unsafe { write_code_u32(va, new_count as u32) }
                && unsafe { read_u32(va) } == new_count as u32
            {
                patched_counts += 1;
            } else {
                verify_failures.push(va - module.base);
            }
        }
    }

    if !verify_failures.is_empty() {
        log(&format!(
            "legacy ui patch: {} patched locations failed verification: {verify_failures:X?}",
            verify_failures.len()
        ));
    }

    log(&format!(
        "legacy ui patch: {} -> {} options, {} references and {} count immediates patched",
        BUILT_IN_COUNT, new_count, patched_references, patched_counts
    ));

    Some(Report {
        module: module.name.clone(),
        table_offset: table - module.base,
        reference_sites: sites.len(),
        patched_references,
        patched_counts,
        options: new_count,
    })
}

/// Finds a loaded module named `parsecd-*.dll` (or `parsecd.dll`).
fn find_client_module() -> Option<ModuleInfo> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<MODULEENTRY32W>() as u32;

        let mut found = None;
        if Module32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name = utf16_string(&entry.szModule);
                let lower = name.to_ascii_lowercase();
                let is_client = (lower.starts_with("parsecd-") || lower == "parsecd.dll")
                    && lower.ends_with(".dll");
                if is_client {
                    found = Some(ModuleInfo {
                        name,
                        base: entry.modBaseAddr as usize,
                    });
                    break;
                }
                if Module32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

/// Scans `.rdata` for the twelve-record bandwidth table.
fn find_table(base: usize, rdata: &Section) -> Option<usize> {
    let bytes = unsafe { std::slice::from_raw_parts((base + rdata.va) as *const u8, rdata.size) };
    let first_label = b"3 Mbps\0";
    let mut offset = 0usize;
    while offset + BUILT_IN_COUNT * RECORD_STRIDE <= bytes.len() {
        let candidate = match bytes[offset..]
            .windows(first_label.len())
            .position(|window| window == first_label)
        {
            Some(position) => offset + position,
            None => break,
        };
        // The records are read straight from memory, so a candidate whose
        // twelve records would run past the section is not a table: it could
        // be the tail of a string, and the bytes after it may not be mapped.
        if candidate + BUILT_IN_COUNT * RECORD_STRIDE > bytes.len() {
            break;
        }
        if is_bandwidth_table(base + rdata.va + candidate) {
            return Some(base + rdata.va + candidate);
        }
        offset = candidate + 1;
    }
    None
}

/// Verifies that `table` holds the exact twelve built-in records.
fn is_bandwidth_table(table: usize) -> bool {
    for (index, (label, value)) in BUILT_IN_OPTIONS.iter().enumerate() {
        let record = table + index * RECORD_STRIDE;
        let stored = unsafe { std::slice::from_raw_parts(record as *const u8, 64) };
        let end = stored.iter().position(|b| *b == 0).unwrap_or(64);
        if &stored[..end] != label.as_bytes() {
            return false;
        }
        let stored_value =
            unsafe { std::ptr::read_unaligned((record + VALUE_OFFSET) as *const i32) };
        if stored_value != *value {
            return false;
        }
    }
    true
}

/// Finds `lea reg, [rip+disp]` instructions whose target lies inside the table.
fn find_reference_sites(base: usize, text: &Section, table: usize) -> Vec<(usize, usize)> {
    let bytes = unsafe { std::slice::from_raw_parts((base + text.va) as *const u8, text.size) };
    let low = table;
    let high = table + BUILT_IN_COUNT * RECORD_STRIDE;
    let mut sites = Vec::new();
    let mut index = 0usize;
    while index + 7 <= bytes.len() {
        let opcode = bytes[index];
        let is_lea = (opcode == 0x48 || opcode == 0x4C)
            && bytes[index + 1] == 0x8D
            && (bytes[index + 2] & 0xC7) == 0x05;
        if !is_lea {
            index += 1;
            continue;
        }
        let site = base + text.va + index;
        let displacement = i32::from_le_bytes([
            bytes[index + 3],
            bytes[index + 4],
            bytes[index + 5],
            bytes[index + 6],
        ]) as i64;
        let target = (site as i64 + 7 + displacement) as usize;
        if target >= low && target < high {
            sites.push((site, target));
        }
        index += 7;
    }
    sites
}

/// Finds `mov r32, imm32` / `mov [rsp+0x20], imm32` immediates equal to the
/// built-in count inside the search window. Returns offsets of the immediates
/// relative to the `.text` slice.
fn find_count_immediates(bytes: &[u8], from: usize) -> Vec<usize> {
    let end = (from + COUNT_SEARCH_WINDOW).min(bytes.len());
    let mut offsets = Vec::new();
    let mut index = from;
    while index + 5 <= end {
        // mov dword ptr [rsp + 0x20], imm32
        if bytes[index] == 0xC7
            && bytes[index + 1] == 0x44
            && bytes[index + 2] == 0x24
            && bytes[index + 3] == 0x20
        {
            if index + 8 <= end {
                let imm = imm32(bytes, index + 4);
                if imm == BUILT_IN_COUNT as i32 {
                    offsets.push(index + 4);
                }
                index += 8;
                continue;
            }
        }
        // mov r32, imm32 (optionally with a REX prefix)
        let mov_at = if bytes[index] == 0x41 && (0xB8..=0xBF).contains(&bytes[index + 1]) {
            Some(index + 1)
        } else if (0xB8..=0xBF).contains(&bytes[index]) {
            Some(index)
        } else {
            None
        };
        if let Some(opcode) = mov_at {
            if opcode + 5 <= end {
                let imm = imm32(bytes, opcode + 1);
                if imm == BUILT_IN_COUNT as i32 {
                    offsets.push(opcode + 1);
                }
                index = opcode + 5;
                continue;
            }
        }
        index += 1;
    }
    offsets
}

/// Allocates `size` bytes close enough to every patch site for a 32-bit
/// RIP-relative displacement.
fn allocate_near(base: usize, size: usize, sites: &[(usize, usize)]) -> Option<usize> {
    let within_range = |address: usize| {
        sites.iter().all(|(site, _)| {
            (address as i64 - (*site as i64 + 7)).abs() <= MAX_DISPLACEMENT
                && (address as i64 - *site as i64).abs() <= MAX_DISPLACEMENT
        })
    };

    let mut hints = Vec::new();
    for step in 1..=48i64 {
        hints.push(base as i64 + step * 0x0200_0000);
        hints.push(base as i64 - step * 0x0200_0000);
    }
    for hint in hints {
        if hint <= 0 || !within_range(hint as usize) {
            continue;
        }
        let memory = unsafe {
            VirtualAlloc(
                hint as *const c_void,
                size,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if !memory.is_null() {
            return Some(memory as usize);
        }
    }
    None
}

unsafe fn write_code_u32(address: usize, value: u32) -> bool {
    let mut previous = 0u32;
    if VirtualProtect(
        address as *const c_void,
        4,
        PAGE_EXECUTE_READWRITE,
        &mut previous,
    ) == 0
    {
        return false;
    }
    std::ptr::write_unaligned(address as *mut u32, value);
    let mut ignored = 0u32;
    VirtualProtect(address as *const c_void, 4, previous, &mut ignored);
    true
}

unsafe fn read_u16(address: usize) -> u16 {
    std::ptr::read_unaligned(address as *const u16)
}

unsafe fn read_u32(address: usize) -> u32 {
    std::ptr::read_unaligned(address as *const u32)
}

fn imm32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

unsafe fn read_name(address: usize) -> String {
    let bytes = std::slice::from_raw_parts(address as *const u8, 8);
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(8);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn utf16_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use windows_sys::Win32::System::Memory::{
        VirtualFree, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_RELEASE,
    };

    // ---- a fake client module ---------------------------------------------
    //
    // A PE image built in memory with the two sections the patcher looks at:
    //
    //   .text   rva 0x1000   code that references the bandwidth table
    //   .rdata  rva 0x3000   the twelve-record bandwidth table
    //
    // The patcher only needs a base address to read from and write to, so a
    // page-aligned allocation stands in for the mapped `parsecd-*.dll`.

    const TEXT_RVA: usize = 0x1000;
    const TEXT_SIZE: usize = 0x2000;
    const RDATA_RVA: usize = 0x3000;
    const RDATA_SIZE: usize = 0x4000;
    const IMAGE_SIZE: usize = 0x8000;
    const TABLE_RVA: usize = 0x3100;

    /// `lea rcx, [rip+table]` followed by `mov r8d, 12`.
    const SITE_A: usize = 0x1100;
    /// `lea r8, [rip+table+2 records]` followed by `mov [rsp+20h], 12` and
    /// `mov ecx, 12`.
    const SITE_B: usize = 0x1400;
    /// `lea rax, [rip+...]` that points into .text, not at the table.
    const DECOY_LEA: usize = 0x1700;
    /// `mov ecx, 12` before the first reference: not part of any window.
    const DECOY_BEFORE: usize = 0x10F0;
    /// `mov ecx, 12` too far behind the reference.
    const DECOY_FAR: usize = SITE_A + 7 + COUNT_SEARCH_WINDOW + 0x10;

    struct Layout {
        text_name: &'static str,
        rdata_name: &'static str,
        optional_magic: u16,
        mz: bool,
        table: bool,
        code: bool,
        /// Record whose value is corrupted, if any.
        corrupt_record: Option<usize>,
    }

    impl Default for Layout {
        fn default() -> Layout {
            Layout {
                text_name: ".text",
                rdata_name: ".rdata",
                optional_magic: 0x20B,
                mz: true,
                table: true,
                code: true,
                corrupt_record: None,
            }
        }
    }

    struct FakeImage {
        base: usize,
    }

    impl FakeImage {
        fn new(layout: Layout) -> FakeImage {
            let memory = unsafe {
                VirtualAlloc(
                    std::ptr::null(),
                    IMAGE_SIZE,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            assert!(!memory.is_null(), "VirtualAlloc failed");
            let image = FakeImage {
                base: memory as usize,
            };

            if layout.mz {
                image.write(0, b"MZ");
            }
            image.write(0x3C, &0x80u32.to_le_bytes());
            image.write(0x80, b"PE\0\0");
            image.write(0x80 + 6, &2u16.to_le_bytes()); // sections
            image.write(0x80 + 20, &0xF0u16.to_le_bytes()); // optional header size
            image.write(0x80 + 24, &layout.optional_magic.to_le_bytes());
            let sections = 0x80 + 24 + 0xF0;
            for (index, (name, rva, size)) in [
                (layout.text_name, TEXT_RVA, TEXT_SIZE),
                (layout.rdata_name, RDATA_RVA, RDATA_SIZE),
            ]
            .into_iter()
            .enumerate()
            {
                let header = sections + index * 40;
                image.write(header, name.as_bytes());
                image.write(header + 8, &(size as u32).to_le_bytes());
                image.write(header + 12, &(rva as u32).to_le_bytes());
            }

            if layout.table {
                image.write_table(TABLE_RVA, layout.corrupt_record);
            }
            if layout.code {
                image.write_code();
            }
            image
        }

        fn at(&self, rva: usize) -> usize {
            self.base + rva
        }

        fn write(&self, rva: usize, bytes: &[u8]) {
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.at(rva) as *mut u8, bytes.len())
            };
        }

        fn read(&self, rva: usize, len: usize) -> Vec<u8> {
            unsafe { std::slice::from_raw_parts(self.at(rva) as *const u8, len) }.to_vec()
        }

        fn u32(&self, rva: usize) -> u32 {
            unsafe { read_u32(self.at(rva)) }
        }

        fn write_table(&self, rva: usize, corrupt: Option<usize>) {
            for (index, (label, value)) in BUILT_IN_OPTIONS.iter().enumerate() {
                let record = rva + index * RECORD_STRIDE;
                self.write(record, label.as_bytes());
                let value = if corrupt == Some(index) {
                    *value + 1
                } else {
                    *value
                };
                self.write(record + VALUE_OFFSET, &value.to_le_bytes());
            }
        }

        /// `lea reg, [rip + target]` at `site`.
        fn lea(&self, site: usize, rex: u8, modrm: u8, target: usize) {
            let displacement = target as i64 - (site as i64 + 7);
            let mut bytes = vec![rex, 0x8D, modrm];
            bytes.extend_from_slice(&(displacement as i32).to_le_bytes());
            self.write(site, &bytes);
        }

        fn write_code(&self) {
            self.write(TEXT_RVA, &vec![0x90; TEXT_SIZE]); // nops
            self.write(DECOY_BEFORE, &[0xB9, 12, 0, 0, 0]);

            self.lea(SITE_A, 0x48, 0x0D, TABLE_RVA);
            self.write(SITE_A + 7, &[0x41, 0xB8, 12, 0, 0, 0]);
            self.write(DECOY_FAR, &[0xB9, 12, 0, 0, 0]);

            self.lea(SITE_B, 0x4C, 0x05, TABLE_RVA + 2 * RECORD_STRIDE);
            self.write(SITE_B + 7, &[0xC7, 0x44, 0x24, 0x20, 12, 0, 0, 0]);
            self.write(SITE_B + 15, &[0xB9, 12, 0, 0, 0]);

            self.lea(DECOY_LEA, 0x48, 0x05, TEXT_RVA);
        }

        fn module(&self) -> ModuleInfo {
            ModuleInfo {
                name: "parsecd-test.dll".to_string(),
                base: self.base,
            }
        }
    }

    impl Drop for FakeImage {
        fn drop(&mut self) {
            unsafe { VirtualFree(self.base as *mut c_void, 0, MEM_RELEASE) };
        }
    }

    fn extend(image: &FakeImage) -> (Option<Report>, Vec<String>) {
        let logs = RefCell::new(Vec::new());
        let extras = crate::default_extras();
        let report = try_extend(&image.module(), &extras, &|message: &str| {
            logs.borrow_mut().push(message.to_string())
        });
        (report, logs.into_inner())
    }

    fn label_at(address: usize) -> String {
        let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, 64) };
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(64);
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    fn value_at(record: usize) -> i32 {
        unsafe { std::ptr::read_unaligned((record + VALUE_OFFSET) as *const i32) }
    }

    /// Absolute address a patched `lea` at `rva` now points to.
    fn lea_target(image: &FakeImage, rva: usize) -> usize {
        let displacement = image.u32(rva + 3) as i32 as i64;
        (image.at(rva) as i64 + 7 + displacement) as usize
    }

    // ---- the whole patch ------------------------------------------------------

    #[test]
    fn extends_the_table_and_repoints_every_reference() {
        let image = FakeImage::new(Layout::default());
        let original_table = image.read(TABLE_RVA, BUILT_IN_COUNT * RECORD_STRIDE);

        let (report, logs) = extend(&image);
        let report = report.expect("the fake client is patchable");
        let extras = crate::default_extras();

        assert_eq!(report.module, "parsecd-test.dll");
        assert_eq!(report.table_offset, TABLE_RVA);
        assert_eq!(report.reference_sites, 2);
        assert_eq!(report.patched_references, 2);
        assert_eq!(report.patched_counts, 3);
        assert_eq!(report.options, BUILT_IN_COUNT + extras.len());
        assert_eq!(report.options, BUILT_IN_COUNT + extras.len());

        // The new table: the original twelve records, then the extras.
        let new_table = lea_target(&image, SITE_A);
        assert_ne!(new_table, image.at(TABLE_RVA));
        for (index, (label, value)) in BUILT_IN_OPTIONS.iter().enumerate() {
            let record = new_table + index * RECORD_STRIDE;
            assert_eq!(label_at(record), *label);
            assert_eq!(value_at(record), *value);
        }
        for (index, mbps) in extras.iter().enumerate() {
            let record = new_table + (BUILT_IN_COUNT + index) * RECORD_STRIDE;
            assert_eq!(label_at(record), format!("{mbps} Mbps"));
            assert_eq!(value_at(record), *mbps);
            // The rest of the record is zeroed.
            let tail = unsafe {
                std::slice::from_raw_parts((record + 0x44) as *const u8, RECORD_STRIDE - 0x44)
            };
            assert!(tail.iter().all(|b| *b == 0));
        }

        // A reference into the middle of the table keeps its record offset.
        let second = lea_target(&image, SITE_B);
        assert_eq!(second, new_table + 2 * RECORD_STRIDE);
        assert_eq!(label_at(second), "7 Mbps");

        // The instruction bytes around the displacement are untouched.
        assert_eq!(image.read(SITE_A, 3), [0x48, 0x8D, 0x0D]);
        assert_eq!(image.read(SITE_B, 3), [0x4C, 0x8D, 0x05]);

        // Every count immediate now says 22; the opcodes in front are intact.
        assert_eq!(image.read(SITE_A + 7, 2), [0x41, 0xB8]);
        assert_eq!(image.u32(SITE_A + 9), 22);
        assert_eq!(image.read(SITE_B + 7, 4), [0xC7, 0x44, 0x24, 0x20]);
        assert_eq!(image.u32(SITE_B + 11), 22);
        assert_eq!(image.read(SITE_B + 15, 1), [0xB9]);
        assert_eq!(image.u32(SITE_B + 16), 22);

        // Nothing else moved: the original table, the decoys.
        assert_eq!(
            image.read(TABLE_RVA, BUILT_IN_COUNT * RECORD_STRIDE),
            original_table
        );
        assert_eq!(image.u32(DECOY_BEFORE + 1), 12);
        assert_eq!(image.u32(DECOY_FAR + 1), 12);
        assert_eq!(lea_target(&image, DECOY_LEA), image.at(TEXT_RVA));

        assert!(logs
            .iter()
            .any(|line| line.contains("client module parsecd-test.dll")));
        assert!(logs
            .iter()
            .any(|line| line.contains(&format!("bandwidth table at +0x{TABLE_RVA:X}"))));
        assert!(logs
            .iter()
            .any(|line| line
                .contains("12 -> 22 options, 2 references and 3 count immediates patched")));
    }

    #[test]
    fn patching_twice_is_a_no_op() {
        let image = FakeImage::new(Layout::default());
        assert!(extend(&image).0.is_some());
        let patched_code = image.read(TEXT_RVA, TEXT_SIZE);

        let (second, logs) = extend(&image);
        assert!(
            second.is_none(),
            "the original table is no longer referenced"
        );
        assert!(logs
            .iter()
            .any(|line| line.contains("no code references the bandwidth table")));
        assert_eq!(
            image.read(TEXT_RVA, TEXT_SIZE),
            patched_code,
            "no second round of edits"
        );
    }

    #[test]
    fn leaves_everything_alone_when_the_table_is_unknown() {
        // A client build whose table differs (one value changed) is not ours
        // to patch.
        let image = FakeImage::new(Layout {
            corrupt_record: Some(7),
            ..Layout::default()
        });
        let code = image.read(TEXT_RVA, TEXT_SIZE);
        let (report, _) = extend(&image);
        assert!(report.is_none());
        assert_eq!(image.read(TEXT_RVA, TEXT_SIZE), code);

        let image = FakeImage::new(Layout {
            table: false,
            ..Layout::default()
        });
        assert!(extend(&image).0.is_none());
    }

    #[test]
    fn skips_a_client_where_no_code_uses_the_table() {
        let image = FakeImage::new(Layout {
            code: false,
            ..Layout::default()
        });
        let (report, logs) = extend(&image);
        assert!(report.is_none());
        assert!(logs
            .iter()
            .any(|line| line.contains("no code references the bandwidth table, skipped")));
    }

    #[test]
    fn refuses_images_it_cannot_parse() {
        for layout in [
            Layout {
                mz: false,
                ..Layout::default()
            },
            Layout {
                optional_magic: 0x10B,
                ..Layout::default()
            }, // PE32
            Layout {
                text_name: ".code",
                ..Layout::default()
            },
            Layout {
                rdata_name: ".data",
                ..Layout::default()
            },
        ] {
            let image = FakeImage::new(layout);
            assert!(extend(&image).0.is_none());
        }
    }

    #[test]
    fn a_missing_pe_signature_is_rejected() {
        let image = FakeImage::new(Layout::default());
        image.write(0x80, b"XX\0\0");
        assert!(unsafe { Image::parse(image.base) }.is_none());
    }

    #[test]
    fn parse_lists_the_sections_with_their_rvas() {
        let image = FakeImage::new(Layout::default());
        let parsed = unsafe { Image::parse(image.base) }.unwrap();
        let text = parsed.section(".text").unwrap();
        assert_eq!((text.va, text.size), (TEXT_RVA, TEXT_SIZE));
        let rdata = parsed.section(".rdata").unwrap();
        assert_eq!((rdata.va, rdata.size), (RDATA_RVA, RDATA_SIZE));
        assert!(parsed.section(".data").is_none());
    }

    // ---- find_table / is_bandwidth_table ---------------------------------------

    /// A heap buffer standing in for a section; `base` is its address and the
    /// section starts at offset 0.
    fn section_of(bytes: &[u8]) -> (Vec<u8>, Section) {
        let owned = bytes.to_vec();
        let section = Section {
            name: ".rdata".into(),
            va: 0,
            size: owned.len(),
        };
        (owned, section)
    }

    fn table_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; BUILT_IN_COUNT * RECORD_STRIDE];
        for (index, (label, value)) in BUILT_IN_OPTIONS.iter().enumerate() {
            let record = index * RECORD_STRIDE;
            bytes[record..record + label.len()].copy_from_slice(label.as_bytes());
            bytes[record + VALUE_OFFSET..record + VALUE_OFFSET + 4]
                .copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn is_bandwidth_table_requires_every_label_and_value() {
        let good = table_bytes();
        assert!(is_bandwidth_table(good.as_ptr() as usize));

        for record in 0..BUILT_IN_COUNT {
            let mut bad = good.clone();
            bad[record * RECORD_STRIDE] = b'X';
            assert!(
                !is_bandwidth_table(bad.as_ptr() as usize),
                "label of record {record}"
            );

            let mut bad = good.clone();
            bad[record * RECORD_STRIDE + VALUE_OFFSET] ^= 1;
            assert!(
                !is_bandwidth_table(bad.as_ptr() as usize),
                "value of record {record}"
            );
        }

        // A label that merely starts like the real one.
        let mut longer = good.clone();
        longer[6] = b's'; // "3 Mbpss"
        assert!(!is_bandwidth_table(longer.as_ptr() as usize));
    }

    #[test]
    fn find_table_locates_the_table_after_unrelated_data() {
        let mut bytes = vec![0xAAu8; 0x40];
        bytes.extend_from_slice(b"3 Mbps\0 not a table, just text that shares the first label\0");
        bytes.resize(0x200, 0);
        let real = bytes.len();
        bytes.extend(table_bytes());
        bytes.extend(vec![0u8; 0x80]);

        let (owned, section) = section_of(&bytes);
        assert_eq!(
            find_table(owned.as_ptr() as usize, &section),
            Some(owned.as_ptr() as usize + real)
        );
    }

    #[test]
    fn find_table_returns_the_first_of_two_tables() {
        let mut bytes = table_bytes();
        bytes.extend(table_bytes());
        let (owned, section) = section_of(&bytes);
        assert_eq!(
            find_table(owned.as_ptr() as usize, &section),
            Some(owned.as_ptr() as usize)
        );
    }

    #[test]
    fn find_table_finds_nothing_in_unrelated_data() {
        let (owned, section) = section_of(&vec![0x41u8; 0x2000]);
        assert_eq!(find_table(owned.as_ptr() as usize, &section), None);
        let (owned, section) = section_of(&[]);
        assert_eq!(find_table(owned.as_ptr() as usize, &section), None);
        let (owned, section) = section_of(b"3 Mbps\0");
        assert_eq!(find_table(owned.as_ptr() as usize, &section), None);
    }

    /// The scan must stay inside the section: a candidate whose twelve records
    /// would run past the end is not a table, even if the bytes that follow
    /// the section happen to look like one.
    #[test]
    fn find_table_never_accepts_a_table_that_runs_past_the_section() {
        // Plenty of room before the table, so the scan reaches it.
        let mut bytes = vec![0u8; 0x1000];
        bytes.extend(table_bytes());
        let section_end = bytes.len() - RECORD_STRIDE; // last record sticks out
        let owned = bytes;
        let section = Section {
            name: ".rdata".into(),
            va: 0,
            size: section_end,
        };
        assert_eq!(find_table(owned.as_ptr() as usize, &section), None);
    }

    // ---- find_reference_sites -------------------------------------------------------

    #[test]
    fn reference_sites_are_the_rip_relative_leas_into_the_table() {
        let image = FakeImage::new(Layout::default());
        let text = Section {
            name: ".text".into(),
            va: TEXT_RVA,
            size: TEXT_SIZE,
        };
        let table = image.at(TABLE_RVA);

        let sites = find_reference_sites(image.base, &text, table);
        assert_eq!(
            sites,
            vec![
                (image.at(SITE_A), table),
                (image.at(SITE_B), table + 2 * RECORD_STRIDE),
            ]
        );
    }

    #[test]
    fn reference_sites_include_the_first_and_last_byte_of_the_table_range() {
        let image = FakeImage::new(Layout::default());
        let table = image.at(TABLE_RVA);
        image.write(TEXT_RVA, &vec![0x90; TEXT_SIZE]);
        image.lea(0x1200, 0x48, 0x0D, TABLE_RVA); // first byte
        image.lea(
            0x1210,
            0x48,
            0x0D,
            TABLE_RVA + BUILT_IN_COUNT * RECORD_STRIDE - 1,
        ); // last byte
        image.lea(
            0x1220,
            0x48,
            0x0D,
            TABLE_RVA + BUILT_IN_COUNT * RECORD_STRIDE,
        ); // one past
        image.lea(0x1230, 0x48, 0x0D, TABLE_RVA - 1); // one before
        let text = Section {
            name: ".text".into(),
            va: TEXT_RVA,
            size: TEXT_SIZE,
        };

        let sites: Vec<usize> = find_reference_sites(image.base, &text, table)
            .into_iter()
            .map(|(site, _)| site - image.base)
            .collect();
        assert_eq!(sites, [0x1200, 0x1210]);
    }

    #[test]
    fn reference_sites_understand_both_rex_prefixes_and_any_register() {
        let image = FakeImage::new(Layout::default());
        let table = image.at(TABLE_RVA);
        image.write(TEXT_RVA, &vec![0x90; TEXT_SIZE]);
        // 48 8D /r with modrm 05, 0D, 15, ... and 4C 8D (r8-r15 destinations).
        for (index, modrm) in [0x05u8, 0x0D, 0x15, 0x1D, 0x25, 0x2D, 0x35, 0x3D]
            .into_iter()
            .enumerate()
        {
            image.lea(0x1200 + index * 16, 0x48, modrm, TABLE_RVA);
            image.lea(0x1400 + index * 16, 0x4C, modrm, TABLE_RVA);
        }
        // Not a RIP-relative lea (modrm mod != 00): ignored.
        image.write(0x1600, &[0x48, 0x8D, 0x4C, 0x24, 0x08, 0x90, 0x90]);
        // A different opcode with the same tail.
        image.write(0x1610, &[0x48, 0x8B, 0x0D, 0, 0, 0, 0]);
        let text = Section {
            name: ".text".into(),
            va: TEXT_RVA,
            size: TEXT_SIZE,
        };

        assert_eq!(find_reference_sites(image.base, &text, table).len(), 16);
    }

    #[test]
    fn reference_sites_do_not_read_past_the_end_of_the_section() {
        let image = FakeImage::new(Layout::default());
        let table = image.at(TABLE_RVA);
        image.write(TEXT_RVA, &vec![0x90; TEXT_SIZE]);
        // A reference whose last bytes fall outside the section is not seen.
        image.lea(TEXT_RVA + TEXT_SIZE - 7, 0x48, 0x0D, TABLE_RVA);
        let short = Section {
            name: ".text".into(),
            va: TEXT_RVA,
            size: TEXT_SIZE - 3,
        };
        assert!(find_reference_sites(image.base, &short, table).is_empty());

        let whole = Section {
            name: ".text".into(),
            va: TEXT_RVA,
            size: TEXT_SIZE,
        };
        assert_eq!(find_reference_sites(image.base, &whole, table).len(), 1);
    }

    // ---- find_count_immediates ----------------------------------------------------------

    const TWELVE: [u8; 4] = [12, 0, 0, 0];

    fn code(parts: &[&[u8]]) -> Vec<u8> {
        parts.iter().flat_map(|part| part.iter().copied()).collect()
    }

    #[test]
    fn count_immediates_cover_every_mov_encoding_the_client_uses() {
        // mov eax..edi, 12 (B8..BF), with and without a REX.B prefix, and the
        // stack store form.
        for opcode in 0xB8u8..=0xBF {
            let bytes = code(&[&[0x90, 0x90], &[opcode], &TWELVE]);
            assert_eq!(
                find_count_immediates(&bytes, 0),
                [3],
                "mov r32, 12 ({opcode:#X})"
            );

            let bytes = code(&[&[0x90], &[0x41, opcode], &TWELVE]);
            assert_eq!(
                find_count_immediates(&bytes, 0),
                [3],
                "mov r8d.., 12 ({opcode:#X})"
            );
        }
        let bytes = code(&[&[0xC7, 0x44, 0x24, 0x20], &TWELVE]);
        assert_eq!(find_count_immediates(&bytes, 0), [4]);
    }

    #[test]
    fn count_immediates_ignore_other_values_and_other_stores() {
        let bytes = code(&[
            &[0xB9],
            &[11, 0, 0, 0],
            &[0xB9],
            &[13, 0, 0, 0],
            &[0xB9],
            &[12, 0, 1, 0],
        ]);
        assert!(find_count_immediates(&bytes, 0).is_empty());
        // Same store form with a different stack slot.
        let bytes = code(&[&[0xC7, 0x44, 0x24, 0x28], &TWELVE]);
        assert!(find_count_immediates(&bytes, 0).is_empty());
    }

    #[test]
    fn count_immediates_report_every_hit_in_the_window() {
        let bytes = code(&[
            &[0xB9],
            &TWELVE,
            &[0x90],
            &[0x41, 0xB8],
            &TWELVE,
            &[0xC7, 0x44, 0x24, 0x20],
            &TWELVE,
        ]);
        assert_eq!(find_count_immediates(&bytes, 0), [1, 8, 16]);
    }

    #[test]
    fn count_immediates_start_at_the_given_offset() {
        let bytes = code(&[&[0xB9], &TWELVE, &[0x90, 0x90], &[0xB9], &TWELVE]);
        assert_eq!(find_count_immediates(&bytes, 0), [1, 8]);
        assert_eq!(find_count_immediates(&bytes, 5), [8]);
        assert!(find_count_immediates(&bytes, 9).is_empty());
        assert!(find_count_immediates(&bytes, 10_000).is_empty());
    }

    #[test]
    fn count_immediates_stop_at_the_search_window() {
        // One hit whose immediate ends exactly at the window edge, one past.
        let mut inside = vec![0x90; COUNT_SEARCH_WINDOW];
        let at = COUNT_SEARCH_WINDOW - 5;
        inside[at] = 0xB9;
        inside[at + 1..at + 5].copy_from_slice(&TWELVE);
        inside.extend(vec![0x90; 16]);
        assert_eq!(find_count_immediates(&inside, 0), [at + 1]);

        let mut outside = vec![0x90; COUNT_SEARCH_WINDOW + 16];
        let at = COUNT_SEARCH_WINDOW - 4; // would end one byte past the window
        outside[at] = 0xB9;
        outside[at + 1..at + 5].copy_from_slice(&TWELVE);
        assert!(find_count_immediates(&outside, 0).is_empty());
    }

    #[test]
    fn count_immediates_tolerate_truncated_instructions_at_the_end() {
        assert!(find_count_immediates(&[], 0).is_empty());
        assert!(find_count_immediates(&[0xB9], 0).is_empty());
        assert!(find_count_immediates(&[0xB9, 12, 0, 0], 0).is_empty());
        assert!(find_count_immediates(&[0xC7, 0x44, 0x24, 0x20, 12, 0, 0], 0).is_empty());
        assert!(find_count_immediates(&[0x41], 0).is_empty());
    }

    // ---- allocate_near -----------------------------------------------------------------------

    #[test]
    fn allocate_near_returns_writable_memory_within_reach_of_every_site() {
        let image = FakeImage::new(Layout::default());
        let sites = [(image.at(SITE_A), 0usize), (image.at(SITE_B), 0usize)];
        let size = 22 * RECORD_STRIDE;

        let memory =
            allocate_near(image.base, size, &sites).expect("address space is free near the image");
        for (site, _) in sites {
            let displacement = memory as i64 - (site as i64 + 7);
            assert!(displacement.abs() <= MAX_DISPLACEMENT, "{displacement}");
        }
        unsafe {
            std::ptr::write_bytes(memory as *mut u8, 0xAB, size);
            assert_eq!(*((memory + size - 1) as *const u8), 0xAB);
            VirtualFree(memory as *mut c_void, 0, MEM_RELEASE);
        }
    }

    #[test]
    fn allocate_near_gives_up_when_the_sites_are_too_far_apart() {
        // No single address is within 2 GB of both.
        let gigabyte = 1024 * 1024 * 1024usize;
        let sites = [(4 * gigabyte, 0usize), (9 * gigabyte, 0usize)];
        assert_eq!(allocate_near(6 * gigabyte, 0x1000, &sites), None);
    }

    // ---- low level helpers --------------------------------------------------------------------

    #[test]
    fn write_code_u32_changes_protected_memory_and_restores_the_protection() {
        let page = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                0x1000,
                MEM_COMMIT | MEM_RESERVE,
                0x20, /* PAGE_EXECUTE_READ */
            )
        } as usize;
        assert_ne!(page, 0);

        assert!(unsafe { write_code_u32(page + 0x10, 0xDEADBEEF) });
        assert_eq!(unsafe { read_u32(page + 0x10) }, 0xDEADBEEF);

        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        unsafe {
            VirtualQuery(
                page as *const c_void,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        assert_eq!(info.Protect, 0x20, "protection is restored");
        unsafe { VirtualFree(page as *mut c_void, 0, MEM_RELEASE) };
    }

    #[test]
    fn write_code_u32_reports_failure_on_unmapped_memory() {
        assert!(!unsafe { write_code_u32(0x10, 1) });
    }

    #[test]
    fn small_readers_and_decoders() {
        let bytes = [0x34u8, 0x12, 0x78, 0x56, 0x34, 0x12, 0xFF];
        assert_eq!(unsafe { read_u16(bytes.as_ptr() as usize) }, 0x1234);
        assert_eq!(
            unsafe { read_u32(bytes.as_ptr() as usize + 2) },
            0x1234_5678
        );
        assert_eq!(imm32(&bytes, 2), 0x1234_5678);
        assert_eq!(imm32(&[0xFF, 0xFF, 0xFF, 0xFF], 0), -1);

        let name = *b".rdata\0\0";
        assert_eq!(unsafe { read_name(name.as_ptr() as usize) }, ".rdata");
        let full = *b"12345678";
        assert_eq!(unsafe { read_name(full.as_ptr() as usize) }, "12345678");

        let wide: Vec<u16> = "parsecd-1.dll\0garbage".encode_utf16().collect();
        assert_eq!(utf16_string(&wide), "parsecd-1.dll");
        let unterminated: Vec<u16> = "abc".encode_utf16().collect();
        assert_eq!(utf16_string(&unterminated), "abc");
        assert_eq!(utf16_string(&[]), "");
    }

    #[test]
    fn built_in_options_are_the_documented_ladder() {
        assert_eq!(
            BUILT_IN_OPTIONS
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>(),
            [3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50]
        );
        for (label, value) in BUILT_IN_OPTIONS {
            assert_eq!(label, format!("{value} Mbps"));
        }
        assert_eq!(BUILT_IN_OPTIONS.len(), BUILT_IN_COUNT);
        assert_eq!(RECORD_STRIDE, 0x144);
    }

    // ---- waiting for the client module ------------------------------------------------------------

    #[test]
    fn no_client_module_is_loaded_in_the_test_process() {
        assert!(find_client_module().is_none());
    }

    #[test]
    fn waiting_gives_up_after_the_timeout_and_says_so() {
        let logs = RefCell::new(Vec::new());
        let started = Instant::now();
        let report = extend_bandwidth_when_ready(
            &|message: &str| logs.borrow_mut().push(message.to_string()),
            Duration::from_millis(400),
        );
        assert!(report.is_none());
        assert!(started.elapsed() >= Duration::from_millis(400));
        assert!(logs
            .borrow()
            .iter()
            .any(|line| line.contains("never loaded")));
    }
}
