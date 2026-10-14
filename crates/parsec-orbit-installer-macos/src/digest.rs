//! SHA-256 digests and metadata snapshots used for provenance and restore
//! verification.
//!
//! A [`Tree`] records one entry per file, directory and symlink (relative
//! path, kind, permission bits, size, content hash or link target). Restore
//! and rollback checks compare these snapshots, including the metadata captured
//! by [`MetadataSnapshot`].

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::platform::mode_bits;

#[path = "metadata.rs"]
mod metadata;
pub use metadata::{ExtendedAttribute, FileTimestamp, MetadataSnapshot};

/// SHA-256 of one file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// One entry of a filesystem snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    /// Optional filesystem metadata added after the initial manifest format.
    /// Missing values keep snapshots written by older installer versions
    /// readable and comparable using the fields they recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// Relative path -> entry, sorted by path. The empty path (`""`) represents
/// the root directory itself.
pub type Tree = BTreeMap<String, Entry>;

/// Snapshots a directory tree without following symlinks.
pub fn tree(root: &Path) -> Result<Tree, String> {
    let mut entries = BTreeMap::new();
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("{}: {error}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!("{} is not a real directory", root.display()));
    }
    entries.insert(
        String::new(),
        Entry {
            kind: EntryKind::Dir,
            mode: mode_bits(&root_metadata),
            size: 0,
            metadata: metadata::capture(root, &root_metadata)?,
            sha256: None,
            target: None,
        },
    );
    walk(root, root, &mut entries)?;
    Ok(entries)
}

fn walk(root: &Path, current: &Path, entries: &mut Tree) -> Result<(), String> {
    let listing =
        fs::read_dir(current).map_err(|error| format!("{}: {error}", current.display()))?;
    for item in listing {
        let item = item.map_err(|error| format!("{}: {error}", current.display()))?;
        let path = item.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is not below {}", path.display(), root.display()))?
            .to_string_lossy()
            .into_owned();
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?
                .to_string_lossy()
                .into_owned();
            entries.insert(
                relative,
                Entry {
                    kind: EntryKind::Symlink,
                    mode: 0o777,
                    size: 0,
                    metadata: metadata::capture(&path, &metadata)?,
                    sha256: None,
                    target: Some(target),
                },
            );
        } else if file_type.is_dir() {
            entries.insert(
                relative,
                Entry {
                    kind: EntryKind::Dir,
                    mode: mode_bits(&metadata),
                    size: 0,
                    metadata: metadata::capture(&path, &metadata)?,
                    sha256: None,
                    target: None,
                },
            );
            walk(root, &path, entries)?;
        } else if file_type.is_file() {
            entries.insert(
                relative,
                Entry {
                    kind: EntryKind::File,
                    mode: mode_bits(&metadata),
                    size: metadata.len(),
                    metadata: metadata::capture(&path, &metadata)?,
                    sha256: Some(sha256_file(&path)?),
                    target: None,
                },
            );
        } else {
            return Err(format!(
                "{} is not a file, directory or symlink",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Relative paths whose entries differ, as `path: reason` strings.
pub fn differences(expected: &Tree, actual: &Tree) -> Vec<String> {
    format_changes(changed_paths_impl(
        expected,
        actual,
        MetadataComparison::Exact,
    ))
}

/// Restore comparison preserves every original field. On macOS only a new
/// OS provenance attribute on the root-owned bundle root is retained; callers
/// report this raw-metadata difference rather than removing the OS marker.
pub fn restore_differences(expected: &Tree, actual: &Tree) -> Vec<String> {
    format_changes(changed_paths_impl(
        expected,
        actual,
        MetadataComparison::Restore,
    ))
}

/// Differences between a pristine tree and its macOS `ditto` staging copy.
/// `ditto` resets symlink modification times on macOS, so this comparison
/// permits that copy transformation and transfer to the effective user's
/// uid/gid when copying a root-owned app without privilege. The rename backup
/// retains its exact original ownership; post-signing checks use this copy's
/// validated snapshot rather than exempting owner changes from signing.
pub fn staged_copy_differences(expected: &Tree, actual: &Tree) -> Vec<String> {
    format_changes(changed_paths_impl(
        expected,
        actual,
        MetadataComparison::DittoCopy,
    ))
}

fn format_changes(changes: BTreeMap<String, &'static str>) -> Vec<String> {
    changes
        .into_iter()
        .map(|(path, reason)| format!("{path}: {reason}"))
        .collect()
}

/// Legacy path comparison over the original tree fields. New post-signing
/// checks should use [`changed_paths_for_signing`] so optional metadata on
/// payload files remains part of the allowlist decision.
pub fn changed_paths(expected: &Tree, actual: &Tree) -> BTreeMap<String, &'static str> {
    changed_paths_impl(expected, actual, MetadataComparison::Legacy)
}

/// Compares a signed staged tree against the pristine tree for the existing
/// changed-path allowlist. Symlink mtimes are excluded because `ditto` resets
/// them on macOS. Directory mtimes are excluded only for the explicitly named
/// entries; all other metadata, including payload file mtimes, owners, ACLs
/// and xattrs, remains part of the comparison.
pub fn changed_paths_for_signing(
    expected: &Tree,
    actual: &Tree,
    directory_mtime_exceptions: &[&str],
) -> BTreeMap<String, &'static str> {
    changed_paths_impl(
        expected,
        actual,
        MetadataComparison::Signing(directory_mtime_exceptions),
    )
}

fn changed_paths_impl(
    expected: &Tree,
    actual: &Tree,
    metadata_comparison: MetadataComparison<'_>,
) -> BTreeMap<String, &'static str> {
    let mut report = BTreeMap::new();
    for (path, entry) in expected {
        match actual.get(path) {
            None => {
                report.insert(path.clone(), "missing");
            }
            Some(found) if !entries_match(entry, found, path, metadata_comparison) => {
                report.insert(path.clone(), "differs");
            }
            Some(_) => {}
        }
    }
    for path in actual.keys() {
        // The root entry was added with metadata snapshots. Older manifests
        // omitted it, so its appearance alone must not invalidate those
        // records.
        if !expected.contains_key(path) && !path.is_empty() {
            report.insert(path.clone(), "unexpected");
        }
    }
    report
}

fn entries_match(
    expected: &Entry,
    actual: &Entry,
    path: &str,
    metadata_comparison: MetadataComparison<'_>,
) -> bool {
    expected.kind == actual.kind
        && expected.mode == actual.mode
        && expected.size == actual.size
        && expected.sha256 == actual.sha256
        && expected.target == actual.target
        && metadata_matches(expected, actual, path, metadata_comparison)
}

#[derive(Clone, Copy)]
enum MetadataComparison<'a> {
    Exact,
    Restore,
    DittoCopy,
    Signing(&'a [&'a str]),
    Legacy,
}

fn metadata_matches(
    expected: &Entry,
    actual: &Entry,
    path: &str,
    comparison: MetadataComparison<'_>,
) -> bool {
    if matches!(comparison, MetadataComparison::Legacy) {
        return true;
    }
    let Some(expected_metadata) = expected.metadata.as_ref() else {
        // Old manifests only recorded the pre-metadata fields.
        return true;
    };
    let Some(actual_metadata) = actual.metadata.as_ref() else {
        return false;
    };
    let ignore_mtime = match comparison {
        MetadataComparison::Exact | MetadataComparison::Restore | MetadataComparison::Legacy => {
            false
        }
        MetadataComparison::DittoCopy => expected.kind == EntryKind::Symlink,
        MetadataComparison::Signing(directory_exceptions) => {
            expected.kind == EntryKind::Symlink
                || (expected.kind == EntryKind::Dir && directory_exceptions.contains(&path))
        }
    };
    let same_owner = expected_metadata.uid == actual_metadata.uid
        && expected_metadata.gid == actual_metadata.gid;
    #[cfg(unix)]
    let copied_to_current_user = matches!(comparison, MetadataComparison::DittoCopy)
        && actual_metadata.uid == Some(crate::platform::effective_uid())
        && (actual_metadata.gid == expected_metadata.gid
            || actual_metadata.gid == Some(unsafe { libc::getegid() }));
    #[cfg(not(unix))]
    let copied_to_current_user = false;
    let attributes = if matches!(comparison, MetadataComparison::Restore)
        && !(cfg!(target_os = "macos")
            && path.is_empty()
            && expected.kind == EntryKind::Dir
            && expected_metadata.uid == Some(0))
    {
        MetadataComparison::Exact
    } else {
        comparison
    };
    (same_owner || copied_to_current_user)
        && xattrs_match(expected_metadata, actual_metadata, attributes)
        && expected_metadata.acl == actual_metadata.acl
        && (ignore_mtime || expected_metadata.mtime == actual_metadata.mtime)
}

fn xattrs_match(
    expected: &MetadataSnapshot,
    actual: &MetadataSnapshot,
    comparison: MetadataComparison<'_>,
) -> bool {
    if expected.xattrs == actual.xattrs {
        return true;
    }
    // macOS attaches provenance to new copies of installed apps. Preserve
    // it rather than deleting it. Its addition is a copy exception, and a
    // narrowly scoped root-owned restore exception selected above;
    // changing/removing an existing attribute or any other xattr refuses.
    if !matches!(
        comparison,
        MetadataComparison::DittoCopy | MetadataComparison::Restore
    ) {
        return false;
    }
    let (Some(expected), Some(actual)) = (&expected.xattrs, &actual.xattrs) else {
        return false;
    };
    let provenance = b"com.apple.provenance";
    if expected
        .iter()
        .any(|attribute| attribute.name == provenance)
    {
        return false;
    }
    let filtered: Vec<_> = actual
        .iter()
        .filter(|attribute| attribute.name != provenance)
        .collect();
    expected.iter().eq(filtered)
}

/// Convenience: the canonical path of an existing path.
pub fn canonical(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    #[test]
    fn tree_records_content_and_metadata() {
        let scratch = TempDir::new("installer-digest");
        fs::create_dir(scratch.path().join("sub")).unwrap();
        fs::write(scratch.path().join("sub/a.txt"), b"hello").unwrap();
        let snapshot = tree(scratch.path()).unwrap();
        let file_key = Path::new("sub").join("a.txt");
        let file_key = file_key.to_string_lossy();
        assert_eq!(snapshot["sub"].kind, EntryKind::Dir);
        assert_eq!(snapshot[file_key.as_ref()].kind, EntryKind::File);
        assert_eq!(snapshot[file_key.as_ref()].size, 5);
        assert_eq!(
            snapshot[file_key.as_ref()].sha256.as_deref(),
            Some(
                sha256_file(&scratch.path().join("sub/a.txt"))
                    .unwrap()
                    .as_str()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_can_transfer_ownership_but_restore_and_signing_cannot() {
        let scratch = TempDir::new("installer-staging-owner");
        fs::write(scratch.path().join("payload"), b"unchanged payload").unwrap();
        let copy = tree(scratch.path()).unwrap();
        let mut original = copy.clone();
        for entry in original.values_mut() {
            let metadata = entry.metadata.as_mut().unwrap();
            metadata.uid = Some(crate::platform::effective_uid().wrapping_add(1));
            metadata.gid = Some(unsafe { libc::getegid() }.wrapping_add(1));
        }
        assert!(staged_copy_differences(&original, &copy).is_empty());
        assert!(!differences(&original, &copy).is_empty());
        assert!(!changed_paths_for_signing(&original, &copy, &[""]).is_empty());
        let mut tampered = copy.clone();
        tampered
            .get_mut("payload")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .acl = Some(vec![1]);
        assert!(!staged_copy_differences(&original, &tampered).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn staging_preserves_added_os_provenance_without_exempting_other_attributes() {
        let scratch = TempDir::new("installer-copy-provenance");
        let mut original = tree(scratch.path()).unwrap();
        // Model a source without OS provenance without removing any on disk.
        original
            .get_mut("")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .xattrs
            .as_mut()
            .unwrap()
            .retain(|attribute| attribute.name != b"com.apple.provenance");
        let mut copy = original.clone();
        let metadata = copy.get_mut("").unwrap().metadata.as_mut().unwrap();
        metadata.xattrs.as_mut().unwrap().push(ExtendedAttribute {
            name: b"com.apple.provenance".to_vec(),
            value: vec![1, 2, 3],
        });
        assert!(staged_copy_differences(&original, &copy).is_empty());
        assert!(!differences(&original, &copy).is_empty());
        assert!(!changed_paths_for_signing(&original, &copy, &[""]).is_empty());
        let mut changed = copy.clone();
        changed
            .get_mut("")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .xattrs
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|attribute| attribute.name == b"com.apple.provenance")
            .unwrap()
            .value = vec![4];
        assert!(!staged_copy_differences(&copy, &changed).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn root_restore_keeps_os_provenance_but_rejects_every_other_metadata_change() {
        let scratch = TempDir::new("installer-root-provenance");
        fs::write(scratch.path().join("payload"), b"pristine").unwrap();
        let mut original = tree(scratch.path()).unwrap();
        let root = original.get_mut("").unwrap().metadata.as_mut().unwrap();
        root.uid = Some(0);
        root.xattrs
            .as_mut()
            .unwrap()
            .retain(|a| a.name != b"com.apple.provenance");
        let mut restored = original.clone();
        restored
            .get_mut("")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .xattrs
            .as_mut()
            .unwrap()
            .push(ExtendedAttribute {
                name: b"com.apple.provenance".to_vec(),
                value: vec![1, 2, 3],
            });
        assert!(!differences(&original, &restored).is_empty());
        assert!(restore_differences(&original, &restored).is_empty());
        for name in [b"com.apple.quarantine".to_vec(), b"unrelated".to_vec()] {
            let mut altered = restored.clone();
            altered
                .get_mut("")
                .unwrap()
                .metadata
                .as_mut()
                .unwrap()
                .xattrs
                .as_mut()
                .unwrap()
                .push(ExtendedAttribute {
                    name,
                    value: vec![1],
                });
            assert!(!restore_differences(&original, &altered).is_empty());
        }
        let mut changed = restored.clone();
        changed.get_mut("").unwrap().metadata.as_mut().unwrap().uid = Some(501);
        assert!(!restore_differences(&original, &changed).is_empty());
        let mut changed = restored.clone();
        changed
            .get_mut("")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .mtime = None;
        assert!(!restore_differences(&original, &changed).is_empty());
        let mut changed = restored.clone();
        changed.get_mut("payload").unwrap().sha256 = Some("changed".into());
        assert!(!restore_differences(&original, &changed).is_empty());
        let mut changed = restored.clone();
        changed
            .get_mut("payload")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .xattrs
            .as_mut()
            .unwrap()
            .push(ExtendedAttribute {
                name: b"com.apple.provenance".to_vec(),
                value: vec![1],
            });
        assert!(!restore_differences(&original, &changed).is_empty());
        // An existing OS marker is never exempted from change/removal.
        let mut changed = restored.clone();
        changed
            .get_mut("")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .xattrs
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|a| a.name == b"com.apple.provenance")
            .unwrap()
            .value = vec![9];
        assert!(!restore_differences(&restored, &changed).is_empty());
    }

    #[test]
    fn old_entries_deserialize_and_compare_without_new_metadata() {
        let old: Entry = serde_json::from_value(serde_json::json!({
            "kind": "file",
            "mode": 0o644,
            "size": 3,
            "sha256": "abc",
            "target": null
        }))
        .unwrap();
        assert!(old.metadata.is_none());

        let mut current = old.clone();
        current.metadata = Some(MetadataSnapshot::default());
        let expected = BTreeMap::from([("file".to_string(), old)]);
        let actual = BTreeMap::from([
            (
                String::new(),
                Entry {
                    kind: EntryKind::Dir,
                    mode: 0o755,
                    size: 0,
                    metadata: Some(MetadataSnapshot::default()),
                    sha256: None,
                    target: None,
                },
            ),
            ("file".to_string(), current),
        ]);

        assert!(differences(&expected, &actual).is_empty());
        assert!(changed_paths(&expected, &actual).is_empty());
    }

    #[test]
    fn signing_path_check_ignores_only_named_directory_mtimes() {
        let scratch = TempDir::new("installer-digest-metadata");
        fs::create_dir(scratch.path().join("sub")).unwrap();
        fs::write(scratch.path().join("file"), b"content").unwrap();
        let mut expected = tree(scratch.path()).unwrap();
        // Comparison policy uses explicit timestamps even on hosts that do
        // not capture Unix metadata, rather than requiring Unix filesystem APIs.
        for entry in expected.values_mut() {
            let metadata = entry.metadata.get_or_insert_with(MetadataSnapshot::default);
            metadata.mtime = Some(FileTimestamp {
                seconds: 0,
                nanoseconds: 0,
            });
        }
        let mut actual = expected.clone();
        let changed_time = Some(FileTimestamp {
            seconds: 1,
            nanoseconds: 2,
        });
        actual.get_mut("").unwrap().metadata.as_mut().unwrap().mtime = changed_time;
        actual
            .get_mut("sub")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .mtime = changed_time;
        actual
            .get_mut("file")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .mtime = changed_time;

        assert_eq!(
            differences(&expected, &actual),
            [": differs", "file: differs", "sub: differs"]
        );
        assert!(changed_paths(&expected, &actual).is_empty());
        let signing = changed_paths_for_signing(&expected, &actual, &["", "sub"]);
        assert_eq!(signing, BTreeMap::from([("file".into(), "differs")]));
    }

    #[cfg(unix)]
    #[test]
    fn tree_records_symlink_targets() {
        let scratch = TempDir::new("installer-digest-links");
        fs::create_dir(scratch.path().join("sub")).unwrap();
        fs::write(scratch.path().join("sub/a.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", scratch.path().join("link")).unwrap();
        let snapshot = tree(scratch.path()).unwrap();
        assert_eq!(snapshot["link"].kind, EntryKind::Symlink);
        assert_eq!(snapshot["link"].target.as_deref(), Some("sub/a.txt"));
    }

    #[test]
    fn differences_reports_missing_changed_and_unexpected() {
        let scratch = TempDir::new("installer-digest-diff");
        fs::write(scratch.path().join("a"), b"one").unwrap();
        let before = tree(scratch.path()).unwrap();
        fs::write(scratch.path().join("a"), b"two").unwrap();
        fs::write(scratch.path().join("b"), b"new").unwrap();
        let after = tree(scratch.path()).unwrap();
        let report = differences(&before, &after);
        assert!(report.iter().any(|line| line == "a: differs"), "{report:?}");
        assert!(
            report.iter().any(|line| line == "b: unexpected"),
            "{report:?}"
        );
    }
}
