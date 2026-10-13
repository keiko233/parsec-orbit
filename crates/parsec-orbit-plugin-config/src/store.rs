//! The settings themselves: one JSON object per owner, cached, written back.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use parsec_orbit_plugin_sdk_std::{ERR_INVALID, ERR_NOT_READY};
use serde_json::{Map, Value};

/// Longest key, in bytes.
pub const MAX_KEY: usize = 256;
/// Largest value, as JSON text.
pub const MAX_VALUE: usize = 1024 * 1024;
/// Most keys one owner may have.
pub const MAX_KEYS: usize = 1024;

type Owners = HashMap<String, Map<String, Value>>;

pub struct Store {
    /// `None`: nothing is written to disk.
    root: Option<PathBuf>,
    owners: Mutex<Owners>,
}

/// The file name of an owner: its characters outside `[A-Za-z0-9._-]` are
/// replaced, so an owner can never point out of the directory.
pub fn file_name(owner: &str) -> String {
    let mut name: String = owner
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        name.insert(0, '_');
    }
    format!("{name}.json")
}

impl Store {
    pub fn new(root: Option<PathBuf>) -> Store {
        Store {
            root,
            owners: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Owners> {
        self.owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn path(&self, owner: &str) -> Option<PathBuf> {
        self.root.as_ref().map(|root| root.join(file_name(owner)))
    }

    /// The object of `owner`, loaded on first use. A file that cannot be
    /// read as an object is an error rather than an empty object: writing
    /// would destroy it.
    fn load<'a>(
        &self,
        owners: &'a mut Owners,
        owner: &str,
    ) -> Result<&'a mut Map<String, Value>, i32> {
        if owner.is_empty() {
            return Err(ERR_INVALID);
        }
        if !owners.contains_key(owner) {
            let map = match self.path(owner) {
                Some(path) => read(&path)?,
                None => Map::new(),
            };
            owners.insert(owner.to_string(), map);
        }
        Ok(owners.get_mut(owner).expect("just inserted"))
    }

    fn save(&self, owner: &str, map: &Map<String, Value>) -> Result<(), i32> {
        let Some(path) = self.path(owner) else {
            return Ok(());
        };
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, Value::Object(map.clone()).to_string())?;
            std::fs::rename(&temporary, &path)
        };
        write().map_err(|_| ERR_NOT_READY)
    }

    /// The JSON text of a value.
    pub fn get(&self, owner: &str, key: &str) -> Result<Option<String>, i32> {
        let mut owners = self.lock();
        let map = self.load(&mut owners, owner)?;
        Ok(map.get(key).map(Value::to_string))
    }

    pub fn set(&self, owner: &str, key: &str, json: &str) -> Result<(), i32> {
        if key.is_empty() || key.len() > MAX_KEY || json.len() > MAX_VALUE {
            return Err(ERR_INVALID);
        }
        let value: Value = serde_json::from_str(json).map_err(|_| ERR_INVALID)?;
        let mut owners = self.lock();
        let map = self.load(&mut owners, owner)?;
        if !map.contains_key(key) && map.len() >= MAX_KEYS {
            return Err(ERR_INVALID);
        }
        let previous = map.insert(key.to_string(), value);
        let saved = self.save(owner, map);
        if saved.is_err() {
            // Keep memory and disk telling the same story.
            match previous {
                Some(old) => map.insert(key.to_string(), old),
                None => map.remove(key),
            };
        }
        saved
    }

    /// Whether the key existed.
    pub fn remove(&self, owner: &str, key: &str) -> Result<bool, i32> {
        let mut owners = self.lock();
        let map = self.load(&mut owners, owner)?;
        let Some(old) = map.remove(key) else {
            return Ok(false);
        };
        if let Err(code) = self.save(owner, map) {
            map.insert(key.to_string(), old);
            return Err(code);
        }
        Ok(true)
    }

    pub fn keys(&self, owner: &str) -> Result<Vec<String>, i32> {
        let mut owners = self.lock();
        let map = self.load(&mut owners, owner)?;
        Ok(map.keys().cloned().collect())
    }
}

fn read(path: &Path) -> Result<Map<String, Value>, i32> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(ERR_INVALID),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(_) => Err(ERR_NOT_READY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(label: &str) -> Dir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-config-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            Dir(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn values_round_trip_as_json() {
        let store = Store::new(None);
        assert_eq!(store.get("a", "k").unwrap(), None);
        store.set("a", "k", "{\"list\": [1, 2, \"x\"]}").unwrap();
        assert_eq!(
            store.get("a", "k").unwrap().unwrap(),
            "{\"list\":[1,2,\"x\"]}"
        );
        store.set("a", "k", "null").unwrap();
        assert_eq!(
            store.get("a", "k").unwrap().as_deref(),
            Some("null"),
            "null is a value"
        );
        assert_eq!(store.keys("a").unwrap(), ["k"]);
        assert!(store.remove("a", "k").unwrap());
        assert!(!store.remove("a", "k").unwrap());
        assert!(store.keys("a").unwrap().is_empty());
    }

    #[test]
    fn owners_do_not_see_each_other() {
        let store = Store::new(None);
        store.set("a", "k", "1").unwrap();
        store.set("b", "k", "2").unwrap();
        assert_eq!(store.get("a", "k").unwrap().as_deref(), Some("1"));
        assert_eq!(store.get("b", "k").unwrap().as_deref(), Some("2"));
        assert_eq!(store.get("c", "k").unwrap(), None);
    }

    #[test]
    fn settings_survive_a_restart() {
        let dir = Dir::new("persist");
        {
            let store = Store::new(Some(dir.0.clone()));
            store.set("com.example.p", "answer", "42").unwrap();
            store.set("com.example.p", "name", "\"orbit\"").unwrap();
            store.remove("com.example.p", "name").unwrap();
        }
        let store = Store::new(Some(dir.0.clone()));
        assert_eq!(
            store.get("com.example.p", "answer").unwrap().as_deref(),
            Some("42")
        );
        assert_eq!(store.get("com.example.p", "name").unwrap(), None);
        assert_eq!(
            std::fs::read_to_string(dir.0.join("com.example.p.json")).unwrap(),
            "{\"answer\":42}"
        );
        assert!(
            !dir.0.join("com.example.p.json.tmp").exists(),
            "no temporary file is left"
        );
    }

    #[test]
    fn owners_cannot_escape_the_directory() {
        assert_eq!(file_name("com.example.p"), "com.example.p.json");
        assert_eq!(file_name("..\\..\\evil"), ".._.._evil.json");
        assert_eq!(file_name("a/b:c"), "a_b_c.json");
        assert_eq!(file_name(".."), "_...json");
        assert_eq!(file_name(""), "_.json");
        let dir = Dir::new("escape");
        let store = Store::new(Some(dir.0.clone()));
        store.set("../../outside", "k", "1").unwrap();
        assert!(dir.0.join(".._.._outside.json").is_file());
        assert!(!dir.0.parent().unwrap().join("outside.json").exists());
    }

    #[test]
    fn bad_input_is_refused() {
        let store = Store::new(None);
        assert_eq!(store.set("a", "", "1"), Err(ERR_INVALID));
        assert_eq!(
            store.set("a", &"k".repeat(MAX_KEY + 1), "1"),
            Err(ERR_INVALID)
        );
        assert_eq!(store.set("a", "k", "{not json"), Err(ERR_INVALID));
        assert_eq!(store.set("a", "k", ""), Err(ERR_INVALID));
        assert_eq!(
            store.set("a", "k", &format!("\"{}\"", "x".repeat(MAX_VALUE))),
            Err(ERR_INVALID)
        );
        assert_eq!(store.set("", "k", "1"), Err(ERR_INVALID));
        assert_eq!(store.get("", "k"), Err(ERR_INVALID));
        for index in 0..MAX_KEYS {
            store.set("full", &format!("k{index}"), "1").unwrap();
        }
        assert_eq!(store.set("full", "one-too-many", "1"), Err(ERR_INVALID));
        store.set("full", "k0", "2").unwrap();
    }

    #[test]
    fn a_damaged_file_is_reported_not_overwritten() {
        let dir = Dir::new("damaged");
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join("a.json"), "not json").unwrap();
        std::fs::write(dir.0.join("list.json"), "[1,2]").unwrap();
        let store = Store::new(Some(dir.0.clone()));
        assert_eq!(store.get("a", "k"), Err(ERR_INVALID));
        assert_eq!(store.set("a", "k", "1"), Err(ERR_INVALID));
        assert_eq!(store.get("list", "k"), Err(ERR_INVALID));
        assert_eq!(
            std::fs::read_to_string(dir.0.join("a.json")).unwrap(),
            "not json"
        );
    }

    #[test]
    fn a_failed_write_leaves_memory_as_it_was() {
        // The write cannot land where it should: its folder does not exist,
        // and its path is a file. Both steps of a save (the temporary file
        // and the rename) fail there on every platform.
        let dir = Dir::new("readonly");
        let blocker = dir.0.clone();
        std::fs::create_dir_all(&blocker).unwrap();
        let store = Store::new(Some(blocker.clone()));
        assert_eq!(store.get("a", "k").unwrap(), None);
        std::fs::remove_dir(&blocker).unwrap();
        std::fs::write(&blocker, "x").unwrap();
        assert_eq!(store.set("a", "k", "1"), Err(ERR_NOT_READY));
        // The original expectation remains: no file appears where the map
        // would have been written, and memory has nothing to remember.
        assert!(std::fs::read_to_string(blocker.join(file_name("a"))).is_err());
    }
}
