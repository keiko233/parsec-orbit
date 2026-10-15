//! Elevated restart-and-apply command used by the Windows UI.
//!
//! The helper writes the console choice to the Parsec service's own
//! environment, then restarts that service and the exact client process that
//! launched this helper. It never changes the machine-wide environment.

const COMMAND: &str = "--restart-and-apply";

/// Runs the maintenance command when `args` selects it.
///
/// Returns `None` for ordinary companion startup arguments. The executable
/// should call this before initializing its GUI event loop.
pub fn run(args: &[String]) -> Option<Result<(), String>> {
    if args.first().map(String::as_str) != Some(COMMAND) {
        return None;
    }

    Some((|| {
        let (console, parent_pid) = parse_arguments(args)?;
        #[cfg(windows)]
        {
            restart_and_apply(console, parent_pid)
        }
        #[cfg(not(windows))]
        {
            let _ = (console, parent_pid);
            Err("restart-and-apply is supported only on Windows".to_string())
        }
    })())
}

fn parse_arguments(args: &[String]) -> Result<(&str, u32), String> {
    if args.len() != 3 {
        return Err(format!("usage: {COMMAND} <off|1|client|host> <parent-pid>"));
    }
    let console = args[1].as_str();
    if !matches!(console, "off" | "1" | "client" | "host") {
        return Err(format!(
            "invalid console mode {console:?}; expected off, 1, client, or host"
        ));
    }
    let parent_pid = args[2]
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or_else(|| "parent-pid must be a nonzero decimal process id".to_string())?;
    Ok((console, parent_pid))
}

#[cfg(windows)]
fn restart_and_apply(console: &str, parent_pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_SERVICE_DOES_NOT_EXIST};
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
        SERVICE_START, SERVICE_STOP,
    };

    // Validate and retain the caller before touching service state. Holding
    // this handle pins the process object even if the numeric PID is reused.
    let parent = open_parent_parsec(parent_pid)?;
    let manager = unsafe {
        // SAFETY: null selects the local machine and the default SCM database.
        OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT)
    };
    if manager.is_null() {
        return Err(last_error(
            "could not open the Windows Service Control Manager",
        ));
    }
    let service_name = wide("Parsec");
    let service = unsafe {
        // SAFETY: manager is a live SCM handle and service_name is nul-terminated.
        OpenServiceW(
            manager,
            service_name.as_ptr(),
            SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP,
        )
    };
    if service.is_null() {
        let code = unsafe { GetLastError() };
        unsafe {
            // SAFETY: manager was returned by OpenSCManagerW and is still live.
            CloseServiceHandle(manager);
        }
        if code == ERROR_SERVICE_DOES_NOT_EXIST {
            return Err("the Parsec service is not installed; standalone client restart is not enabled because its executable path cannot be verified".to_string());
        }
        return Err(format!(
            "could not open the Parsec service (Windows error {code})"
        ));
    }

    let result = (|| {
        write_service_console(console)?;
        if let Err(stop_error) = stop_service(service) {
            return match ensure_service_running(service) {
                Ok(()) => Err(stop_error),
                Err(recovery_error) => Err(format!(
                    "{stop_error}; additionally, could not restore the Parsec service: {recovery_error}"
                )),
            };
        }
        let terminate_result = terminate_parent_parsec(&parent);
        let start_result = start_service(service);
        match (terminate_result, start_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Err(stop_error), Err(start_error)) => {
                Err(format!("{stop_error}; additionally, {start_error}"))
            }
        }
    })();

    unsafe {
        // SAFETY: both handles were returned by their corresponding Open calls.
        CloseServiceHandle(service);
        CloseServiceHandle(manager);
    }
    result
}

#[cfg(windows)]
fn stop_service(service: windows_sys::Win32::System::Services::SC_HANDLE) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{
        ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_NOT_ACTIVE,
    };
    use windows_sys::Win32::System::Services::{
        ControlService, SERVICE_CONTROL_STOP, SERVICE_START_PENDING, SERVICE_STATUS,
        SERVICE_STOPPED, SERVICE_STOP_PENDING,
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let current = query_service_status(service)?;
        match current.dwCurrentState {
            SERVICE_STOPPED => return Ok(()),
            SERVICE_STOP_PENDING | SERVICE_START_PENDING => wait_service_step(deadline)?,
            _ => {
                let mut status = SERVICE_STATUS::default();
                let stopped = unsafe {
                    // SAFETY: service is an open service handle with SERVICE_STOP access.
                    ControlService(service, SERVICE_CONTROL_STOP, &mut status)
                };
                if stopped == 0 {
                    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
                    if code == ERROR_SERVICE_NOT_ACTIVE {
                        continue;
                    }
                    if code == ERROR_SERVICE_CANNOT_ACCEPT_CTRL {
                        wait_service_step(deadline)?;
                        continue;
                    }
                    return Err(format!(
                        "could not stop the Parsec service (Windows error {code})"
                    ));
                }
                // A successful control request is asynchronous. Poll the
                // actual state, including STOP_PENDING, before touching client.
                wait_service_step(deadline)?;
            }
        }
    }
}

#[cfg(windows)]
fn start_service(service: windows_sys::Win32::System::Services::SC_HANDLE) -> Result<(), String> {
    use windows_sys::Win32::System::Services::StartServiceW;

    let ok = unsafe {
        // SAFETY: service is an open service handle; the service takes no arguments.
        StartServiceW(service, 0, std::ptr::null())
    };
    if ok != 0 {
        return Ok(());
    }
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    // The service may already have been started by its recovery policy.
    if code == windows_sys::Win32::Foundation::ERROR_SERVICE_ALREADY_RUNNING {
        Ok(())
    } else {
        Err(format!(
            "could not start the Parsec service (Windows error {code})"
        ))
    }
}

#[cfg(windows)]
struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: this wrapper uniquely owns the handle returned by OpenProcess.
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn open_parent_parsec(parent_pid: u32) -> Result<ProcessHandle, String> {
    use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;

    let handle = open_process(parent_pid)?;
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    let ok = unsafe {
        // SAFETY: process is open and path/length are writable output buffers.
        QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut length)
    };
    if ok == 0 {
        return Err(last_error("could not verify the launching process image"));
    }
    let image = String::from_utf16_lossy(&path[..length as usize]);
    if !same_executable_name(&image, "parsecd.exe") {
        return Err(format!(
            "refusing to restart: PID {parent_pid} is not parsecd.exe ({image})"
        ));
    }
    Ok(handle)
}

#[cfg(windows)]
fn open_process(parent_pid: u32) -> Result<ProcessHandle, String> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    let process = unsafe {
        // SAFETY: opens only the PID supplied by the launching UI process. The
        // synchronize right is needed by WaitForSingleObject during restart.
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            0,
            parent_pid,
        )
    };
    if process.is_null() {
        return Err(last_error("could not open the launching process"));
    }
    Ok(ProcessHandle(process))
}

#[cfg(windows)]
fn terminate_parent_parsec(parent: &ProcessHandle) -> Result<(), String> {
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};

    let state = unsafe {
        // SAFETY: the validated process handle remains owned throughout restart.
        WaitForSingleObject(parent.0, 0)
    };
    if state == windows_sys::Win32::Foundation::WAIT_OBJECT_0 {
        return Ok(());
    }
    if state != WAIT_TIMEOUT {
        return Err(last_error("could not check the launching Parsec process"));
    }
    let ok = unsafe {
        // SAFETY: parent is the held, previously validated process handle.
        TerminateProcess(parent.0, 0)
    };
    if ok == 0 {
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        // It may have exited between the zero-time wait and TerminateProcess.
        if unsafe { WaitForSingleObject(parent.0, 0) }
            == windows_sys::Win32::Foundation::WAIT_OBJECT_0
        {
            return Ok(());
        }
        return Err(format!(
            "could not terminate the launching Parsec process (Windows error {code})"
        ));
    }
    let waited = unsafe {
        // SAFETY: parent is a live process handle retained by the caller.
        WaitForSingleObject(parent.0, 30_000)
    };
    if waited == windows_sys::Win32::Foundation::WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err("timed out waiting for the launching Parsec process to exit".to_string())
    }
}

#[cfg(windows)]
fn query_service_status(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
) -> Result<windows_sys::Win32::System::Services::SERVICE_STATUS_PROCESS, String> {
    use windows_sys::Win32::System::Services::{
        QueryServiceStatusEx, SC_STATUS_PROCESS_INFO, SERVICE_STATUS_PROCESS,
    };

    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    let ok = unsafe {
        // SAFETY: status is the correctly sized writable output buffer.
        QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        Err(last_error("could not query Parsec service status"))
    } else {
        Ok(status)
    }
}

#[cfg(windows)]
fn wait_service_step(deadline: std::time::Instant) -> Result<(), String> {
    if std::time::Instant::now() >= deadline {
        return Err("timed out waiting for the Parsec service state to change".to_string());
    }
    std::thread::sleep(std::time::Duration::from_millis(250));
    Ok(())
}

#[cfg(windows)]
fn ensure_service_running(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
) -> Result<(), String> {
    use windows_sys::Win32::System::Services::{
        SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let state = query_service_status(service)?.dwCurrentState;
        match state {
            SERVICE_RUNNING => return Ok(()),
            SERVICE_START_PENDING | SERVICE_STOP_PENDING => wait_service_step(deadline)?,
            SERVICE_STOPPED => return start_service(service),
            _ => return Err(format!("Parsec service is in unexpected state {state}")),
        }
    }
}

#[cfg(windows)]
fn write_service_console(console: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
        KEY_QUERY_VALUE, KEY_SET_VALUE, REG_MULTI_SZ, REG_VALUE_TYPE,
    };

    const SERVICE_KEY: &str = r"SYSTEM\CurrentControlSet\Services\Parsec";
    let key_path = wide(SERVICE_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        // SAFETY: the path is nul-terminated and key is a writable output.
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            key_path.as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "could not open the Parsec service registry key (Windows error {status})"
        ));
    }

    let result = (|| {
        let value_name = wide("Environment");
        let mut value_type: REG_VALUE_TYPE = 0;
        let mut byte_len = 0u32;
        let query_status = unsafe {
            // SAFETY: null data requests only the value's type and byte length.
            RegQueryValueExW(
                key,
                value_name.as_ptr(),
                std::ptr::null(),
                &mut value_type,
                std::ptr::null_mut(),
                &mut byte_len,
            )
        };
        let mut entries = if query_status == ERROR_FILE_NOT_FOUND {
            Vec::new()
        } else {
            if query_status != ERROR_SUCCESS {
                return Err(format!(
                    "could not read Parsec service Environment (Windows error {query_status})"
                ));
            }
            if value_type != REG_MULTI_SZ || !byte_len.is_multiple_of(2) {
                return Err(
                    "Parsec service Environment is not a valid REG_MULTI_SZ value".to_string(),
                );
            }
            let mut bytes = vec![0u8; byte_len as usize];
            let read_status = unsafe {
                // SAFETY: bytes has the queried length and all output pointers are valid.
                RegQueryValueExW(
                    key,
                    value_name.as_ptr(),
                    std::ptr::null(),
                    &mut value_type,
                    bytes.as_mut_ptr(),
                    &mut byte_len,
                )
            };
            if read_status != ERROR_SUCCESS {
                return Err(format!(
                    "could not read Parsec service Environment data (Windows error {read_status})"
                ));
            }
            let words: Vec<u16> = bytes[..byte_len as usize]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            parse_multi_sz(&words)?
        };
        let replacement = format!("PARSEC_ORBIT_CONSOLE={console}");
        entries.retain(|entry| !entry_name(entry).eq_ignore_ascii_case("PARSEC_ORBIT_CONSOLE"));
        entries.push(replacement);
        let encoded = encode_multi_sz(&entries);
        let bytes: Vec<u8> = encoded.iter().flat_map(|word| word.to_le_bytes()).collect();
        let write_status = unsafe {
            // SAFETY: encoded is a terminated UTF-16 MULTI_SZ buffer.
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_MULTI_SZ,
                bytes.as_ptr(),
                bytes.len() as u32,
            )
        };
        if write_status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "could not update Parsec service Environment (Windows error {write_status})"
            ))
        }
    })();
    unsafe {
        // SAFETY: key was opened by RegOpenKeyExW.
        RegCloseKey(key);
    }
    result
}

fn entry_name(entry: &str) -> &str {
    entry.split_once('=').map_or(entry, |(name, _)| name)
}

fn parse_multi_sz(words: &[u16]) -> Result<Vec<String>, String> {
    if words.len() < 2 || words[words.len() - 2..] != [0, 0] {
        return Err("Parsec service Environment has an unterminated MULTI_SZ value".to_string());
    }
    let mut entries = Vec::new();
    let mut start = 0;
    for (index, word) in words.iter().enumerate() {
        if *word == 0 {
            if index == start {
                break;
            }
            entries.push(
                String::from_utf16(&words[start..index]).map_err(|_| {
                    "Parsec service Environment contains invalid UTF-16".to_string()
                })?,
            );
            start = index + 1;
        }
    }
    Ok(entries)
}

fn encode_multi_sz(entries: &[String]) -> Vec<u16> {
    let mut words = Vec::new();
    for entry in entries {
        words.extend(entry.encode_utf16());
        words.push(0);
    }
    words.push(0);
    if entries.is_empty() {
        words.push(0);
    }
    words
}

fn same_executable_name(path: &str, expected: &str) -> bool {
    path.rsplit(['\\', '/'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn last_error(context: &str) -> String {
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    format!("{context} (Windows error {code})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_arguments_are_strictly_validated() {
        for mode in ["off", "1", "client", "host"] {
            assert_eq!(
                parse_arguments(&[COMMAND.into(), mode.into(), "42".into()]),
                Ok((mode, 42))
            );
        }
        assert!(parse_arguments(&[COMMAND.into(), "machine".into(), "42".into()]).is_err());
        assert!(parse_arguments(&[COMMAND.into(), "host".into(), "0".into()]).is_err());
        assert!(parse_arguments(&[COMMAND.into(), "host".into(), "42x".into()]).is_err());
        assert!(parse_arguments(&[COMMAND.into(), "host".into()]).is_err());
    }

    #[test]
    fn multi_sz_update_keeps_other_variables_and_replaces_console_case_insensitively() {
        let initial = encode_multi_sz(&[
            "PATH=C:\\Parsec".into(),
            "parsec_orbit_console=client".into(),
            "TEMP=C:\\Temp".into(),
        ]);
        let mut entries = parse_multi_sz(&initial).unwrap();
        entries.retain(|entry| !entry_name(entry).eq_ignore_ascii_case("PARSEC_ORBIT_CONSOLE"));
        entries.push("PARSEC_ORBIT_CONSOLE=host".into());
        assert_eq!(
            parse_multi_sz(&encode_multi_sz(&entries)).unwrap(),
            [
                "PATH=C:\\Parsec",
                "TEMP=C:\\Temp",
                "PARSEC_ORBIT_CONSOLE=host"
            ]
        );
    }

    #[test]
    fn multi_sz_round_trips_empty_and_rejects_unterminated_data() {
        assert_eq!(
            parse_multi_sz(&encode_multi_sz(&[])).unwrap(),
            Vec::<String>::new()
        );
        assert!(parse_multi_sz(&[b'A' as u16, 0]).is_err());
    }

    #[test]
    fn process_image_validation_checks_only_the_final_executable_component() {
        assert!(same_executable_name(
            r"C:\Program Files\Parsec\parsecd.exe",
            "parsecd.exe"
        ));
        assert!(same_executable_name(
            r"C:/Parsec/PARSECD.EXE",
            "parsecd.exe"
        ));
        assert!(!same_executable_name(
            r"C:\Parsec\parsecd-helper.exe",
            "parsecd.exe"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn process_handle_can_be_waited_and_non_parsec_is_rejected_before_service_access() {
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::{GetCurrentProcessId, WaitForSingleObject};

        let pid = unsafe {
            // SAFETY: this reads the current test process ID.
            GetCurrentProcessId()
        };
        let process = open_process(pid).expect("open the current test process");
        let state = unsafe {
            // SAFETY: open_process requests PROCESS_SYNCHRONIZE on this handle.
            WaitForSingleObject(process.0, 0)
        };
        assert_eq!(state, WAIT_TIMEOUT);
        drop(process);

        let error = match open_parent_parsec(pid) {
            Ok(handle) => {
                drop(handle);
                panic!("the test executable must not be accepted as parsecd.exe");
            }
            Err(error) => error,
        };
        assert!(error.starts_with("refusing to restart: PID"), "{error}");
    }
}
