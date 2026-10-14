//! Current-UID home substitution, with the original NSS result preserved.
//!
//! Callbacks forward unchanged until isolation is installed. The bootstrap
//! refuses an active vendor host until shared filesystem, Keychain and IPC
//! isolation passes; only the explicit test harness can install a home today.
//! Reentrant results keep every string in the caller's buffer. The legacy
//! result uses thread-local storage and expires on the next call on that thread.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::sync::OnceLock;

use crate::original::{resolve, Symbol};

static HOME: OnceLock<(libc::uid_t, CString)> = OnceLock::new();

/// Installed only after the complete isolation backend has been validated.
pub fn install(uid: libc::uid_t, home: CString) -> Result<(), String> {
    HOME.set((uid, home))
        .map_err(|_| "home already installed".into())
}

type Reentrant = unsafe extern "C" fn(
    libc::uid_t,
    *mut libc::passwd,
    *mut libc::c_char,
    usize,
    *mut *mut libc::passwd,
) -> libc::c_int;
type Legacy = unsafe extern "C" fn(libc::uid_t) -> *mut libc::passwd;

/// Appends the replacement after all strings returned by NSS, including the
/// original home. Fails rather than returning a pointer outside the buffer.
unsafe fn replace_home(
    entry: &mut libc::passwd,
    buffer: *mut libc::c_char,
    length: usize,
    home: &CStr,
) -> Result<(), i32> {
    let start = buffer as usize;
    let end = start.checked_add(length).ok_or(libc::ERANGE)?;
    let mut used = 0;
    for pointer in [
        entry.pw_name,
        entry.pw_passwd,
        entry.pw_class,
        entry.pw_gecos,
        entry.pw_dir,
        entry.pw_shell,
    ] {
        if pointer.is_null() {
            continue;
        }
        let address = pointer as usize;
        if address < start || address >= end {
            return Err(libc::EIO);
        }
        let offset = address - start;
        let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), length - offset);
        let count = bytes.iter().position(|byte| *byte == 0).ok_or(libc::EIO)? + 1;
        used = used.max(offset + count);
    }
    let replacement = home.to_bytes_with_nul();
    if replacement.len() > length - used {
        return Err(libc::ERANGE);
    }
    let destination = buffer.add(used);
    std::ptr::copy_nonoverlapping(replacement.as_ptr(), destination.cast(), replacement.len());
    entry.pw_dir = destination;
    Ok(())
}

/// Calls NSS first; UID misses, errors and other users retain NSS semantics.
pub unsafe extern "C" fn getpwuid_r(
    uid: libc::uid_t,
    entry: *mut libc::passwd,
    buffer: *mut libc::c_char,
    length: usize,
    result: *mut *mut libc::passwd,
) -> libc::c_int {
    // dyld excludes the image containing the interpose tuple itself. Calling
    // this image's import preserves inactive NSS semantics without lookup,
    // allocation, caching, or filesystem side effects from Orbit.
    if HOME.get().is_none() {
        return libc::getpwuid_r(uid, entry, buffer, length, result);
    }
    let address = resolve(Symbol::GetpwuidR);
    if address.is_null() {
        if !result.is_null() {
            *result = std::ptr::null_mut();
        }
        return libc::EIO;
    }
    let original: Reentrant = std::mem::transmute(address);
    let code = original(uid, entry, buffer, length, result);
    if code != 0 || result.is_null() || (*result).is_null() {
        return code;
    }
    if let Some((current, home)) = HOME.get() {
        if uid == *current {
            if let Err(code) = replace_home(&mut *entry, buffer, length, home) {
                *result = std::ptr::null_mut();
                return code;
            }
        }
    }
    code
}

struct OwnedEntry {
    entry: libc::passwd,
    strings: Vec<CString>,
}
thread_local! { static LEGACY: RefCell<Option<OwnedEntry>> = const { RefCell::new(None) }; }

/// Preserves original errno and every passwd field. No cross-thread shared
/// mutable passwd copy and no pointer into NSS's transient backing storage.
pub unsafe extern "C" fn getpwuid(uid: libc::uid_t) -> *mut libc::passwd {
    if HOME.get().is_none() {
        return libc::getpwuid(uid);
    }
    let address = resolve(Symbol::Getpwuid);
    if address.is_null() {
        *libc::__error() = libc::EIO;
        return std::ptr::null_mut();
    }
    let original: Legacy = std::mem::transmute(address);
    let source = original(uid);
    if source.is_null() {
        return source;
    }
    let saved_errno = *libc::__error();
    let Some((_, home)) = HOME.get().filter(|(current, _)| uid == *current) else {
        return source;
    };
    let result = LEGACY.with(|slot| {
        let mut copy = OwnedEntry {
            entry: std::ptr::read(source),
            strings: Vec::with_capacity(6),
        };
        for (index, pointer) in [
            (*source).pw_name,
            (*source).pw_passwd,
            (*source).pw_class,
            (*source).pw_gecos,
            (*source).pw_dir,
            (*source).pw_shell,
        ]
        .into_iter()
        .enumerate()
        {
            if pointer.is_null() {
                continue;
            }
            let value = if index == 4 {
                home.clone()
            } else {
                CStr::from_ptr(pointer).to_owned()
            };
            copy.strings.push(value);
            let pointer = copy.strings.last().unwrap().as_ptr().cast_mut();
            match index {
                0 => copy.entry.pw_name = pointer,
                1 => copy.entry.pw_passwd = pointer,
                2 => copy.entry.pw_class = pointer,
                3 => copy.entry.pw_gecos = pointer,
                4 => copy.entry.pw_dir = pointer,
                _ => copy.entry.pw_shell = pointer,
            }
        }
        let mut slot = slot.borrow_mut();
        *slot = Some(copy);
        &mut slot.as_mut().unwrap().entry as *mut libc::passwd
    });
    *libc::__error() = saved_errno;
    result
}

#[repr(C)]
struct Interpose {
    replacement: *const libc::c_void,
    original: *const libc::c_void,
}
// Immutable relocations written by dyld, not mutable application pointers.
unsafe impl Sync for Interpose {}

#[used]
#[link_section = "__DATA,__interpose"]
static INTERPOSE: [Interpose; 2] = [
    Interpose {
        replacement: getpwuid as *const () as _,
        original: libc::getpwuid as *const () as _,
    },
    Interpose {
        replacement: getpwuid_r as *const () as _,
        original: libc::getpwuid_r as *const () as _,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_keeps_other_fields_and_respects_buffer_capacity() {
        let mut buffer = [0u8; 96];
        buffer[..11].copy_from_slice(b"name\0/home\0");
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        entry.pw_name = buffer.as_mut_ptr().cast();
        entry.pw_dir = unsafe { buffer.as_mut_ptr().add(5).cast() };
        entry.pw_uid = 42;
        let home = CString::new("/private/orbit/home").unwrap();
        unsafe { replace_home(&mut entry, buffer.as_mut_ptr().cast(), 96, &home) }.unwrap();
        assert_eq!(unsafe { CStr::from_ptr(entry.pw_dir) }, home.as_c_str());
        assert_eq!(unsafe { CStr::from_ptr(entry.pw_name) }.to_bytes(), b"name");
        assert_eq!(entry.pw_uid, 42);
        entry.pw_dir = unsafe { buffer.as_mut_ptr().add(5).cast() };
        assert_eq!(
            unsafe { replace_home(&mut entry, buffer.as_mut_ptr().cast(), 11, &home) },
            Err(libc::ERANGE)
        );
    }

    #[test]
    fn malformed_nss_string_does_not_read_outside_the_buffer() {
        let mut buffer = [b'x'; 10];
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        entry.pw_dir = buffer.as_mut_ptr().cast();
        let home = CString::new("/home").unwrap();
        assert_eq!(
            unsafe { replace_home(&mut entry, buffer.as_mut_ptr().cast(), 10, &home) },
            Err(libc::EIO)
        );
        entry.pw_dir = std::ptr::dangling_mut();
        assert_eq!(
            unsafe { replace_home(&mut entry, buffer.as_mut_ptr().cast(), 10, &home) },
            Err(libc::EIO)
        );
    }
}
