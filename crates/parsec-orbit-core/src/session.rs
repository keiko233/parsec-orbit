//! One loader session: the scan and the modules it mapped.
//!
//! Both loaders used to keep their own list of mapped modules and write their
//! own "ready" line. There is no unload (interface tables point into the
//! mapped images), so the bookkeeping must outlive the scan result; a loader
//! keeps one `Session` for the life of the process.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::{scan, Env, Loaded, Summary};

/// The modules mapped so far, in load order.
#[derive(Default)]
pub struct Session {
    owned: Mutex<Vec<Loaded>>,
}

impl Session {
    pub const fn new() -> Session {
        Session {
            owned: Mutex::new(Vec::new()),
        }
    }

    /// Runs one scan, keeps what it mapped and logs the one-line summary the
    /// end-to-end suite asserts on.
    pub fn scan(&self, env: Env, plugin_dirs: &[PathBuf]) -> Summary {
        let logger = env.logger;
        let result = scan(env, plugin_dirs);
        self.remember(result.loaded);
        logger.info(&format!(
            "ready: {} loaded, {} skipped, {} failed",
            result.summary.loaded, result.summary.skipped, result.summary.failed
        ));
        result.summary
    }

    fn remember(&self, loaded: Vec<Loaded>) {
        match self.owned.lock() {
            Ok(mut owned) => owned.extend(loaded),
            // A poisoned lock cannot be fixed from here; keeping the modules
            // mapped matters more than the bookkeeping.
            Err(_) => std::mem::forget(loaded),
        }
    }

    /// Ids of the plugins mapped so far, in load order.
    pub fn loaded_ids(&self) -> Vec<String> {
        self.owned
            .lock()
            .map(|owned| owned.iter().map(|plugin| plugin.id.clone()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use crate::{Exports, Level, Logger, Module, Platform};
    use std::cell::RefCell;
    use std::path::Path;

    /// Maps any library as one with no exports, so a native plugin "fails".
    struct NoExports;
    impl Platform for NoExports {
        fn load(&self, _path: &Path) -> Result<Module, String> {
            Ok(Module {
                handle: 1,
                exports: Exports::default(),
            })
        }
        fn native_kind(&self) -> &'static str {
            crate::KIND_DLL
        }
    }

    #[derive(Default)]
    struct Recording(RefCell<Vec<String>>);
    impl Logger for Recording {
        fn log(&self, _level: Level, message: &str) {
            self.0.borrow_mut().push(message.to_string());
        }
    }

    #[test]
    fn a_scan_logs_one_ready_line_with_the_counts() {
        let dir = TempDir::new("session-ready");
        dir.write("a/plugin.toml", "id = \"a\"\nentry = \"a.dll\"\n");
        dir.write("a/a.dll", "");
        let logger = Recording::default();
        let session = Session::new();
        let summary = session.scan(
            Env {
                platform: &NoExports,
                logger: &logger,
                loader_version: "0.0.0-test",
                data_root: None,
            },
            &[dir.path().to_path_buf()],
        );
        assert_eq!((summary.loaded, summary.skipped, summary.failed), (0, 0, 1));
        let ready: Vec<String> = logger
            .0
            .borrow()
            .iter()
            .filter(|line| line.starts_with("ready:"))
            .cloned()
            .collect();
        assert_eq!(ready, ["ready: 0 loaded, 0 skipped, 1 failed"]);
        assert!(session.loaded_ids().is_empty());
    }
}
