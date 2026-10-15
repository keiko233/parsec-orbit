//! Which Parsec instance this process is.
//!
//! The LocalSystem host can run in the user's session too. It must not claim
//! the user's global hotkeys or start a companion under the service account.
//! Require a known nonzero session and a known non-System process token.

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TokenUser, WinLocalSystemSid, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Whether this is the instance the user started, where a window is visible.
pub fn is_client() -> bool {
    let mut session_id = 0;
    // SAFETY: the current process ID is live, and `session_id` is writable for
    // the duration of the call.
    let known = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) } != 0;
    is_client_instance(known.then_some(session_id), is_local_system())
}

fn is_client_instance(session_id: Option<u32>, local_system: Option<bool>) -> bool {
    session_id.is_some_and(|session_id| session_id != 0) && local_system == Some(false)
}

fn is_local_system() -> Option<bool> {
    let mut token = std::ptr::null_mut();
    // Safety: the output points to a live handle slot and the pseudo-handle
    // identifies this process, not a potentially reused external PID.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    // TOKEN_USER plus the largest Windows SID fits in this aligned buffer.
    let mut buffer = [0usize; 64];
    let mut needed = 0;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(&buffer) as u32,
            &mut needed,
        )
    };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return None;
    }
    // Safety: a successful query populated TOKEN_USER and its SID within the
    // aligned buffer, which remains alive throughout IsWellKnownSid.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    Some(unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_zero_is_the_service_instance() {
        assert!(!is_client_instance(Some(0), Some(false)));
    }

    #[test]
    fn nonzero_session_is_the_client_instance() {
        assert!(is_client_instance(Some(1), Some(false)));
        assert!(is_client_instance(Some(7), Some(false)));
    }

    #[test]
    fn unknown_session_fails_closed() {
        assert!(!is_client_instance(None, Some(false)));
    }

    #[test]
    fn system_host_in_the_users_session_must_not_claim_desktop_hotkeys() {
        assert!(!is_client_instance(Some(1), Some(true)));
        assert!(is_client_instance(Some(1), Some(false)));
        assert!(!is_client_instance(Some(0), Some(false)));
        assert!(!is_client_instance(Some(1), None));
    }
}
