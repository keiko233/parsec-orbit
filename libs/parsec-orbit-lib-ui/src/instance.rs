//! Which Parsec instance this process is.
//!
//! The client runs twice: the instance the Parsec service starts in session 0,
//! and the one the user starts in an interactive session. A window in session
//! 0 is invisible to the user, so the companion UI and its hotkeys only run in
//! a nonzero session. If the session cannot be read, fail closed and do not
//! launch the UI.

use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

/// Whether this is the instance the user started, where a window is visible.
pub fn is_client() -> bool {
    let mut session_id = 0;
    // SAFETY: the current process ID is live, and `session_id` is writable for
    // the duration of the call.
    let known = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) } != 0;
    is_client_session(known.then_some(session_id))
}

fn is_client_session(session_id: Option<u32>) -> bool {
    session_id.is_some_and(|session_id| session_id != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_zero_is_the_service_instance() {
        assert!(!is_client_session(Some(0)));
    }

    #[test]
    fn nonzero_session_is_the_client_instance() {
        assert!(is_client_session(Some(1)));
        assert!(is_client_session(Some(7)));
    }

    #[test]
    fn unknown_session_fails_closed() {
        assert!(!is_client_session(None));
    }
}
