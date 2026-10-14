//! The real implementations behind the symbols this library interposes.
//!
//! `dlsym` can return the interposing function again. Resolve the original
//! functions from libsystem_info's Mach-O export trie instead, which dyld does
//! not rewrite. The image is part of the dyld shared cache on current macOS
//! releases, but dyld exposes its mapped header and image name in the same way.

use std::ffi::{c_void, CStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};

const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_SEGMENT_64: u32 = 0x19;
const LC_DYLD_INFO: u32 = 0x22;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x8000_0033;
const MAX_LOAD_COMMAND_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRIE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRIE_STEPS: usize = 4096;
const NSS_IMAGE_PATHS: [&[u8]; 2] = [
    b"/usr/lib/system/libsystem_info.dylib",
    b"/usr/lib/libsystem_info.dylib",
];

/// The callers that need a symbol from another image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Symbol {
    /// `int getpwuid_r(uid_t, struct passwd *, char *, size_t, struct passwd **)`.
    GetpwuidR,
    /// `struct passwd *getpwuid(uid_t)`.
    Getpwuid,
}

impl Symbol {
    /// The C symbol name, without Mach-O's leading underscore.
    pub fn name(self) -> &'static str {
        match self {
            Symbol::GetpwuidR => "getpwuid_r",
            Symbol::Getpwuid => "getpwuid",
        }
    }
}

mod dyld {
    unsafe extern "C" {
        pub fn _dyld_image_count() -> u32;
        pub fn _dyld_get_image_header(index: u32) -> *const u8;
        pub fn _dyld_get_image_name(index: u32) -> *const std::ffi::c_char;
    }
}

/// Paths of every image dyld currently has mapped, in load order.
///
/// The readiness policy needs a fresh snapshot on every probe, so this reads
/// the live dyld table instead of caching. An image that disappears between
/// the count and the read is skipped rather than failing the snapshot.
pub fn mapped_images() -> Vec<PathBuf> {
    let count = unsafe { dyld::_dyld_image_count() };
    let mut images = Vec::with_capacity(count as usize);
    for index in 0..count {
        let name = unsafe { dyld::_dyld_get_image_name(index) };
        if name.is_null() {
            continue;
        }
        // Safety: dyld owns the NUL-terminated image path for the process
        // lifetime.
        let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
        images.push(PathBuf::from(Path::new(std::ffi::OsStr::from_bytes(bytes))));
    }
    images
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    Some(u64::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

/// Reads one bounded unsigned LEB128 value and advances the cursor.
fn read_uleb128(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for index in 0..10 {
        let byte = *bytes.get(*cursor)?;
        *cursor = cursor.checked_add(1)?;
        let payload = u64::from(byte & 0x7f);
        let shift = index * 7;
        if shift == 63 && payload > 1 {
            return None;
        }
        value |= payload.checked_shl(shift as u32)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportAddress {
    Regular(u64),
    Absolute(u64),
}

/// Reads a NUL-terminated edge string and returns its bytes and end position.
fn edge_at(bytes: &[u8], cursor: usize) -> Option<(&[u8], usize)> {
    let rest = bytes.get(cursor..)?;
    let length = rest.iter().position(|byte| *byte == 0)?;
    Some((&rest[..length], cursor.checked_add(length)?.checked_add(1)?))
}

/// Looks up one complete export name by following matching compressed edges.
///
/// The step limit also rejects cycles and hostile tries with excessive depth.
fn trie_lookup(bytes: &[u8], name: &str) -> Option<ExportAddress> {
    let wanted = name.as_bytes();
    let mut node = 0usize;
    let mut matched = 0usize;

    for _ in 0..MAX_TRIE_STEPS {
        let mut cursor = node;
        let terminal_size = usize::try_from(read_uleb128(bytes, &mut cursor)?).ok()?;
        let terminal_end = cursor.checked_add(terminal_size)?;
        let terminal = bytes.get(cursor..terminal_end)?;
        cursor = terminal_end;

        if matched == wanted.len() {
            if terminal.is_empty() {
                return None;
            }
            let mut terminal_cursor = 0;
            let flags = read_uleb128(terminal, &mut terminal_cursor)?;
            // Reexports and stub/resolver exports do not identify a callable
            // implementation in this image.
            if flags & (0x08 | 0x10) != 0 {
                return None;
            }
            let address = read_uleb128(terminal, &mut terminal_cursor)?;
            if terminal_cursor != terminal.len() {
                return None;
            }
            return match flags & 0x03 {
                0x00 => Some(ExportAddress::Regular(address)),
                0x02 => Some(ExportAddress::Absolute(address)),
                _ => None,
            };
        }

        let child_count = usize::from(*bytes.get(cursor)?);
        cursor = cursor.checked_add(1)?;
        let mut selected = None;
        for _ in 0..child_count {
            let (edge, after_edge) = edge_at(bytes, cursor)?;
            if edge.is_empty() {
                return None;
            }
            cursor = after_edge;
            let child = usize::try_from(read_uleb128(bytes, &mut cursor)?).ok()?;
            if wanted[matched..].starts_with(edge) {
                if selected.is_some() {
                    return None;
                }
                selected = Some((child, edge.len()));
            }
        }
        let (child, edge_len) = selected?;
        matched = matched.checked_add(edge_len)?;
        node = child;
        if node >= bytes.len() {
            return None;
        }
    }
    None
}

/// File offset and size of the export trie. LC_DYLD_INFO stores the export
/// fields at command offsets +40/+44; LC_DYLD_EXPORTS_TRIE stores them at +8/+12.
fn export_trie_range(commands: &[u8], ncmds: usize) -> Option<(u64, u64)> {
    let mut cursor = 0usize;
    let mut standalone = None;
    let mut dyld_info = None;
    for _ in 0..ncmds {
        let command = u32_at(commands, cursor)?;
        let size = usize::try_from(u32_at(commands, cursor.checked_add(4)?)?).ok()?;
        let end = cursor.checked_add(size)?;
        if size < 8 || end > commands.len() {
            return None;
        }
        match command {
            LC_DYLD_EXPORTS_TRIE => {
                if size < 16 {
                    return None;
                }
                standalone = Some((
                    u64::from(u32_at(commands, cursor + 8)?),
                    u64::from(u32_at(commands, cursor + 12)?),
                ));
            }
            LC_DYLD_INFO | LC_DYLD_INFO_ONLY => {
                if size < 48 {
                    return None;
                }
                dyld_info = Some((
                    u64::from(u32_at(commands, cursor + 40)?),
                    u64::from(u32_at(commands, cursor + 44)?),
                ));
            }
            _ => {}
        }
        cursor = end;
    }
    standalone
        .filter(|(_, size)| *size != 0)
        .or_else(|| dyld_info.filter(|(_, size)| *size != 0))
}

#[derive(Clone, Copy)]
struct Linkedit {
    fileoff: u64,
    vmaddr: u64,
    filesize: u64,
    image_vmaddr: u64,
}

fn linkedit_range(commands: &[u8], ncmds: usize) -> Option<Linkedit> {
    let mut cursor = 0usize;
    let mut image_vmaddr = None;
    let mut linkedit = None;
    for _ in 0..ncmds {
        let command = u32_at(commands, cursor)?;
        let size = usize::try_from(u32_at(commands, cursor.checked_add(4)?)?).ok()?;
        let end = cursor.checked_add(size)?;
        if size < 8 || end > commands.len() {
            return None;
        }
        if command == LC_SEGMENT_64 {
            if size < 72 {
                return None;
            }
            let name = commands.get(cursor + 8..cursor + 24)?;
            let vmaddr = u64_at(commands, cursor + 24)?;
            let fileoff = u64_at(commands, cursor + 40)?;
            let filesize = u64_at(commands, cursor + 48)?;
            if name.starts_with(b"__TEXT\0") && image_vmaddr.is_none() {
                image_vmaddr = Some(vmaddr);
            }
            if name.starts_with(b"__LINKEDIT\0") {
                linkedit = Some((vmaddr, fileoff, filesize));
            }
        }
        cursor = end;
    }
    let (vmaddr, fileoff, filesize) = linkedit?;
    Some(Linkedit {
        fileoff,
        vmaddr,
        filesize,
        image_vmaddr: image_vmaddr?,
    })
}

/// Reads only the 32-byte header first, then exactly its bounded load-command area,
/// and finally the validated mapped trie extent.
unsafe fn image_trie(header: *const u8) -> Option<&'static [u8]> {
    if header.is_null() {
        return None;
    }
    // The dyld API guarantees a live mapped Mach-O header at this address.
    let fixed_header = unsafe { std::slice::from_raw_parts(header, 32) };
    if u32_at(fixed_header, 0)? != MH_MAGIC_64 {
        return None;
    }
    let ncmds = usize::try_from(u32_at(fixed_header, 16)?).ok()?;
    let sizeofcmds = usize::try_from(u32_at(fixed_header, 20)?).ok()?;
    if sizeofcmds > MAX_LOAD_COMMAND_BYTES {
        return None;
    }
    let commands_address = (header as usize).checked_add(32)?;
    commands_address.checked_add(sizeofcmds)?;
    let commands_ptr = header.wrapping_add(32);
    let commands = unsafe { std::slice::from_raw_parts(commands_ptr, sizeofcmds) };
    let linkedit = linkedit_range(commands, ncmds)?;
    let (trie_offset, trie_size) = export_trie_range(commands, ncmds)?;
    let trie_end = trie_offset.checked_add(trie_size)?;
    let linkedit_end = linkedit.fileoff.checked_add(linkedit.filesize)?;
    if trie_size == 0
        || trie_size > MAX_TRIE_BYTES as u64
        || trie_offset < linkedit.fileoff
        || trie_end > linkedit_end
    {
        return None;
    }
    let linkedit_delta = linkedit.vmaddr.checked_sub(linkedit.image_vmaddr)?;
    let trie_delta = trie_offset.checked_sub(linkedit.fileoff)?;
    let mapped_offset = linkedit_delta.checked_add(trie_delta)?;
    let mapped_offset = usize::try_from(mapped_offset).ok()?;
    let trie_size = usize::try_from(trie_size).ok()?;
    let trie_address = (header as usize).checked_add(mapped_offset)?;
    trie_address.checked_add(trie_size)?;
    let trie_ptr = header.wrapping_add(mapped_offset);
    let trie = unsafe { std::slice::from_raw_parts(trie_ptr, trie_size) };
    Some(trie)
}

/// Only resolve from libsystem_info, the system library that owns macOS NSS.
fn is_nss_image(path: &CStr) -> bool {
    NSS_IMAGE_PATHS
        .iter()
        .any(|candidate| path.to_bytes() == *candidate)
}

unsafe fn find_export(symbol: Symbol) -> Option<*mut c_void> {
    for index in 0..unsafe { dyld::_dyld_image_count() } {
        let image_name = unsafe { dyld::_dyld_get_image_name(index) };
        if image_name.is_null() || !is_nss_image(unsafe { CStr::from_ptr(image_name) }) {
            continue;
        }
        let header = unsafe { dyld::_dyld_get_image_header(index) };
        let Some(trie) = (unsafe { image_trie(header) }) else {
            continue;
        };
        let mach_name = format!("_{}", symbol.name());
        let Some(address) =
            trie_lookup(trie, &mach_name).or_else(|| trie_lookup(trie, symbol.name()))
        else {
            continue;
        };
        let address = match address {
            ExportAddress::Absolute(address) => usize::try_from(address).ok()?,
            ExportAddress::Regular(address) => {
                (header as usize).checked_add(usize::try_from(address).ok()?)?
            }
        };
        return Some(address as *mut c_void);
    }
    None
}

static GETPWUID_R: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static GETPWUID: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn cache(symbol: Symbol) -> &'static AtomicPtr<c_void> {
    match symbol {
        Symbol::GetpwuidR => &GETPWUID_R,
        Symbol::Getpwuid => &GETPWUID,
    }
}

/// Resolves `symbol` once and caches it.
pub fn resolve(symbol: Symbol) -> *mut c_void {
    let slot = cache(symbol);
    let cached = slot.load(Ordering::Acquire);
    if !cached.is_null() {
        return cached;
    }
    let found = unsafe { find_export(symbol) }.unwrap_or(std::ptr::null_mut());
    if !found.is_null() {
        slot.store(found, Ordering::Release);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    fn single_export(flags: u8, address: u8) -> Vec<u8> {
        // The leaf for edge "x" contains flags and address. Small values keep
        // both ULEBs to one byte.
        vec![0, 1, b'x', 0, 5, 2, flags, address, 0]
    }

    #[test]
    fn follows_compressed_edges_and_reads_terminal_address() {
        // Root has edge "getpw", pointing to a node whose edge "uid_r" has
        // a regular export terminal at address 0x34.
        let trie = vec![
            0, 1, b'g', b'e', b't', b'p', b'w', 0, 9, 0, 1, b'u', b'i', b'd', b'_', b'r', 0, 18, 2,
            0, 0x34, 0,
        ];
        assert_eq!(
            trie_lookup(&trie, "getpwuid_r"),
            Some(ExportAddress::Regular(0x34))
        );
        assert_eq!(trie_lookup(&trie, "getpwuid"), None);
    }

    #[test]
    fn supports_absolute_exports_and_rejects_reexports_and_resolvers() {
        assert_eq!(
            trie_lookup(&single_export(2, 0x34), "x"),
            Some(ExportAddress::Absolute(0x34))
        );
        assert_eq!(trie_lookup(&single_export(8, 0x34), "x"), None);
        assert_eq!(trie_lookup(&single_export(0x10, 0x34), "x"), None);
    }

    #[test]
    fn rejects_truncated_overflowing_and_cyclic_tries() {
        assert_eq!(trie_lookup(&[0x80], "x"), None);
        assert_eq!(trie_lookup(&[0, 1, b'x'], "x"), None);
        // The root points to itself, which must terminate at the step limit.
        assert_eq!(
            trie_lookup(&[0, 1, b'x', 0, 0], &"x".repeat(MAX_TRIE_STEPS + 1)),
            None
        );
        assert_eq!(trie_lookup(&[0x80; 10], "x"), None);
    }

    #[test]
    fn reads_export_fields_from_dyld_info_plus_40() {
        let mut command = vec![0u8; 48];
        command[0..4].copy_from_slice(&LC_DYLD_INFO.to_le_bytes());
        command[4..8].copy_from_slice(&48u32.to_le_bytes());
        command[40..44].copy_from_slice(&0x1234u32.to_le_bytes());
        command[44..48].copy_from_slice(&0x56u32.to_le_bytes());
        assert_eq!(export_trie_range(&command, 1), Some((0x1234, 0x56)));
    }

    #[test]
    fn the_real_getpwuid_r_is_found_and_returns_the_home() {
        let address = resolve(Symbol::GetpwuidR);
        assert!(!address.is_null(), "libsystem_info exports getpwuid_r");
        type GetpwuidR = unsafe extern "C" fn(
            libc::uid_t,
            *mut libc::passwd,
            *mut libc::c_char,
            libc::size_t,
            *mut *mut libc::passwd,
        ) -> libc::c_int;
        let getpwuid_r: GetpwuidR = unsafe { std::mem::transmute(address) };
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buffer = vec![0u8; 4096];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let code = unsafe {
            getpwuid_r(
                libc::getuid(),
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        assert_eq!(code, 0);
        assert!(!result.is_null());
        let home = unsafe { CStr::from_ptr(entry.pw_dir) }.to_string_lossy();
        assert!(home.starts_with('/'), "resolved home is absolute: {home}");
        assert_eq!(home, std::env::var("HOME").unwrap_or_default());
    }

    #[test]
    fn the_real_getpwuid_is_found_and_returns_the_home() {
        let address = resolve(Symbol::Getpwuid);
        assert!(!address.is_null(), "libsystem_info exports getpwuid");
        type Getpwuid = unsafe extern "C" fn(libc::uid_t) -> *mut libc::passwd;
        let getpwuid: Getpwuid = unsafe { std::mem::transmute(address) };
        let result = unsafe { getpwuid(libc::getuid()) };
        assert!(!result.is_null());
        let home = unsafe { CStr::from_ptr((*result).pw_dir) }.to_string_lossy();
        assert_eq!(home, std::env::var("HOME").unwrap_or_default());
    }

    #[test]
    fn the_dyld_snapshot_contains_the_running_executable() {
        let images = mapped_images();
        assert!(!images.is_empty(), "dyld reports at least one image");
        assert!(images.iter().all(|path| path.is_absolute()));
        let executable = std::env::current_exe().expect("test executable path");
        assert!(
            images
                .iter()
                .any(|path| path.file_name() == executable.file_name()),
            "the snapshot must include the running test binary"
        );
    }
}
