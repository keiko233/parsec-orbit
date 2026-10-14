//! Read-only signature and Gatekeeper evidence. A valid signature does not
//! imply Gatekeeper acceptance, and a rejection never removes quarantine.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct ToolEvidence {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Serialize)]
pub struct Assessment {
    pub app: PathBuf,
    pub bundle_id: String,
    pub strict_signature: ToolEvidence,
    pub signing_details: ToolEvidence,
    pub entitlements: ToolEvidence,
    pub gatekeeper: ToolEvidence,
    pub quarantine: Option<Vec<u8>>,
}

#[cfg(target_os = "macos")]
pub fn assess(app: &Path) -> Result<Assessment, String> {
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::process::Command;
    fn run(program: &str, args: &[&OsStr]) -> Result<ToolEvidence, String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|error| format!("{program}: {error}"))?;
        Ok(ToolEvidence {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
    let app = std::fs::canonicalize(app).map_err(|error| error.to_string())?;
    if !app.is_dir() {
        return Err("assessment requires an app bundle directory".into());
    }
    let bundle_id = crate::platform::bundle_identifier(&app)?;
    let path = CString::new(app.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let attr = c"com.apple.quarantine";
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            attr.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    let quarantine = if size < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOATTR) {
            return Err(error.to_string());
        }
        None
    } else {
        if size > 65536 {
            return Err("quarantine attribute exceeds 64 KiB".into());
        }
        let mut bytes = vec![0u8; size as usize];
        let read = unsafe {
            libc::getxattr(
                path.as_ptr(),
                attr.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if read != size {
            return Err("quarantine changed during assessment".into());
        }
        Some(bytes)
    };
    let args = |flags: &[&str]| -> Vec<std::ffi::OsString> {
        flags
            .iter()
            .map(std::ffi::OsString::from)
            .chain(std::iter::once(app.as_os_str().to_owned()))
            .collect()
    };
    let execute = |program, flags: &[&str]| {
        let args = args(flags);
        let refs: Vec<&OsStr> = args.iter().map(|arg| arg.as_os_str()).collect();
        run(program, &refs)
    };
    let strict_signature = execute("/usr/bin/codesign", &["--verify", "--strict"])?;
    let signing_details = execute("/usr/bin/codesign", &["--display", "--verbose=4"])?;
    let entitlements = execute("/usr/bin/codesign", &["--display", "--entitlements", ":-"])?;
    let gatekeeper = execute(
        "/usr/sbin/spctl",
        &["--assess", "--type", "execute", "--verbose=4"],
    )?;
    Ok(Assessment {
        app,
        bundle_id,
        strict_signature,
        signing_details,
        entitlements,
        gatekeeper,
        quarantine,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn assess(_app: &Path) -> Result<Assessment, String> {
    Err("signature and Gatekeeper assessment requires macOS".into())
}
