//! Filesystem metadata captured alongside installer tree digests.
//!
//! The serialized representation uses ordinary integers and byte arrays so
//! manifests remain portable JSON. Fields are optional to support manifests
//! written before metadata snapshots were introduced.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A filesystem modification time at nanosecond precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTimestamp {
    pub seconds: i64,
    pub nanoseconds: i64,
}

/// One macOS extended attribute, with its original name and value bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtendedAttribute {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

/// Optional Unix and macOS metadata for one file, directory, or symlink.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<FileTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// `Some(vec![])` records that xattrs were read and none were present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xattrs: Option<Vec<ExtendedAttribute>>,
    /// External ACL bytes; an empty value records that no extended ACL exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<Vec<u8>>,
}

/// Captures supported metadata without following the final symlink component.
pub(super) fn capture(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<Option<MetadataSnapshot>, String> {
    let mut snapshot = MetadataSnapshot::default();

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        snapshot.mtime = Some(FileTimestamp {
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec(),
        });
        snapshot.uid = Some(metadata.uid());
        snapshot.gid = Some(metadata.gid());
    }

    #[cfg(target_os = "macos")]
    {
        snapshot.xattrs = Some(capture_xattrs(path)?);
        snapshot.acl = Some(capture_acl(path, metadata.file_type().is_symlink())?);
    }

    if snapshot.mtime.is_some()
        || snapshot.uid.is_some()
        || snapshot.gid.is_some()
        || snapshot.xattrs.is_some()
        || snapshot.acl.is_some()
    {
        Ok(Some(snapshot))
    } else {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
fn capture_xattrs(path: &Path) -> Result<Vec<ExtendedAttribute>, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", path.display()))?;
    let nofollow = libc::XATTR_NOFOLLOW;

    for _ in 0..3 {
        let size = unsafe { libc::listxattr(c_path.as_ptr(), ptr::null_mut(), 0, nofollow) };
        if size < 0 {
            return Err(format!(
                "cannot list extended attributes on {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        if size == 0 {
            return Ok(Vec::new());
        }

        let mut names = vec![0_u8; size as usize];
        let written = unsafe {
            libc::listxattr(
                c_path.as_ptr(),
                names.as_mut_ptr().cast(),
                names.len(),
                nofollow,
            )
        };
        if written < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ERANGE) {
                continue;
            }
            return Err(format!(
                "cannot list extended attributes on {}: {error}",
                path.display()
            ));
        }
        if written as usize > names.len() {
            continue;
        }
        names.truncate(written as usize);

        let mut attributes = Vec::new();
        for name in names
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            let c_name = CString::new(name).map_err(|_| {
                format!("{} has an invalid extended attribute name", path.display())
            })?;
            let value = read_xattr(&c_path, &c_name).map_err(|error| {
                format!(
                    "cannot read extended attribute on {}: {error}",
                    path.display()
                )
            })?;
            attributes.push(ExtendedAttribute {
                name: name.to_vec(),
                value,
            });
        }
        attributes.sort_by(|left, right| left.name.cmp(&right.name));
        return Ok(attributes);
    }

    Err(format!(
        "extended attributes on {} changed repeatedly while being read",
        path.display()
    ))
}

#[cfg(target_os = "macos")]
fn read_xattr(path: &std::ffi::CStr, name: &std::ffi::CStr) -> std::io::Result<Vec<u8>> {
    use std::ptr;

    for _ in 0..3 {
        let size = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                ptr::null_mut(),
                0,
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if size < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut value = vec![0_u8; size as usize];
        let written = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if written >= 0 && written as usize <= value.len() {
            value.truncate(written as usize);
            return Ok(value);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ERANGE) {
            return Err(error);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "extended attribute changed repeatedly while being read",
    ))
}

#[cfg(target_os = "macos")]
fn capture_acl(path: &Path, is_symlink: bool) -> Result<Vec<u8>, String> {
    use std::ffi::CString;
    use std::os::raw::{c_int, c_void};
    use std::os::unix::ffi::OsStrExt;

    type Acl = *mut c_void;
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;

    #[link(name = "System")]
    extern "C" {
        fn acl_get_link_np(path: *const libc::c_char, kind: c_int) -> Acl;
        fn acl_size(acl: Acl) -> libc::ssize_t;
        fn acl_copy_ext(buffer: *mut c_void, acl: Acl, size: libc::ssize_t) -> libc::ssize_t;
        fn acl_free(object: *mut c_void) -> c_int;
    }

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", path.display()))?;
    let acl = unsafe { acl_get_link_np(c_path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let code = unsafe { *libc::__error() };
        // macOS reports ENOENT when the object has no extended ACL. ACLs are
        // not supported on symlinks, for which it may report EINVAL.
        if code == libc::ENOENT || (is_symlink && code == libc::EINVAL) {
            return Ok(Vec::new());
        }
        return Err(format!(
            "cannot read ACL on {}: {}",
            path.display(),
            std::io::Error::from_raw_os_error(code)
        ));
    }

    struct AclGuard(Acl);
    impl Drop for AclGuard {
        fn drop(&mut self) {
            unsafe {
                acl_free(self.0);
            }
        }
    }
    let _guard = AclGuard(acl);

    let size = unsafe { acl_size(acl) };
    if size < 0 {
        return Err(format!(
            "cannot size ACL on {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    if size == 0 {
        return Ok(Vec::new());
    }
    let mut bytes = vec![0_u8; size as usize];
    let written = unsafe { acl_copy_ext(bytes.as_mut_ptr().cast(), acl, size) };
    if written < 0 {
        return Err(format!(
            "cannot copy ACL on {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    bytes.truncate(written as usize);
    Ok(bytes)
}
