//! Which Parsec instance this process is.
//!
//! The client runs twice: the instance the Parsec service starts, which runs as
//! `LocalSystem` (`S-1-5-18`) in session 0, and the one the user starts. A
//! window in session 0 is invisible to the user, so the companion UI and its
//! hotkeys only run in the user's instance. The check matches the debug
//! console's (`parsec-orbit-loader-windows`): the process token's user is the only
//! difference, and a token that cannot be read counts as the client's.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, EqualSid, GetTokenInformation, TokenUser, WinLocalSystemSid, PSID,
    SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Whether this is the instance the user started, where a window is visible.
pub fn is_client() -> bool {
    !is_service()
}

/// Whether this process is the instance the Parsec service started.
fn is_service() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut buffer = [0u8; 256];
    let mut needed = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut needed,
        )
    };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return false;
    }
    // Safety: the query succeeded, so the buffer holds a `TOKEN_USER` whose
    // `Sid` points at a valid SID.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    is_local_system(sid)
}

/// Whether `sid` is `S-1-5-18`.
fn is_local_system(sid: PSID) -> bool {
    if sid.is_null() {
        return false;
    }
    let mut system = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = system.len() as u32;
    let built = unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            std::ptr::null_mut(),
            system.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if built == 0 {
        return false;
    }
    unsafe { EqualSid(sid, system.as_mut_ptr().cast()) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test` runs as the logged-in user, never as `LocalSystem`, so the
    /// test process is the client instance - the same assumption the loader's
    /// console test makes.
    #[test]
    fn this_test_process_is_the_client_instance() {
        assert!(is_client());
        assert!(!is_service());
    }
}
