//! Plugins of a kind the loader does not map itself.
//!
//! `kind = "js"` (or any kind other than `dll`) is served by the plugin that
//! registered `parsec-orbit.kind.<kind>`. The loader finds that handler in the
//! interface registry and hands it a [`ParsecOrbitPluginLoadInfo`].

use std::path::Path;

use parsec_orbit_plugin_abi::{
    ParsecOrbitKindHandlerV1, ParsecOrbitPluginLoadInfo, INTERFACE_KIND_PREFIX,
};

use crate::host::{self, leak_c_string};

/// Outcome of looking for the handler of a kind.
pub enum Lookup {
    /// Nobody registered `parsec-orbit.kind.<kind>`.
    Missing,
    /// Something is registered under that name but cannot serve this loader.
    Unusable(String),
    Found(&'static ParsecOrbitKindHandlerV1),
}

/// Interface name of the handler for `kind`.
pub fn interface_name(kind: &str) -> String {
    format!("{INTERFACE_KIND_PREFIX}{kind}")
}

/// Finds the handler registered for `kind` and checks that it is a version
/// 1 table with a `load_plugin` function.
pub fn handler_for(kind: &str) -> Lookup {
    let name = interface_name(kind);
    let table = host::find(&name);
    if table.is_null() {
        return Lookup::Missing;
    }
    if (table as usize) % std::mem::align_of::<ParsecOrbitKindHandlerV1>() != 0 {
        return Lookup::Unusable(format!("{name} is misaligned for a kind handler"));
    }
    // Safety: registered tables start with a header and live for the whole
    // process.
    let header = unsafe { *table };
    if header.major() != 1 {
        return Lookup::Unusable(format!(
            "{name} is version {}.{}, this loader serves 1.x",
            header.major(),
            header.minor()
        ));
    }
    let end = std::mem::offset_of!(ParsecOrbitKindHandlerV1, load_plugin)
        + std::mem::size_of::<Option<unsafe extern "system" fn()>>();
    if !header.covers(end) {
        return Lookup::Unusable(format!(
            "{name} is only {} bytes, too small for a kind handler",
            header.size
        ));
    }
    // Safety: one lookup is used for both the validation and the cast, and the
    // registry guarantees the table outlives this reference.
    let handler = unsafe { &*table.cast::<ParsecOrbitKindHandlerV1>() };
    if handler.load_plugin.is_none() {
        return Lookup::Unusable(format!("{name} has no load_plugin"));
    }
    Lookup::Found(handler)
}

/// What a handler is asked to load.
pub struct LoadRequest<'a> {
    pub id: &'a str,
    pub dir: &'a Path,
    pub entry: &'a Path,
    pub data_dir: Option<&'a Path>,
    /// The manifest text, verbatim.
    pub manifest: &'a str,
}

/// Calls `load_plugin`. A non-zero result is an error; the handler logs the
/// reason itself.
pub fn load(handler: &ParsecOrbitKindHandlerV1, request: &LoadRequest) -> Result<(), String> {
    let Some(load_plugin) = handler.load_plugin else {
        return Err("kind handler has no load_plugin".to_string());
    };
    let absolute = |path: &Path| {
        std::path::absolute(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    // The strings only need to outlive the call, but a handler may keep the
    // pointers, so they are leaked like the host tables' strings. The manifest
    // is owned by the candidate and would otherwise be freed when `scan`
    // returns, so it is copied and leaked too.
    let manifest = Box::leak(request.manifest.as_bytes().to_vec().into_boxed_slice());
    let info = ParsecOrbitPluginLoadInfo {
        size: std::mem::size_of::<ParsecOrbitPluginLoadInfo>() as u32,
        id: leak_c_string(request.id),
        dir: leak_c_string(&absolute(request.dir)),
        entry: leak_c_string(&absolute(request.entry)),
        data_dir: request
            .data_dir
            .map_or(std::ptr::null(), |dir| leak_c_string(&absolute(dir))),
        manifest: manifest.as_ptr(),
        manifest_len: request.manifest.len(),
    };
    match unsafe { load_plugin(&info) } {
        0 => Ok(()),
        code => Err(format!("kind handler returned {code}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::register;
    use parsec_orbit_plugin_abi::ParsecOrbitInterfaceHeader;
    use std::ffi::CStr;
    use std::sync::Mutex;

    static LOADED: Mutex<Vec<(String, String, String, Option<String>, String)>> =
        Mutex::new(Vec::new());

    unsafe extern "system" fn record(info: *const ParsecOrbitPluginLoadInfo) -> i32 {
        let info = unsafe { &*info };
        assert_eq!(
            info.size as usize,
            std::mem::size_of::<ParsecOrbitPluginLoadInfo>()
        );
        let text = |p: *const u8| {
            unsafe { CStr::from_ptr(p as *const _) }
                .to_string_lossy()
                .into_owned()
        };
        let manifest = unsafe { std::slice::from_raw_parts(info.manifest, info.manifest_len) };
        let id = text(info.id);
        LOADED.lock().unwrap().push((
            id.clone(),
            text(info.dir),
            text(info.entry),
            (!info.data_dir.is_null()).then(|| text(info.data_dir)),
            String::from_utf8_lossy(manifest).into_owned(),
        ));
        if id.ends_with(".fails") {
            9
        } else {
            0
        }
    }

    static GOOD: ParsecOrbitKindHandlerV1 = ParsecOrbitKindHandlerV1 {
        header: ParsecOrbitInterfaceHeader::new(
            std::mem::size_of::<ParsecOrbitKindHandlerV1>(),
            1,
            0,
        ),
        load_plugin: Some(record),
        unload_plugin: None,
    };
    static NEW_MAJOR: ParsecOrbitKindHandlerV1 = ParsecOrbitKindHandlerV1 {
        header: ParsecOrbitInterfaceHeader::new(
            std::mem::size_of::<ParsecOrbitKindHandlerV1>(),
            2,
            0,
        ),
        load_plugin: Some(record),
        unload_plugin: None,
    };
    static NO_LOAD: ParsecOrbitKindHandlerV1 = ParsecOrbitKindHandlerV1 {
        header: ParsecOrbitInterfaceHeader::new(
            std::mem::size_of::<ParsecOrbitKindHandlerV1>(),
            1,
            0,
        ),
        load_plugin: None,
        unload_plugin: None,
    };
    static SHORT: ParsecOrbitInterfaceHeader = ParsecOrbitInterfaceHeader::new(8, 1, 0);

    fn put(kind: &str, table: *const ParsecOrbitInterfaceHeader) {
        assert_eq!(
            register("com.example.kind-test", &interface_name(kind), table),
            0
        );
    }

    #[test]
    fn the_interface_name_is_the_documented_one() {
        assert_eq!(interface_name("js"), "parsec-orbit.kind.js");
    }

    #[test]
    fn an_unregistered_kind_has_no_handler() {
        assert!(matches!(handler_for("kt-nobody"), Lookup::Missing));
    }

    #[test]
    fn a_registered_handler_is_found_and_called_with_absolute_paths() {
        put("kt-good", &GOOD.header);
        let Lookup::Found(handler) = handler_for("kt-good") else {
            panic!("handler not found");
        };
        let dir = std::env::temp_dir().join("kt-good-plugin");
        let entry = dir.join("main.js");
        let data = dir.join("data");
        load(
            handler,
            &LoadRequest {
                id: "com.example.kt-good",
                dir: &dir,
                entry: &entry,
                data_dir: Some(&data),
                manifest: "id = \"com.example.kt-good\"\n",
            },
        )
        .unwrap();
        let loaded = LOADED.lock().unwrap();
        let call = loaded
            .iter()
            .find(|call| call.0 == "com.example.kt-good")
            .unwrap();
        assert_eq!(call.1, dir.to_string_lossy());
        assert_eq!(call.2, entry.to_string_lossy());
        assert_eq!(call.3.as_deref(), Some(&*data.to_string_lossy()));
        assert_eq!(call.4, "id = \"com.example.kt-good\"\n");
    }

    #[test]
    fn a_failing_handler_reports_its_code() {
        put("kt-fail", &GOOD.header);
        let Lookup::Found(handler) = handler_for("kt-fail") else {
            panic!("handler not found");
        };
        let error = load(
            handler,
            &LoadRequest {
                id: "com.example.fails",
                dir: Path::new("d"),
                entry: Path::new("d/main.js"),
                data_dir: None,
                manifest: "",
            },
        )
        .unwrap_err();
        assert_eq!(error, "kind handler returned 9");
        let loaded = LOADED.lock().unwrap();
        let call = loaded
            .iter()
            .find(|call| call.0 == "com.example.fails")
            .unwrap();
        assert_eq!(call.3, None, "no data directory is passed on as null");
    }

    #[test]
    fn handlers_this_loader_cannot_serve_are_unusable_not_missing() {
        put("kt-major", &NEW_MAJOR.header);
        put("kt-noload", &NO_LOAD.header);
        put("kt-short", &SHORT);
        for (kind, needle) in [
            ("kt-major", "version 2.0"),
            ("kt-noload", "no load_plugin"),
            ("kt-short", "too small"),
        ] {
            match handler_for(kind) {
                Lookup::Unusable(reason) => assert!(reason.contains(needle), "{reason}"),
                _ => panic!("{kind} should be unusable"),
            }
        }
    }
}
