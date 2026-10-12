//! Starting JavaScript plugins and native contexts.
//!
//! The process-wide bookkeeping of the runtime: which plugin has which
//! context, and the loader-facing [`Context`] the contexts log through.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use parsec_orbit_plugin_sdk_std::services::WebViewClient;
use parsec_orbit_plugin_sdk_std::{Context, Level};

use crate::engine::{self, Handle, PluginInfo, Spec};

struct Runtime {
    cx: Context,
    plugins: Mutex<HashMap<String, Handle>>,
    natives: Mutex<HashMap<i64, (String, Handle)>>,
    next_native: AtomicI64,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Remembers the runtime plugin's own context. Called from `init`.
pub fn configure(cx: Context) {
    let _ = RUNTIME.set(Runtime {
        cx,
        plugins: Mutex::new(HashMap::new()),
        natives: Mutex::new(HashMap::new()),
        next_native: AtomicI64::new(0),
    });
}

fn runtime() -> Option<&'static Runtime> {
    RUNTIME.get()
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A logger that tags lines with the runtime's id and the name of the context.
fn logger(cx: Context, name: &str) -> engine::Logger {
    let name = name.to_string();
    Arc::new(move |level: Level, message: &str| {
        cx.log(level, format_args!("{name}: {message}"));
    })
}

/// Logs why a plugin failed to load, under the runtime's id.
pub fn report(id: &str, error: &str) {
    if let Some(runtime) = runtime() {
        runtime.cx.log(Level::Error, format_args!("{id}: {error}"));
    }
}

/// Logs why an evaluation failed.
pub fn report_eval(context: i64, error: &str) {
    if let Some(runtime) = runtime() {
        runtime
            .cx
            .log(Level::Debug, format_args!("context {context}: {error}"));
    }
}

/// The parts of `plugin.toml` the runtime reads itself.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub permissions: Vec<String>,
}

pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let document = toml::de::DeTable::parse(text).map_err(|error| error.to_string())?;
    let table = document.get_ref();
    let string = |key: &str| {
        table
            .get(key)
            .and_then(|value| value.get_ref().as_str())
            .map(str::to_string)
    };
    let permissions = table
        .get("permissions")
        .and_then(|value| value.get_ref().as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get_ref().as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Manifest {
        version: string("version").unwrap_or_else(|| "-".to_string()),
        permissions,
    })
}

/// What the loader hands to `load_plugin`.
pub struct LoadRequest {
    pub id: String,
    pub dir: PathBuf,
    pub entry: PathBuf,
    pub data_dir: Option<PathBuf>,
    pub manifest: String,
}

/// Starts the plugin's context and waits for its `setup`.
pub fn load_plugin(request: LoadRequest) -> Result<(), String> {
    let runtime = runtime().ok_or("the runtime is not initialised")?;
    let manifest = parse_manifest(&request.manifest)?;
    let known = ["webview", "config"];
    for permission in &manifest.permissions {
        if !known.contains(&permission.as_str()) {
            runtime.cx.log(
                Level::Warn,
                format_args!(
                    "{}: unknown permission \"{permission}\" ignored",
                    request.id
                ),
            );
        }
    }

    let spec = Spec {
        name: format!("js:{}", request.id),
        plugin: Some(PluginInfo {
            id: request.id.clone(),
            version: manifest.version,
            dir: request.dir,
            data_dir: request.data_dir,
            entry: request.entry,
            permissions: manifest.permissions,
        }),
        log: logger(runtime.cx, &request.id),
        webview: WebViewClient::new(&runtime.cx),
    };
    if locked(&runtime.plugins).contains_key(&request.id) {
        return Err("a plugin with this id is already running".to_string());
    }
    let handle =
        engine::spawn(spec).map_err(|error| format!("could not start a thread: {error}"))?;
    match handle.load() {
        Ok(()) => {
            locked(&runtime.plugins).insert(request.id, handle);
            Ok(())
        }
        Err(error) => {
            handle.stop();
            Err(error)
        }
    }
}

/// Creates a context for a native plugin.
pub fn create_context(name: &str) -> Result<i64, i32> {
    let runtime = runtime().ok_or(parsec_orbit_plugin_sdk_std::ERR_NOT_READY)?;
    let mut natives = locked(&runtime.natives);
    if natives.values().any(|(existing, _)| existing == name) {
        return Err(parsec_orbit_plugin_sdk_std::ERR_EXISTS);
    }
    let spec = Spec {
        name: format!("js-context:{name}"),
        plugin: None,
        log: logger(runtime.cx, name),
        webview: None,
    };
    let handle = engine::spawn(spec).map_err(|_| parsec_orbit_plugin_sdk_std::ERR_NOT_READY)?;
    let id = runtime.next_native.fetch_add(1, Ordering::SeqCst) + 1;
    natives.insert(id, (name.to_string(), handle));
    Ok(id)
}

/// The context made by `create_context`.
pub fn native_context(id: i64) -> Option<Handle> {
    let runtime = runtime()?;
    locked(&runtime.natives)
        .get(&id)
        .map(|(_, handle)| handle.clone())
}

pub fn destroy_context(id: i64) -> bool {
    let Some(runtime) = runtime() else {
        return false;
    };
    match locked(&runtime.natives).remove(&id) {
        Some((_, handle)) => {
            handle.stop();
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_gives_version_and_permissions() {
        let manifest = parse_manifest(
            "id = \"x\"\nversion = \"1.2.3\"\nkind = \"js\"\npermissions = [\"webview\", \"config\", 5]\n",
        )
        .unwrap();
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(manifest.permissions, ["webview", "config"]);
    }

    #[test]
    fn a_manifest_without_them_means_no_permissions() {
        let manifest = parse_manifest("id = \"x\"\n").unwrap();
        assert_eq!(manifest.version, "-");
        assert!(manifest.permissions.is_empty());
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_an_error() {
        assert!(parse_manifest("id = ").is_err());
    }
}
