//! macOS scratch checks for metadata fidelity across `ditto` and rename.
//!
//! Every path lives under a disposable test directory. The test covers the
//! bundle root and nested entries, including a symlink with its own xattr.

#![cfg(target_os = "macos")]

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::Path;
use std::process::Command;

use parsec_orbit_core::testutil::TempDir;
use parsec_orbit_installer::digest;
use parsec_orbit_installer::platform;

const ATTRIBUTE: &str = "org.parsec-orbit.metadata-restore-test";

#[test]
fn ditto_and_rename_preserve_metadata_and_strict_comparison_detects_mutation() {
    let scratch = TempDir::new("installer-metadata-restore");
    let app = scratch.path().join("Parsec.app");
    let resources = app.join("Contents/Resources");
    fs::create_dir_all(&resources).unwrap();
    let payload = resources.join("payload.bin");
    let link = resources.join("payload-link");
    fs::write(&payload, b"metadata fidelity fixture").unwrap();
    symlink("payload.bin", &link).unwrap();

    set_xattr(&app, ATTRIBUTE, b"bundle-root", false);
    set_xattr(&resources, ATTRIBUTE, b"resources-directory", false);
    set_xattr(&payload, ATTRIBUTE, b"regular-file", false);
    set_xattr(&link, ATTRIBUTE, b"symlink-itself", true);
    add_acl(&payload);
    set_mtime(&payload, 1_700_000_001, 123_456_789, false);
    set_mtime(&link, 1_700_000_002, 987_654_321, true);
    set_mtime(&resources, 1_700_000_003, 222_333_444, false);
    set_mtime(&app, 1_700_000_004, 555_666_777, false);

    let pristine = digest::tree(&app).unwrap();
    assert!(
        pristine.contains_key(""),
        "the bundle root is not snapshotted"
    );
    assert_eq!(
        attr_value(
            &pristine["Contents/Resources/payload-link"].metadata,
            ATTRIBUTE
        ),
        Some(b"symlink-itself".as_slice()),
        "the symlink snapshot followed its target"
    );
    assert!(
        pristine["Contents/Resources/payload.bin"]
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.acl.as_ref())
            .is_some_and(|acl| !acl.is_empty()),
        "the fixture ACL was not captured"
    );

    let staged = scratch.path().join("staged.app");
    platform::ditto(&app, &staged).unwrap();
    let staged_tree = digest::tree(&staged).unwrap();
    let symlink_path = "Contents/Resources/payload-link";
    let symlink_mtime_changed = pristine[symlink_path].metadata.as_ref().unwrap().mtime
        != staged_tree[symlink_path].metadata.as_ref().unwrap().mtime;
    let exact_copy_differences = digest::differences(&pristine, &staged_tree);
    if symlink_mtime_changed {
        assert_eq!(
            exact_copy_differences,
            [format!("{symlink_path}: differs")],
            "ditto changed metadata beyond the symlink mtime: {exact_copy_differences:?}"
        );
    } else {
        assert!(
            exact_copy_differences.is_empty(),
            "ditto changed metadata unexpectedly: {exact_copy_differences:?}"
        );
    }
    assert!(
        digest::staged_copy_differences(&pristine, &staged_tree).is_empty(),
        "ditto changed metadata beyond the known symlink mtime limitation: {:?}",
        digest::staged_copy_differences(&pristine, &staged_tree)
    );

    let backup = scratch.path().join("pristine.app");
    fs::rename(&app, &backup).unwrap();
    fs::rename(&backup, &app).unwrap();
    let renamed_tree = digest::tree(&app).unwrap();
    assert!(
        digest::differences(&pristine, &renamed_tree).is_empty(),
        "rename roundtrip changed the pristine tree: {:?}",
        digest::differences(&pristine, &renamed_tree)
    );

    let mutated_payload = staged.join("Contents/Resources/payload.bin");
    set_xattr(&mutated_payload, ATTRIBUTE, b"changed-payload-xattr", false);
    let changed_payload_tree = digest::tree(&staged).unwrap();
    let signing_changes = digest::changed_paths_for_signing(
        &staged_tree,
        &changed_payload_tree,
        &["", "Contents", "Contents/Resources"],
    );
    assert_eq!(
        signing_changes.get("Contents/Resources/payload.bin"),
        Some(&"differs"),
        "signing comparison missed a payload xattr mutation: {signing_changes:?}"
    );

    let mutated = staged.join(symlink_path);
    set_xattr(&mutated, ATTRIBUTE, b"changed-symlink-xattr", true);
    let changed_tree = digest::tree(&staged).unwrap();
    assert!(
        digest::differences(&staged_tree, &changed_tree)
            .iter()
            .any(|difference| difference == &format!("{symlink_path}: differs")),
        "strict restore comparison missed a symlink xattr change"
    );
    assert!(
        digest::changed_paths_for_signing(
            &staged_tree,
            &changed_tree,
            &["", "Contents", "Contents/Resources"],
        )
        .get(symlink_path)
            == Some(&"differs"),
        "the signing allowlist comparison missed a symlink xattr change"
    );
}

fn set_xattr(path: &Path, name: &str, value: &[u8], nofollow: bool) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new(name).unwrap();
    let flags = if nofollow { libc::XATTR_NOFOLLOW } else { 0 };
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            flags,
        )
    };
    assert_eq!(
        result,
        0,
        "setxattr failed: {}",
        std::io::Error::last_os_error()
    );
}

fn set_mtime(path: &Path, seconds: i64, nanoseconds: i64, nofollow: bool) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let path_c = CString::new(path.as_os_str().as_bytes()).unwrap();
    let times = [
        libc::timespec {
            tv_sec: metadata.atime() as _,
            tv_nsec: metadata.atime_nsec() as _,
        },
        libc::timespec {
            tv_sec: seconds as _,
            tv_nsec: nanoseconds as _,
        },
    ];
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    let result = unsafe { libc::utimensat(libc::AT_FDCWD, path_c.as_ptr(), times.as_ptr(), flags) };
    assert_eq!(
        result,
        0,
        "utimensat failed: {}",
        std::io::Error::last_os_error()
    );
}

fn add_acl(path: &Path) {
    let output = Command::new("/bin/chmod")
        .args([
            "+a",
            "everyone allow read,readattr,readextattr,readsecurity",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "chmod could not create the ACL fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn attr_value<'a>(metadata: &'a Option<digest::MetadataSnapshot>, name: &str) -> Option<&'a [u8]> {
    metadata
        .as_ref()?
        .xattrs
        .as_ref()?
        .iter()
        .find(|attribute| attribute.name == name.as_bytes())
        .map(|attribute| attribute.value.as_slice())
}
