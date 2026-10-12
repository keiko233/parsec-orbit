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
//! 1. allocates a copy extended with [`crate::EXTRA_BANDWIDTH_MBPS`],
//! 2. rewrites the two `lea` displacements to point at the copy (the
//!    displacement is relative, so the copy has to live within 2 GB of the
//!    code - the allocator asks for addresses near the module), and
//! 3. patches the two count immediates from `12` to the new length.
//!
//! Every patched location is read back afterwards, and the report tells the
//! loader what happened. All edits are in memory only.

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
    extend_bandwidth_when_ready(&log, Duration::from_secs(10))
}

/// Same as [`extend_bandwidth`], but waits for the client module to be loaded
/// first: the native UI lives in `parsecd-<version>.dll`, which the executable
/// maps after start-up.
pub fn extend_bandwidth_when_ready<F: Fn(&str)>(log: &F, timeout: Duration) -> Option<Report> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(module) = find_client_module() {
            return try_extend(&module, log);
        }
        if Instant::now() >= deadline {
            log("legacy ui patch: the client module never loaded, skipped");
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn try_extend<F: Fn(&str)>(module: &ModuleInfo, log: &F) -> Option<Report> {
    log(&format!(
        "legacy ui patch: client module {} at 0x{:X}",
        module.name, module.base
    ));

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

    let extras = crate::EXTRA_BANDWIDTH_MBPS;
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
