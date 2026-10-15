//! Updates staged for the next start.
//!
//! Windows will not replace a mapped DLL or rename its folder, so a plugin
//! that is already loaded cannot be updated by a program running inside the
//! client (the market plugin). Such an update is written to
//! `<Orbit root>/pending/<id>/` instead, and the loader calls [`promote`]
//! before it maps anything: the staged folder takes the place of the installed
//! one. Nothing is loaded yet at that point, so the rename cannot collide with
//! a mapped image.
//!
//! The old folder is set aside as `<id>.orbit-old` and deleted afterwards. If
//! the deletion fails (a scanner holds a file), the leftover is removed by the
//! next call. If the swap fails half-way, the old folder is put back; a failed
//! promotion is logged and the installed version loads as before.

use std::fs;
use std::path::Path;

use crate::host::valid_plugin_id;
use crate::{Logger, MANIFEST};

/// Suffix of a folder that was replaced and is waiting to be deleted.
pub const OLD_SUFFIX: &str = ".orbit-old";

/// Moves every staged plugin folder of `pending_dir` into `plugins_dir`,
/// replacing the installed folder of the same id. Returns how many were
/// promoted. A missing `pending_dir` is not an error.
///
/// Only folders named by a valid plugin id that hold a `plugin.toml` are
/// taken; anything else (including names starting with `.`, which a writer
/// uses while it is still copying) is left alone.
pub fn promote(log: &dyn Logger, plugins_dir: &Path, pending_dir: &Path) -> usize {
    remove_leftovers(log, plugins_dir);

    let Ok(entries) = fs::read_dir(pending_dir) else {
        return 0;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();

    let mut promoted = 0;
    for id in names {
        if id.starts_with('.') || !valid_plugin_id(&id) {
            continue;
        }
        let staged = pending_dir.join(&id);
        if !staged.join(MANIFEST).is_file() {
            log.warn(&format!(
                "pending update {id}: no {MANIFEST} in {}, left alone",
                staged.display()
            ));
            continue;
        }
        match swap_in(plugins_dir, &id, &staged) {
            Ok(()) => {
                log.info(&format!("pending update of {id} is now installed"));
                promoted += 1;
            }
            Err(error) => log.warn(&format!(
                "pending update of {id} could not be installed ({error}); the installed version stays"
            )),
        }
    }
    promoted
}

fn swap_in(plugins_dir: &Path, id: &str, staged: &Path) -> Result<(), String> {
    fs::create_dir_all(plugins_dir)
        .map_err(|error| format!("cannot create {}: {error}", plugins_dir.display()))?;
    let target = plugins_dir.join(id);
    let aside = plugins_dir.join(format!("{id}{OLD_SUFFIX}"));

    let replaced = target.exists();
    if replaced {
        if aside.exists() {
            fs::remove_dir_all(&aside)
                .map_err(|error| format!("cannot clear {}: {error}", aside.display()))?;
        }
        fs::rename(&target, &aside)
            .map_err(|error| format!("cannot set aside {}: {error}", target.display()))?;
    }
    if let Err(error) = fs::rename(staged, &target) {
        if replaced {
            // Put the installed version back; if even that fails the text says so.
            if let Err(restore) = fs::rename(&aside, &target) {
                return Err(format!(
                    "cannot move the update in ({error}) and cannot restore the old folder ({restore}), it is at {}",
                    aside.display()
                ));
            }
        }
        return Err(format!("cannot move the update in: {error}"));
    }
    // Best effort: a leftover is removed by the next call.
    let _ = fs::remove_dir_all(&aside);
    Ok(())
}

/// Deletes `<id>.orbit-old` folders a previous promotion could not remove.
fn remove_leftovers(log: &dyn Logger, plugins_dir: &Path) {
    let Ok(entries) = fs::read_dir(plugins_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_suffix(OLD_SUFFIX) else {
            continue;
        };
        if !valid_plugin_id(id) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(entry.path()) {
            log.debug(&format!("could not remove {name} yet: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use crate::Level;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recording(RefCell<Vec<String>>);
    impl Logger for Recording {
        fn log(&self, _level: Level, message: &str) {
            self.0.borrow_mut().push(message.to_string());
        }
    }
    impl Recording {
        fn mentions(&self, text: &str) -> bool {
            self.0.borrow().iter().any(|line| line.contains(text))
        }
    }

    fn manifest(version: &str) -> String {
        format!("id = \"com.example.p\"\nversion = \"{version}\"\n")
    }

    #[test]
    fn a_staged_new_plugin_is_installed() {
        let dir = TempDir::new("staging-new");
        dir.write("pending/com.example.p/plugin.toml", &manifest("2"));
        dir.write("pending/com.example.p/p.dll", "new");
        let log = Recording::default();

        let promoted = promote(
            &log,
            &dir.path().join("plugins"),
            &dir.path().join("pending"),
        );

        assert_eq!(promoted, 1);
        let installed = dir.path().join("plugins/com.example.p");
        assert_eq!(fs::read_to_string(installed.join("p.dll")).unwrap(), "new");
        assert!(!dir.path().join("pending/com.example.p").exists());
        assert!(log.mentions("com.example.p is now installed"));
    }

    #[test]
    fn a_staged_update_replaces_the_installed_folder_completely() {
        let dir = TempDir::new("staging-replace");
        dir.write("plugins/com.example.p/plugin.toml", &manifest("1"));
        dir.write(
            "plugins/com.example.p/old-only.txt",
            "gone after the update",
        );
        dir.write("pending/com.example.p/plugin.toml", &manifest("2"));
        let log = Recording::default();

        assert_eq!(
            promote(
                &log,
                &dir.path().join("plugins"),
                &dir.path().join("pending")
            ),
            1
        );

        let installed = dir.path().join("plugins/com.example.p");
        assert!(fs::read_to_string(installed.join("plugin.toml"))
            .unwrap()
            .contains("version = \"2\""));
        assert!(!installed.join("old-only.txt").exists());
        assert!(
            !dir.path()
                .join(format!("plugins/com.example.p{OLD_SUFFIX}"))
                .exists(),
            "the replaced folder is deleted"
        );
    }

    #[test]
    fn unfinished_and_foreign_entries_are_left_alone() {
        let dir = TempDir::new("staging-ignored");
        dir.write("pending/.com.example.p.partial/plugin.toml", &manifest("2"));
        dir.write("pending/not a plugin id/plugin.toml", &manifest("2"));
        dir.write("pending/com.example.nomanifest/readme.txt", "x");
        dir.write("pending/loose-file.txt", "x");
        let log = Recording::default();

        assert_eq!(
            promote(
                &log,
                &dir.path().join("plugins"),
                &dir.path().join("pending")
            ),
            0
        );

        assert!(dir.path().join("pending/.com.example.p.partial").exists());
        assert!(dir.path().join("pending/com.example.nomanifest").exists());
        assert!(log.mentions("com.example.nomanifest: no plugin.toml"));
        assert!(
            !dir.path().join("plugins").exists()
                || fs::read_dir(dir.path().join("plugins"))
                    .unwrap()
                    .next()
                    .is_none()
        );
    }

    #[test]
    fn a_missing_pending_directory_is_not_an_error() {
        let dir = TempDir::new("staging-none");
        let log = Recording::default();
        assert_eq!(
            promote(
                &log,
                &dir.path().join("plugins"),
                &dir.path().join("pending")
            ),
            0
        );
        assert!(log.0.borrow().is_empty());
    }

    #[test]
    fn leftovers_of_an_earlier_promotion_are_removed() {
        let dir = TempDir::new("staging-leftover");
        dir.write(
            &format!("plugins/com.example.p{OLD_SUFFIX}/plugin.toml"),
            &manifest("1"),
        );
        // A folder that merely looks similar is not ours.
        dir.write("plugins/com.example.q.old/plugin.toml", &manifest("1"));
        let log = Recording::default();

        promote(
            &log,
            &dir.path().join("plugins"),
            &dir.path().join("pending"),
        );

        assert!(!dir
            .path()
            .join(format!("plugins/com.example.p{OLD_SUFFIX}"))
            .exists());
        assert!(dir.path().join("plugins/com.example.q.old").exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_installed_file_keeps_the_old_version_and_pending_update() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = TempDir::new("staging-locked");
        dir.write("plugins/com.example.p/plugin.toml", &manifest("1"));
        dir.write("pending/com.example.p/plugin.toml", &manifest("2"));
        let installed = dir.path().join("plugins/com.example.p/plugin.toml");
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&installed)
            .unwrap();
        let log = Recording::default();

        assert_eq!(
            promote(
                &log,
                &dir.path().join("plugins"),
                &dir.path().join("pending")
            ),
            0
        );
        assert!(dir
            .path()
            .join("pending/com.example.p/plugin.toml")
            .is_file());
        assert!(log.mentions("the installed version stays"));
        drop(lock);
        assert!(fs::read_to_string(installed)
            .unwrap()
            .contains("version = \"1\""));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_swap_keeps_the_installed_version() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("staging-readonly");
        dir.write("plugins/com.example.p/plugin.toml", &manifest("1"));
        dir.write("pending/com.example.p/plugin.toml", &manifest("2"));
        let plugins = dir.path().join("plugins");
        // A read-only parent cannot have entries renamed; root ignores that.
        fs::set_permissions(&plugins, fs::Permissions::from_mode(0o555)).unwrap();
        let can_rename = fs::rename(plugins.join("com.example.p"), plugins.join("com.example.p"))
            .is_ok()
            && fs::create_dir(plugins.join("probe")).is_ok();
        let log = Recording::default();

        let promoted = promote(&log, &plugins, &dir.path().join("pending"));

        fs::set_permissions(&plugins, fs::Permissions::from_mode(0o755)).unwrap();
        if can_rename {
            return; // running as a user the permission bits do not bind
        }
        assert_eq!(promoted, 0);
        assert!(
            fs::read_to_string(plugins.join("com.example.p/plugin.toml"))
                .unwrap()
                .contains("version = \"1\"")
        );
        assert!(dir.path().join("pending/com.example.p").exists());
        assert!(log.mentions("the installed version stays"));
    }
}
