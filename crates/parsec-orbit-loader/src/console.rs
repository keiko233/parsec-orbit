//! The debug console window.
//!
//! `parsecd.exe` is a GUI process, so it has no console of its own and its log
//! goes to a file. When `PARSEC_ORBIT_CONSOLE` asks for one, the loader worker
//! allocates a console window at start-up and installs it as the sink in
//! `parsec-orbit-core`, which mirrors every log line to it - including the
//! lines `log::log` writes for plugins.
//!
//! Which processes open a window is [`Target`]: the client runs twice on a
//! normal machine (see [`instance`]) and a Windows console belongs to one
//! process, so `1` gives every instance its own window while `client` and
//! `host` keep a debug session to a single one.
//!
//! Nothing here may fail the load: a console that cannot be opened is a
//! warning in the log file and nothing more. Closing the window is the one
//! thing a console cannot be protected from - see [`keep_alive`] - so the log
//! file stays the place to read a session that ended that way.

use std::ffi::c_void;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use parsec_orbit_core::console::{self, Console, Target};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, EqualSid, GetTokenInformation, TokenUser, WinLocalSystemSid, PSID,
    SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AllocConsole, GetConsoleMode, GetConsoleWindow, SetConsoleCtrlHandler, SetConsoleTitleW,
    WriteConsoleW, CTRL_BREAK_EVENT, CTRL_C_EVENT,
};
use windows_sys::Win32::System::Threading::{CreateThread, GetCurrentProcess, OpenProcessToken};

/// The one console this process can have.
static CONSOLE: OnceLock<DebugConsole> = OnceLock::new();

/// What [`ensure`] did for this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The console sink is installed: a window was opened here, or an
    /// existing console was taken over.
    Open,
    /// `PARSEC_ORBIT_CONSOLE` is off.
    Off,
    /// The setting asks for the other instance, not this one.
    OtherInstance,
}

/// The console and the screen buffer its lines go to.
struct DebugConsole {
    output: HANDLE,
}

// Safety: the handle is this process's console screen buffer, which lives as
// long as the process does, and `WriteConsoleW` is safe to call from any
// thread. The loader never closes the handle or the console.
unsafe impl Send for DebugConsole {}
unsafe impl Sync for DebugConsole {}

impl Console for DebugConsole {
    fn print_line(&self, line: &str) {
        // The console wants the line break spelled out, and UTF-16 rather
        // than the log file's UTF-8.
        let mut text: Vec<u16> = OsStr::new(line).encode_wide().collect();
        text.extend_from_slice(&[b'\r' as u16, b'\n' as u16]);

        // A console write can come up short when the buffer is small; write
        // the rest instead of losing the line.
        let mut offset = 0usize;
        while offset < text.len() {
            let mut written = 0u32;
            let ok = unsafe {
                WriteConsoleW(
                    self.output,
                    text.as_ptr().add(offset),
                    (text.len() - offset) as u32,
                    &mut written,
                    std::ptr::null(),
                )
            };
            if ok == 0 || written == 0 {
                return;
            }
            offset += written as usize;
        }
    }
}

/// Opens the console when `target` covers this instance, installs it as the
/// log sink and gives it the loader's title.
///
/// Returns the Win32 error code when a console was asked for and could not be
/// opened. The process may already have one (a debugger, a host that was
/// started from a terminal); that console is reused instead of opening a
/// second window.
pub fn ensure(target: Target) -> Result<Outcome, u32> {
    if target == Target::Off {
        return Ok(Outcome::Off);
    }
    if !wants(target, is_service_instance()) {
        return Ok(Outcome::OtherInstance);
    }
    if console::installed() {
        return Ok(Outcome::Open);
    }

    let fresh = unsafe { GetConsoleWindow() }.is_null();
    if fresh && unsafe { AllocConsole() } == 0 {
        let error = unsafe { GetLastError() };
        // `GetConsoleWindow` also returns null for a console whose window is
        // not visible to this process, and there `AllocConsole` fails with
        // `ERROR_ACCESS_DENIED` while `CONOUT$` still works.
        if error != ERROR_ACCESS_DENIED {
            return Err(error);
        }
    }

    let output = screen_buffer().ok_or_else(|| unsafe { GetLastError() })?;
    let console = CONSOLE.get_or_init(|| DebugConsole { output });

    if fresh {
        // Only the window this process opened is ours to rename and to give a
        // control handler; another process's console is left alone.
        let title = format!(
            "Parsec Orbit {} - {} pid={}",
            env!("CARGO_PKG_VERSION"),
            crate::log::process_name(),
            std::process::id()
        );
        let title = wide(&title);
        unsafe { SetConsoleTitleW(title.as_ptr()) };
        unsafe { SetConsoleCtrlHandler(Some(keep_alive), 1) };
    }

    console::install(console);
    Ok(Outcome::Open)
}

/// Whether `target` asks for a console in an instance like this one.
fn wants(target: Target, service_instance: bool) -> bool {
    match target {
        Target::Off => false,
        Target::All => true,
        Target::Client => !service_instance,
        Target::Host => service_instance,
    }
}

/// The instance this process is, in the words [`Target`] uses.
///
/// Both instances are `parsecd.exe` in the user's session with the same
/// environment; the token is what tells them apart, so this is `host` exactly
/// when the process runs as `LocalSystem`.
pub fn instance() -> &'static str {
    if is_service_instance() {
        "host"
    } else {
        "client"
    }
}

/// Whether this process is the instance the Parsec service started, which
/// runs as `LocalSystem` (`S-1-5-18`).
///
/// A token that cannot be read counts as the client's: the user instance is
/// the one a debug session is normally after, and guessing "host" would
/// silently leave the window closed.
fn is_service_instance() -> bool {
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
    sid_is_local_system(sid)
}

/// Whether `sid` is `S-1-5-18`.
fn sid_is_local_system(sid: PSID) -> bool {
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

/// Handles the console events a user can raise in the window.
///
/// Ctrl+C and Ctrl+Break are what a keypress sends; returning true means the
/// system treats them as handled and the client keeps running, so a stray
/// keypress in a debug window cannot end a session.
///
/// Closing the window is the one event that cannot be survived. Windows sends
/// `CTRL_CLOSE_EVENT` "so that the process has an opportunity to clean up
/// before termination" and then terminates it whatever the handler returns
/// (measured: a handler that returns true, with and without `FreeConsole`,
/// still dies). Returning false leaves that default in place rather than
/// pretending the client outlives the window. `CTRL_LOGOFF_EVENT` and
/// `CTRL_SHUTDOWN_EVENT` are not even delivered to a process that has loaded
/// user32, which `parsecd.exe` has.
unsafe extern "system" fn keep_alive(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => 1,
        _ => 0,
    }
}

/// Starts the thread that mirrors Parsec's own log into the console.
///
/// `parsec-orbit-core` owns what is followed and how; the thread is created
/// here with the same `CreateThread` the plugin worker uses, because
/// `std::thread` would add about 20 KB of runtime to the DLL.
pub fn follow(paths: Vec<PathBuf>) -> Result<(), u32> {
    let parameter = Box::into_raw(Box::new(paths)) as *mut c_void;
    let handle = unsafe {
        CreateThread(
            std::ptr::null(),
            0,
            Some(follow_worker),
            parameter,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() {
        // No thread took the box; take it back so the paths are not leaked.
        unsafe { drop(Box::from_raw(parameter as *mut Vec<PathBuf>)) };
        return Err(unsafe { GetLastError() });
    }
    unsafe { CloseHandle(handle) };
    Ok(())
}

/// Thread body for [`follow`]: owns the paths it was handed.
unsafe extern "system" fn follow_worker(parameter: *mut c_void) -> u32 {
    let paths = unsafe { Box::from_raw(parameter as *mut Vec<PathBuf>) };
    parsec_orbit_core::console::follow_forever(*paths);
    0
}

/// The screen buffer to write to.
///
/// `CONOUT$` rather than `GetStdHandle(STD_OUTPUT_HANDLE)`: the standard
/// handles can be redirected (a host that pipes them, a console allocated
/// after they were set up) while `CONOUT$` always names the console this
/// process is attached to. `GetConsoleMode` is the console API's own way to
/// tell a console handle from a redirected one.
fn screen_buffer() -> Option<HANDLE> {
    let name = wide("CONOUT$");
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut mode = 0u32;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        unsafe { CloseHandle(handle) };
        return None;
    }
    Some(handle)
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_covers_exactly_the_instances_it_names() {
        let cases = [
            (Target::Off, false, false),
            (Target::Off, true, false),
            (Target::All, false, true),
            (Target::All, true, true),
            (Target::Client, false, true),
            (Target::Client, true, false),
            (Target::Host, false, false),
            (Target::Host, true, true),
        ];
        for (target, service_instance, expected) in cases {
            assert_eq!(
                wants(target, service_instance),
                expected,
                "{target:?} in the {} instance",
                if service_instance { "host" } else { "client" }
            );
        }
    }

    #[test]
    fn this_test_process_is_the_client_instance() {
        // `cargo test` runs as the logged-in user, never as `LocalSystem`, so
        // the detection has to answer "client" here - and the same call is
        // what picks the window in a real client.
        assert!(!is_service_instance());
        assert_eq!(instance(), "client");
    }

    #[test]
    fn only_local_system_is_the_host_instance() {
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
        assert_eq!(built, 1, "the well-known SID has to be built");
        assert!(sid_is_local_system(system.as_mut_ptr().cast()));
        assert!(!sid_is_local_system(std::ptr::null_mut()));
    }
}
