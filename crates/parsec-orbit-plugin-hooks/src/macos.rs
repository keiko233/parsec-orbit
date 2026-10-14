//! Bounded arm64 macOS hooks using classic Mach-O imported-pointer sections.
//!
//! This backend never writes executable memory. It accepts only imported
//! pointer slots whose current value is the requested function address.
//! Images using chained fixups or malformed/unsupported pointer metadata are
//! left untouched.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once, OnceLock};

use parsec_orbit_plugin_sdk_std::prelude::*;
use parsec_orbit_plugin_sdk_std::services::Hooks;
use parsec_orbit_plugin_sdk_std::{
    ParsecOrbitHooksV1, ParsecOrbitInterfaceHeader, ERR_EXCEPTION, ERR_EXISTS, ERR_INVALID,
    ERR_NOT_FOUND, ERR_NOT_READY, OK,
};

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_EXECUTE: u32 = 0x2;
const CPU_TYPE_ARM64: i32 = 0x0100_000c;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_LOAD_DYLIB: u32 = 0xc;
const LC_ID_DYLIB: u32 = 0xd;
const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
const LC_RPATH: u32 = 0x8000_001c;
const LC_SEGMENT_64: u32 = 0x19;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_MAIN: u32 = 0x8000_0028;
const LC_REEXPORT_DYLIB: u32 = 0x8000_001f;
const LC_LAZY_LOAD_DYLIB: u32 = 0x20;
const LC_LOAD_UPWARD_DYLIB: u32 = 0x8000_0023;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x8000_0033;
const SECTION_TYPE_MASK: u32 = 0xff;
const S_NON_LAZY_SYMBOL_POINTERS: u32 = 0x6;
const S_LAZY_SYMBOL_POINTERS: u32 = 0x7;
const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;
const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_UNDF: u8 = 0x00;
const VM_PROT_EXECUTE: c_int = 4;
const VM_REGION_BASIC_INFO_64: c_int = 9;
const VM_PROT_READ: c_int = 1;
const VM_PROT_WRITE: c_int = 2;
const RTLD_LAZY: c_int = 1;
const RTLD_NOLOAD: c_int = 0x10;
const MAX_LOAD_COMMANDS: u32 = 16_384;
const MAX_LOAD_COMMAND_BYTES: usize = 16 * 1024 * 1024;
const MAX_LINKEDIT_TABLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_POINTER_SLOTS_PER_IMAGE: usize = 16_384;
const MAX_IMPORT_SYMBOL_BYTES: usize = 512;
const MAX_LIBRARY_NAME_BYTES: usize = 512;
const MAX_IMAGE_PATH_BYTES: usize = 4096;
const MAX_HOOK_IMPORT_KEYS: usize = 256;
const MAX_ACTIVE_HOOKS: usize = 256;
const MAX_HOOK_SLOTS: usize = 65_536;
const MAX_PINNED_IMAGES: usize = 4096;

type MachPort = u32;
type KernReturn = c_int;

#[repr(C)]
#[derive(Clone, Copy)]
struct MachHeader64 {
    magic: u32,
    cpu_type: i32,
    _cpu_subtype: i32,
    _file_type: u32,
    command_count: u32,
    command_bytes: u32,
    _flags: u32,
    _reserved: u32,
}

#[repr(C)]
struct DlInfo {
    file_name: *const c_char,
    file_base: *mut c_void,
    symbol_name: *const c_char,
    symbol_address: *mut c_void,
}

#[derive(Clone, Copy)]
struct Segment {
    vm_address: u64,
    vm_size: u64,
    file_offset: u64,
    file_size: u64,
}

#[derive(Clone, Copy)]
struct PointerSection {
    vm_address: u64,
    byte_size: u64,
    indirect_index: u32,
    containing_segment_address: u64,
    containing_segment_size: u64,
}

#[derive(Clone, Copy)]
struct Symtab {
    symbol_offset: u32,
    symbol_count: u32,
    string_offset: u32,
    string_size: u32,
}

#[derive(Clone, Copy)]
struct Dysymtab {
    indirect_offset: u32,
    indirect_count: u32,
}

#[derive(Clone, Copy)]
struct Linkedit {
    segment: Segment,
}

#[derive(Default)]
struct Registry {
    next_id: i64,
    hooks: HashMap<i64, Hook>,
    images: HashMap<usize, ImageInfo>,
    image_paths: HashMap<usize, String>,
}

struct Hook {
    target: usize,
    detour: usize,
    enabled: bool,
    degraded: bool,
    imports: HashSet<ImportKey>,
    // Values are saved while the hook is enabled. For an already-bound
    // import the saved value is target; a lazy resolver value is preserved
    // too if dyld has not yet resolved the slot.
    slots: HashMap<usize, usize>,
}

enum RebindFailure {
    Write(usize),
    Limit,
}

enum ImageInfo {
    Supported(ParsedImage),
    Unsupported,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ImportKey {
    symbol: String,
    library: Option<String>,
}

#[derive(Clone)]
struct ImportSlot {
    address: usize,
    key: ImportKey,
}

#[derive(Clone)]
struct ParsedImage {
    imports: Vec<ImportSlot>,
    mapped_ranges: Vec<(usize, usize)>,
    is_main_executable: bool,
}

#[derive(Clone)]
struct ImageSnapshot {
    path: String,
    mapped_ranges: Vec<(usize, usize)>,
    is_main_executable: bool,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static PINNED_IMAGES: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
static REGISTER_DYLD_CALLBACKS: Once = Once::new();

#[link(name = "System")]
unsafe extern "C" {
    static mach_task_self_: MachPort;

    fn mach_vm_read_overwrite(
        target_task: MachPort,
        address: u64,
        size: u64,
        data: u64,
        out_size: *mut u64,
    ) -> KernReturn;
    fn vm_region_64(
        target_task: MachPort,
        address: *mut usize,
        size: *mut usize,
        flavor: c_int,
        info: *mut c_int,
        info_count: *mut u32,
        object_name: *mut MachPort,
    ) -> KernReturn;
    fn vm_protect(
        target_task: MachPort,
        address: usize,
        size: usize,
        set_maximum: c_int,
        new_protection: c_int,
    ) -> KernReturn;
    fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
    fn getpagesize() -> c_int;

    fn _dyld_register_func_for_add_image(
        callback: unsafe extern "C" fn(*const MachHeader64, isize),
    );
    fn _dyld_register_func_for_remove_image(
        callback: unsafe extern "C" fn(*const MachHeader64, isize),
    );

    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
}

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Register dyld callbacks without holding the registry lock. dyld invokes
/// the add callback synchronously for existing images and while its loader
/// lock is held for later images. Callbacks never call back into dyld, and
/// service operations never call dyld while holding the registry lock.
fn ensure_dyld_callbacks() {
    let _ = REGISTRY.get_or_init(|| Mutex::new(Registry::default()));
    REGISTER_DYLD_CALLBACKS.call_once(|| unsafe {
        _dyld_register_func_for_add_image(image_added);
        _dyld_register_func_for_remove_image(image_removed);
    });
}

fn read_process(address: u64, length: usize) -> Option<Vec<u8>> {
    if length > MAX_LINKEDIT_TABLE_BYTES {
        return None;
    }
    if length == 0 {
        return Some(Vec::new());
    }
    let mut bytes = vec![0_u8; length];
    let mut copied = 0_u64;
    // SAFETY: mach_vm_read_overwrite copies from this process into the owned
    // buffer and reports whether the entire requested range was readable.
    let result = unsafe {
        mach_vm_read_overwrite(
            mach_task_self_,
            address,
            length as u64,
            bytes.as_mut_ptr() as usize as u64,
            &mut copied,
        )
    };
    (result == 0 && copied == length as u64).then_some(bytes)
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let raw: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(raw))
}

fn add_slide(address: u64, slide: isize) -> Option<u64> {
    if slide >= 0 {
        address.checked_add(slide as u64)
    } else {
        address.checked_sub(slide.unsigned_abs() as u64)
    }
}

fn checked_range_end(start: u64, length: u64) -> Option<u64> {
    start.checked_add(length)
}

fn mapped_linkedit_bytes(
    linkedit: Linkedit,
    slide: isize,
    file_offset: u32,
    byte_size: usize,
) -> Option<Vec<u8>> {
    if byte_size > MAX_LINKEDIT_TABLE_BYTES {
        return None;
    }
    let relative = u64::from(file_offset).checked_sub(linkedit.segment.file_offset)?;
    let end_in_file = relative.checked_add(byte_size as u64)?;
    if end_in_file > linkedit.segment.file_size || end_in_file > linkedit.segment.vm_size {
        return None;
    }
    let address = add_slide(linkedit.segment.vm_address.checked_add(relative)?, slide)?;
    read_process(address, byte_size)
}

fn load_command_string(
    commands: &[u8],
    cursor: usize,
    command_size: usize,
    minimum_offset: usize,
) -> Result<String, ()> {
    let name_offset = read_u32(commands, cursor.checked_add(8).ok_or(())?).ok_or(())? as usize;
    if name_offset < minimum_offset || name_offset >= command_size {
        return Err(());
    }
    let start = cursor.checked_add(name_offset).ok_or(())?;
    let end = cursor.checked_add(command_size).ok_or(())?;
    let bytes = commands.get(start..end).ok_or(())?;
    let nul = bytes.iter().position(|byte| *byte == 0).ok_or(())?;
    if nul > MAX_LIBRARY_NAME_BYTES {
        return Err(());
    }
    let text = std::str::from_utf8(bytes.get(..nul).ok_or(())?).map_err(|_| ())?;
    if text.is_empty() {
        return Err(());
    }
    Ok(text.to_owned())
}

fn parse_image(header: *const MachHeader64, slide: isize) -> Result<ParsedImage, ()> {
    let header_address = header as usize as u64;
    let header_bytes = read_process(header_address, size_of::<MachHeader64>()).ok_or(())?;
    let magic = read_u32(&header_bytes, 0).ok_or(())?;
    let cpu_type = read_u32(&header_bytes, 4).ok_or(())? as i32;
    let file_type = read_u32(&header_bytes, 12).ok_or(())?;
    let command_count = read_u32(&header_bytes, 16).ok_or(())?;
    let command_bytes = read_u32(&header_bytes, 20).ok_or(())? as usize;
    if magic != MH_MAGIC_64
        || cpu_type != CPU_TYPE_ARM64
        || command_count > MAX_LOAD_COMMANDS
        || command_bytes > MAX_LOAD_COMMAND_BYTES
    {
        return Err(());
    }
    let command_address = header_address
        .checked_add(size_of::<MachHeader64>() as u64)
        .ok_or(())?;
    let commands = if command_bytes == 0 {
        Vec::new()
    } else {
        read_process(command_address, command_bytes).ok_or(())?
    };

    let mut pointer_sections = Vec::new();
    let mut symtab = None;
    let mut dysymtab = None;
    let mut linkedit = None;
    let mut dependencies = Vec::new();
    let mut has_install_name = false;
    let mut mapped_ranges = Vec::new();
    let mut has_chained_fixups = false;
    let mut cursor = 0_usize;

    for _ in 0..command_count {
        let command = read_u32(&commands, cursor).ok_or(())?;
        let command_size =
            read_u32(&commands, cursor.checked_add(4).ok_or(())?).ok_or(())? as usize;
        let command_end = cursor.checked_add(command_size).ok_or(())?;
        if command_size < 8 || !command_size.is_multiple_of(4) || command_end > commands.len() {
            return Err(());
        }

        match command {
            LC_SEGMENT_64 => {
                if command_size < 72 {
                    return Err(());
                }
                let vm_address = read_u64(&commands, cursor + 24).ok_or(())?;
                let vm_size = read_u64(&commands, cursor + 32).ok_or(())?;
                let file_offset = read_u64(&commands, cursor + 40).ok_or(())?;
                let file_size = read_u64(&commands, cursor + 48).ok_or(())?;
                let section_count = read_u32(&commands, cursor + 64).ok_or(())? as usize;
                let expected_size = 72_usize
                    .checked_add(section_count.checked_mul(80).ok_or(())?)
                    .ok_or(())?;
                if expected_size != command_size
                    || checked_range_end(vm_address, vm_size).is_none()
                    || checked_range_end(file_offset, file_size).is_none()
                {
                    return Err(());
                }
                let segment = Segment {
                    vm_address,
                    vm_size,
                    file_offset,
                    file_size,
                };
                if vm_size != 0 {
                    let runtime_start = add_slide(vm_address, slide).ok_or(())?;
                    let runtime_end = runtime_start.checked_add(vm_size).ok_or(())?;
                    mapped_ranges.push((
                        usize::try_from(runtime_start).map_err(|_| ())?,
                        usize::try_from(runtime_end).map_err(|_| ())?,
                    ));
                }
                let mut section_offset = cursor + 72;
                for _ in 0..section_count {
                    let section_address = read_u64(&commands, section_offset + 32).ok_or(())?;
                    let section_size = read_u64(&commands, section_offset + 40).ok_or(())?;
                    let section_flags = read_u32(&commands, section_offset + 64).ok_or(())?;
                    let section_type = section_flags & SECTION_TYPE_MASK;
                    if section_type == S_NON_LAZY_SYMBOL_POINTERS
                        || section_type == S_LAZY_SYMBOL_POINTERS
                    {
                        let section_end =
                            checked_range_end(section_address, section_size).ok_or(())?;
                        let segment_end = checked_range_end(vm_address, vm_size).ok_or(())?;
                        if section_address < vm_address || section_end > segment_end {
                            return Err(());
                        }
                        pointer_sections.push(PointerSection {
                            vm_address: section_address,
                            byte_size: section_size,
                            indirect_index: read_u32(&commands, section_offset + 68).ok_or(())?,
                            containing_segment_address: vm_address,
                            containing_segment_size: vm_size,
                        });
                    }
                    section_offset += 80;
                }
                if commands.get(cursor + 8..cursor + 24).is_some_and(|name| {
                    name.get(..10) == Some(&b"__LINKEDIT"[..])
                        && name[10..].iter().all(|byte| *byte == 0)
                }) {
                    if linkedit.is_some() {
                        return Err(());
                    }
                    linkedit = Some(Linkedit { segment });
                }
            }
            LC_SYMTAB => {
                if command_size != 24 || symtab.is_some() {
                    return Err(());
                }
                symtab = Some(Symtab {
                    symbol_offset: read_u32(&commands, cursor + 8).ok_or(())?,
                    symbol_count: read_u32(&commands, cursor + 12).ok_or(())?,
                    string_offset: read_u32(&commands, cursor + 16).ok_or(())?,
                    string_size: read_u32(&commands, cursor + 20).ok_or(())?,
                });
            }
            LC_DYSYMTAB => {
                if command_size != 80 || dysymtab.is_some() {
                    return Err(());
                }
                dysymtab = Some(Dysymtab {
                    indirect_offset: read_u32(&commands, cursor + 56).ok_or(())?,
                    indirect_count: read_u32(&commands, cursor + 60).ok_or(())?,
                });
            }
            LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LAZY_LOAD_DYLIB
            | LC_LOAD_UPWARD_DYLIB => {
                dependencies.push(load_command_string(&commands, cursor, command_size, 24)?);
            }
            LC_ID_DYLIB => {
                if has_install_name {
                    return Err(());
                }
                let _ = load_command_string(&commands, cursor, command_size, 24)?;
                has_install_name = true;
            }
            LC_RPATH => {
                let _ = load_command_string(&commands, cursor, command_size, 12)?;
            }
            LC_DYLD_CHAINED_FIXUPS => has_chained_fixups = true,
            LC_DYLD_INFO_ONLY | LC_MAIN | LC_DYLD_EXPORTS_TRIE => {}
            _ if command & 0x8000_0000 != 0 => {
                return Err(());
            }
            _ => {}
        }
        cursor = command_end;
    }
    if cursor != commands.len() {
        return Err(());
    }
    if has_chained_fixups {
        return Err(());
    }
    if pointer_sections.is_empty() {
        return Ok(ParsedImage {
            imports: Vec::new(),
            mapped_ranges,
            is_main_executable: file_type == MH_EXECUTE,
        });
    }

    let symtab = symtab.ok_or(())?;
    let dysymtab = dysymtab.ok_or(())?;
    let linkedit = linkedit.ok_or(())?;
    let symbol_bytes_len = (symtab.symbol_count as usize)
        .checked_mul(16)
        .filter(|length| *length <= MAX_LINKEDIT_TABLE_BYTES)
        .ok_or(())?;
    let indirect_bytes_len = (dysymtab.indirect_count as usize)
        .checked_mul(4)
        .filter(|length| *length <= MAX_LINKEDIT_TABLE_BYTES)
        .ok_or(())?;
    let symbols =
        mapped_linkedit_bytes(linkedit, slide, symtab.symbol_offset, symbol_bytes_len).ok_or(())?;
    let indirect_symbols = mapped_linkedit_bytes(
        linkedit,
        slide,
        dysymtab.indirect_offset,
        indirect_bytes_len,
    )
    .ok_or(())?;
    let strings = mapped_linkedit_bytes(
        linkedit,
        slide,
        symtab.string_offset,
        symtab.string_size as usize,
    )
    .ok_or(())?;

    let mut imports = Vec::new();
    let mut total_pointer_slots = 0_usize;
    for section in pointer_sections {
        if section.byte_size % size_of::<usize>() as u64 != 0 {
            return Err(());
        }
        let count =
            usize::try_from(section.byte_size / size_of::<usize>() as u64).map_err(|_| ())?;
        total_pointer_slots = total_pointer_slots.checked_add(count).ok_or(())?;
        if count > MAX_POINTER_SLOTS_PER_IMAGE
            || total_pointer_slots > MAX_POINTER_SLOTS_PER_IMAGE
            || (section.indirect_index as usize)
                .checked_add(count)
                .is_none_or(|end| end > dysymtab.indirect_count as usize)
        {
            return Err(());
        }
        let segment_end = section
            .containing_segment_address
            .checked_add(section.containing_segment_size)
            .ok_or(())?;
        let section_end = section
            .vm_address
            .checked_add(section.byte_size)
            .ok_or(())?;
        if section.vm_address < section.containing_segment_address || section_end > segment_end {
            return Err(());
        }
        let runtime_address = add_slide(section.vm_address, slide).ok_or(())?;
        runtime_address
            .checked_add(section.byte_size)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(())?;
        for index in 0..count {
            let indirect_offset = (section.indirect_index as usize + index)
                .checked_mul(4)
                .ok_or(())?;
            let indirect_symbol = read_u32(&indirect_symbols, indirect_offset).ok_or(())?;
            if indirect_symbol & (INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS) != 0 {
                continue;
            }
            let symbol_index = indirect_symbol as usize;
            if symbol_index >= symtab.symbol_count as usize {
                return Err(());
            }
            let symbol_offset = symbol_index.checked_mul(16).ok_or(())?;
            let string_index = read_u32(&symbols, symbol_offset).ok_or(())? as usize;
            let symbol_type = *symbols.get(symbol_offset + 4).ok_or(())?;
            if symbol_type & N_STAB != 0 || symbol_type & N_TYPE != N_UNDF {
                continue;
            }
            if string_index >= strings.len() || !strings[string_index..].contains(&0) {
                return Err(());
            }
            let symbol_bytes = &strings[string_index..];
            let symbol_end = symbol_bytes.iter().position(|byte| *byte == 0).ok_or(())?;
            if symbol_end > MAX_IMPORT_SYMBOL_BYTES {
                return Err(());
            }
            let raw_symbol = std::str::from_utf8(&symbol_bytes[..symbol_end]).map_err(|_| ())?;
            if raw_symbol.is_empty() {
                continue;
            }
            let symbol = raw_symbol.strip_prefix('_').unwrap_or(raw_symbol);
            if symbol.is_empty() {
                return Err(());
            }
            let library_ordinal =
                (u16::from_le_bytes([symbols[symbol_offset + 6], symbols[symbol_offset + 7]]) >> 8)
                    as usize;
            let library = if (1..=dependencies.len()).contains(&library_ordinal) {
                Some(dependencies[library_ordinal - 1].clone())
            } else if library_ordinal == 0 || library_ordinal >= 0xfe {
                None
            } else {
                return Err(());
            };
            let slot = runtime_address
                .checked_add((index * size_of::<usize>()) as u64)
                .ok_or(())?;
            if slot % size_of::<usize>() as u64 != 0 {
                return Err(());
            }
            imports.push(ImportSlot {
                address: usize::try_from(slot).map_err(|_| ())?,
                key: ImportKey {
                    symbol: symbol.to_owned(),
                    library,
                },
            });
        }
    }
    imports.sort_unstable_by_key(|import| import.address);
    imports.dedup_by_key(|import| import.address);
    Ok(ParsedImage {
        imports,
        mapped_ranges,
        is_main_executable: file_type == MH_EXECUTE,
    })
}

fn read_pointer(slot: usize) -> Option<usize> {
    let bytes = read_process(slot as u64, size_of::<usize>())?;
    let raw: [u8; size_of::<usize>()] = bytes.try_into().ok()?;
    Some(usize::from_ne_bytes(raw))
}

fn region_info(address: usize) -> Option<(usize, usize, c_int)> {
    let mut region_address = address;
    let mut region_size = 0_usize;
    let mut info = [0 as c_int; 16];
    let mut info_count = info.len() as u32;
    let mut object_name = 0 as MachPort;
    // SAFETY: all output pointers refer to live stack storage and the task is
    // this process. Mach returns the containing VM region and protections.
    let result = unsafe {
        vm_region_64(
            mach_task_self_,
            &mut region_address,
            &mut region_size,
            VM_REGION_BASIC_INFO_64,
            info.as_mut_ptr(),
            &mut info_count,
            &mut object_name,
        )
    };
    if object_name != 0 {
        // SAFETY: vm_region_64 returns a send right owned by this task.
        unsafe {
            mach_port_deallocate(mach_task_self_, object_name);
        }
    }
    (result == 0 && info_count >= 1 && region_size > 0).then_some((
        region_address,
        region_size,
        info[0],
    ))
}

fn is_executable_address(address: usize) -> bool {
    region_info(address).is_some_and(|(base, size, protection)| {
        base.checked_add(size)
            .is_some_and(|end| address >= base && address < end)
            && protection & VM_PROT_EXECUTE != 0
    })
}

/// Atomically changes an imported pointer only if it still has the value we
/// observed. Read-only data pages are made writable for this pointer-sized
/// update and immediately returned to their exact original protection.
fn compare_exchange_pointer(slot: usize, expected: usize, replacement: usize) -> Result<bool, ()> {
    let page_size = unsafe { getpagesize() };
    if page_size <= 0 || !(page_size as usize).is_power_of_two() {
        return Err(());
    }
    let page_size = page_size as usize;
    let page_start = slot & !(page_size - 1);
    let page_end = page_start.checked_add(page_size).ok_or(())?;
    let (region_start, region_size, original_protection) = region_info(slot).ok_or(())?;
    let region_end = region_start.checked_add(region_size).ok_or(())?;
    if page_start < region_start
        || page_end > region_end
        || original_protection & VM_PROT_READ == 0
        || original_protection & VM_PROT_EXECUTE != 0
    {
        return Err(());
    }

    let changed_protection = original_protection & VM_PROT_WRITE == 0;
    if changed_protection {
        // SAFETY: page bounds and original protection were queried above;
        // set_maximum is false, so the mapping's maximum protection is kept.
        let result = unsafe {
            vm_protect(
                mach_task_self_,
                page_start,
                page_size,
                0,
                original_protection | VM_PROT_WRITE,
            )
        };
        if result != 0 {
            return Err(());
        }
    }

    // SAFETY: the slot came from a loaded image's pointer section, is aligned,
    // readable, and its containing data page is writable for this operation.
    let cell = unsafe { &*(slot as *const AtomicUsize) };
    let write_result =
        cell.compare_exchange(expected, replacement, Ordering::SeqCst, Ordering::SeqCst);
    if changed_protection {
        // SAFETY: restore precisely the protection returned by vm_region_64.
        let restore_result = unsafe {
            vm_protect(
                mach_task_self_,
                page_start,
                page_size,
                0,
                original_protection,
            )
        };
        if restore_result != 0 {
            if write_result.is_ok() {
                let _ = cell.compare_exchange(
                    replacement,
                    expected,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            }
            // Retry after the best-effort pointer rollback.
            let _ = unsafe {
                vm_protect(
                    mach_task_self_,
                    page_start,
                    page_size,
                    0,
                    original_protection,
                )
            };
            return Err(());
        }
    }
    match write_result {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

fn original_slot_value(hook: &Hook, import: &ImportSlot) -> Option<usize> {
    let current = read_pointer(import.address)?;
    (current == hook.target && hook.imports.contains(&import.key)).then_some(current)
}

fn rebind_import(hook: &Hook, import: &ImportSlot) -> Result<Option<usize>, RebindFailure> {
    if !hook.slots.contains_key(&import.address) && hook.slots.len() >= MAX_HOOK_SLOTS {
        return Err(RebindFailure::Limit);
    }
    for _ in 0..3 {
        let Some(old_value) = original_slot_value(hook, import) else {
            return Ok(None);
        };
        match compare_exchange_pointer(import.address, old_value, hook.detour) {
            Ok(true) => return Ok(Some(old_value)),
            Ok(false) => continue,
            Err(()) => return Err(RebindFailure::Write(old_value)),
        }
    }
    Ok(None)
}

fn apply_to_new_image(hook: &mut Hook, image: &ParsedImage) -> bool {
    if !hook.enabled {
        return true;
    }
    for import in &image.imports {
        match rebind_import(hook, import) {
            Ok(Some(old_value)) => {
                hook.slots.insert(import.address, old_value);
            }
            Ok(None) => {}
            Err(RebindFailure::Write(old_value)) => {
                hook.slots.insert(import.address, old_value);
                return false;
            }
            Err(RebindFailure::Limit) => return false,
        }
    }
    true
}

fn restore_hook_slots(hook: &mut Hook) -> bool {
    let slots: Vec<(usize, usize)> = hook.slots.iter().map(|(&slot, &old)| (slot, old)).collect();
    let mut all_restored = true;
    for (slot, old_value) in slots {
        match compare_exchange_pointer(slot, hook.detour, old_value) {
            Ok(_) => {
                hook.slots.remove(&slot);
            }
            Err(()) => all_restored = false,
        }
    }
    if all_restored {
        hook.enabled = false;
        hook.degraded = false;
        hook.slots.clear();
    }
    all_restored
}

unsafe extern "C" fn image_added(header: *const MachHeader64, slide: isize) {
    if header.is_null() {
        return;
    }
    let key = header as usize;
    // Copy the callback image's path before taking the registry mutex.
    // Resolver code snapshots these owned strings and calls dlopen only after
    // releasing that mutex.
    let path = image_path_from_dyld(header);
    let parsed = parse_image(header, slide);
    let mut state = registry();
    let image = match parsed {
        Ok(image) => ImageInfo::Supported(image),
        Err(()) => ImageInfo::Unsupported,
    };
    let parsed_image = match &image {
        ImageInfo::Supported(image) => Some(image.clone()),
        ImageInfo::Unsupported => None,
    };
    state.images.insert(key, image);
    if let Some(path) = path {
        state.image_paths.insert(key, path);
    } else {
        state.image_paths.remove(&key);
    }
    if let Some(image) = parsed_image {
        for hook in state
            .hooks
            .values_mut()
            .filter(|hook| hook.enabled && !hook.degraded)
        {
            if !apply_to_new_image(hook, &image) && !restore_hook_slots(hook) {
                hook.degraded = true;
            }
        }
    }
}

unsafe extern "C" fn image_removed(header: *const MachHeader64, _slide: isize) {
    if header.is_null() {
        return;
    }
    let key = header as usize;
    let mut state = registry();
    state.image_paths.remove(&key);
    if let Some(ImageInfo::Supported(image)) = state.images.remove(&key) {
        for hook in state.hooks.values_mut() {
            for import in &image.imports {
                hook.slots.remove(&import.address);
            }
        }
    }
}

fn image_path_from_dyld(header: *const MachHeader64) -> Option<String> {
    let mut info = DlInfo {
        file_name: std::ptr::null(),
        file_base: std::ptr::null_mut(),
        symbol_name: std::ptr::null(),
        symbol_address: std::ptr::null_mut(),
    };
    if unsafe { dladdr(header.cast(), &mut info) } == 0 || info.file_name.is_null() {
        return None;
    }
    let bytes = unsafe { CStr::from_ptr(info.file_name) }.to_bytes();
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_PATH_BYTES {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn image_snapshots() -> Vec<ImageSnapshot> {
    let state = registry();
    state
        .image_paths
        .iter()
        .filter_map(|(&key, path)| {
            let ImageInfo::Supported(image) = state.images.get(&key)? else {
                return Some(ImageSnapshot {
                    path: path.clone(),
                    mapped_ranges: Vec::new(),
                    is_main_executable: false,
                });
            };
            Some(ImageSnapshot {
                path: path.clone(),
                mapped_ranges: image.mapped_ranges.clone(),
                is_main_executable: image.is_main_executable,
            })
        })
        .collect()
}

fn pinned_handle(path: &str) -> Option<*mut c_void> {
    let pinned = PINNED_IMAGES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(handle) = pinned
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .copied()
    {
        return Some(handle as *mut c_void);
    }
    if pinned
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
        >= MAX_PINNED_IMAGES
    {
        return None;
    }

    let path = CString::new(path).ok()?;
    let path_text = path.to_str().ok()?.to_owned();
    // Do not hold registry or pin locks across dlopen: it can run dyld
    // callbacks. RTLD_NOLOAD only acquires a reference to an existing image.
    let opened = unsafe { dlopen(path.as_ptr(), RTLD_LAZY | RTLD_NOLOAD) };
    if opened.is_null() {
        return None;
    }

    let mut pinned = pinned
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (result, keep_opened) = if let Some(handle) = pinned.get(&path_text).copied() {
        (Some(handle as *mut c_void), false)
    } else if pinned.len() < MAX_PINNED_IMAGES {
        pinned.insert(path_text, opened as usize);
        (Some(opened), true)
    } else {
        (None, false)
    };
    drop(pinned);
    if !keep_opened {
        unsafe {
            dlclose(opened);
        }
    }
    result
}

fn image_for_address(snapshots: &[ImageSnapshot], address: usize) -> Option<&ImageSnapshot> {
    let mut matches = snapshots.iter().filter(|image| {
        image
            .mapped_ranges
            .iter()
            .any(|&(start, end)| address >= start && address < end)
    });
    let image = matches.next()?;
    matches.next().is_none().then_some(image)
}

fn resolve_symbol(module: &str, symbol: &str) -> Option<*const c_void> {
    if symbol.is_empty() || symbol.len() > 4096 || module.len() > 4096 {
        return None;
    }
    ensure_dyld_callbacks();
    let symbol_c = CString::new(symbol).ok()?;
    let snapshots = image_snapshots();

    if module.is_empty() {
        // RTLD_DEFAULT is the reserved handle value -2 on Darwin. Resolve
        // first, then pin its uniquely identified owner and verify the address
        // through that permanent handle before returning it.
        let address = unsafe { dlsym((-2_isize) as *mut c_void, symbol_c.as_ptr()) };
        if address.is_null() {
            return None;
        }
        let owner = image_for_address(&snapshots, address as usize)?;
        if owner.is_main_executable {
            return Some(address.cast_const());
        }
        let handle = pinned_handle(&owner.path)?;
        let pinned_address = unsafe { dlsym(handle, symbol_c.as_ptr()) };
        return (pinned_address == address).then_some(address.cast_const());
    }

    let mut candidates: Vec<&ImageSnapshot> = snapshots
        .iter()
        .filter(|image| {
            module == image.path || module == image.path.rsplit('/').next().unwrap_or(&image.path)
        })
        .collect();
    candidates.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    for image in candidates {
        let address = if image.is_main_executable {
            let address = unsafe { dlsym((-2_isize) as *mut c_void, symbol_c.as_ptr()) };
            if address.is_null()
                || image_for_address(&snapshots, address as usize).map(|owner| owner.path.as_str())
                    != Some(image.path.as_str())
            {
                continue;
            }
            address
        } else {
            let Some(handle) = pinned_handle(&image.path) else {
                continue;
            };
            unsafe { dlsym(handle, symbol_c.as_ptr()) }
        };
        if address.is_null() {
            continue;
        }
        if let Some(owner) = image_for_address(&snapshots, address as usize) {
            if !owner.is_main_executable && pinned_handle(&owner.path).is_none() {
                continue;
            }
        }
        return Some(address.cast_const());
    }
    None
}

fn discover_imports(state: &Registry, target: usize) -> Option<HashSet<ImportKey>> {
    let mut imports = HashSet::new();
    let mut found_slot = false;
    for image in state.images.values() {
        let ImageInfo::Supported(image) = image else {
            continue;
        };
        for import in &image.imports {
            let current = read_pointer(import.address)?;
            if current == target {
                found_slot = true;
                imports.insert(import.key.clone());
            }
            if imports.len() > MAX_HOOK_IMPORT_KEYS {
                return None;
            }
        }
    }
    found_slot.then_some(imports)
}

unsafe fn utf8<'a>(pointer: *const u8, length: usize) -> Option<&'a str> {
    if pointer.is_null() || length > 4096 {
        return None;
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer, length) }).ok()
}

unsafe extern "system" fn resolve_service(
    module: *const u8,
    module_len: usize,
    symbol: *const u8,
    symbol_len: usize,
) -> *const c_void {
    let (Some(module), Some(symbol)) = (unsafe { utf8(module, module_len) }, unsafe {
        utf8(symbol, symbol_len)
    }) else {
        return std::ptr::null();
    };
    let Some(address) = resolve_symbol(module, symbol) else {
        return std::ptr::null();
    };
    address
}

unsafe extern "system" fn install_service(
    target: *const c_void,
    detour: *const c_void,
    original: *mut *const c_void,
) -> i64 {
    if target.is_null() || detour.is_null() || target == detour {
        return ERR_INVALID as i64;
    }
    let target_address = target as usize;
    let detour_address = detour as usize;
    if !is_executable_address(target_address) || !is_executable_address(detour_address) {
        return ERR_NOT_READY as i64;
    }

    ensure_dyld_callbacks();
    let mut state = registry();
    if state
        .hooks
        .values()
        .any(|hook| hook.target == target_address)
    {
        return ERR_EXISTS as i64;
    }
    if state.hooks.len() >= MAX_ACTIVE_HOOKS {
        return ERR_EXCEPTION as i64;
    }
    let Some(imports) = discover_imports(&state, target_address) else {
        return ERR_NOT_READY as i64;
    };
    let Some(id) = state.next_id.checked_add(1).filter(|id| *id > 0) else {
        return ERR_EXCEPTION as i64;
    };
    state.next_id = id;
    state.hooks.insert(
        id,
        Hook {
            target: target_address,
            detour: detour_address,
            enabled: false,
            degraded: false,
            imports,
            slots: HashMap::new(),
        },
    );
    if !original.is_null() {
        // The function body remains unchanged; the original address remains
        // callable after disable and remove.
        unsafe {
            *original = target;
        }
    }
    id
}

fn enable_hook(id: i64) -> i32 {
    let mut state = registry();
    let Some(hook) = state.hooks.get_mut(&id) else {
        return ERR_NOT_FOUND;
    };
    if hook.degraded {
        return ERR_EXCEPTION;
    }
    if hook.enabled {
        return OK;
    }

    let images: Vec<ParsedImage> = state
        .images
        .values()
        .filter_map(|image| match image {
            ImageInfo::Supported(image) => Some(image.clone()),
            ImageInfo::Unsupported => None,
        })
        .collect();
    let mut changed = Vec::new();
    {
        let hook = state.hooks.get_mut(&id).expect("hook remains registered");
        hook.enabled = true;
        for image in &images {
            for import in &image.imports {
                match rebind_import(hook, import) {
                    Ok(Some(old_value)) => {
                        changed.push((import.address, old_value));
                        hook.slots.insert(import.address, old_value);
                    }
                    Ok(None) => {}
                    Err(failure) => {
                        if let RebindFailure::Write(old_value) = failure {
                            changed.push((import.address, old_value));
                            hook.slots.insert(import.address, old_value);
                        }
                        let mut rollback_ok = true;
                        for (slot, old_value) in changed.iter().rev().copied() {
                            match compare_exchange_pointer(slot, hook.detour, old_value) {
                                Ok(_) => {
                                    hook.slots.remove(&slot);
                                }
                                Err(()) => rollback_ok = false,
                            }
                        }
                        if rollback_ok {
                            hook.enabled = false;
                            hook.slots.clear();
                            return ERR_EXCEPTION;
                        }
                        hook.degraded = true;
                        return ERR_EXCEPTION;
                    }
                }
            }
        }
        if changed.is_empty() {
            hook.enabled = false;
            return ERR_NOT_READY;
        }
    }
    OK
}

fn disable_hook(id: i64) -> i32 {
    let mut state = registry();
    let Some(hook) = state.hooks.get_mut(&id) else {
        return ERR_NOT_FOUND;
    };
    if !hook.enabled && hook.slots.is_empty() {
        hook.degraded = false;
        return OK;
    }
    if restore_hook_slots(hook) {
        OK
    } else {
        ERR_EXCEPTION
    }
}

fn remove_hook(id: i64) -> i32 {
    let mut state = registry();
    let Some(hook) = state.hooks.get_mut(&id) else {
        return ERR_NOT_FOUND;
    };
    if (hook.enabled || !hook.slots.is_empty()) && !restore_hook_slots(hook) {
        return ERR_EXCEPTION;
    }
    state.hooks.remove(&id);
    OK
}

unsafe extern "system" fn enable_service(id: i64) -> i32 {
    enable_hook(id)
}

unsafe extern "system" fn disable_service(id: i64) -> i32 {
    disable_hook(id)
}

unsafe extern "system" fn remove_service(id: i64) -> i32 {
    remove_hook(id)
}

/// The table registered under parsec-orbit.hooks.
pub static HOOKS: ParsecOrbitHooksV1 = ParsecOrbitHooksV1 {
    header: ParsecOrbitInterfaceHeader::new(size_of::<ParsecOrbitHooksV1>(), 1, 0),
    resolve: Some(resolve_service),
    install: Some(install_service),
    enable: Some(enable_service),
    disable: Some(disable_service),
    remove: Some(remove_service),
};

#[derive(Default)]
struct HooksPlugin;

impl Plugin for HooksPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        info!(
            cx,
            "hooks {} starting (macOS import rebinding)",
            env!("CARGO_PKG_VERSION")
        );
        cx.register::<Hooks>(&HOOKS)?;
        Ok(())
    }
}

parsec_orbit_plugin_sdk_std::declare_plugin! {
    id = "com.parsec-orbit.hooks",
    name = "Hooks",
    version = "0.1.1",
    author = "Parsec Orbit contributors",
    description = "Provides parsec-orbit.hooks: bounded imported-symbol rebinding on macOS",
    license = "MIT",
    plugin = HooksPlugin,
}

#[cfg(test)]
mod tests {
    use super::*;

    static IMPORT_SLOT: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn target_function() {}
    extern "C" fn unresolved_stub() {}
    extern "C" fn detour_function() {}

    #[test]
    fn executable_stub_address_is_not_accepted_as_the_original_target() {
        let target = target_function as *const () as usize;
        let stub = unresolved_stub as *const () as usize;
        assert_ne!(target, stub);
        assert!(is_executable_address(stub));

        let key = ImportKey {
            symbol: "target_function".to_owned(),
            library: Some("libtarget.dylib".to_owned()),
        };
        let hook = Hook {
            target,
            detour: detour_function as *const () as usize,
            enabled: true,
            degraded: false,
            imports: HashSet::from([key.clone()]),
            slots: HashMap::new(),
        };
        let import = ImportSlot {
            address: &IMPORT_SLOT as *const AtomicUsize as usize,
            key,
        };

        IMPORT_SLOT.store(stub, Ordering::SeqCst);
        assert_eq!(original_slot_value(&hook, &import), None);
        IMPORT_SLOT.store(target, Ordering::SeqCst);
        assert_eq!(original_slot_value(&hook, &import), Some(target));
    }
}
