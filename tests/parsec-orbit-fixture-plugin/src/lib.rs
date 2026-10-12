//! A plugin for the end-to-end tests whose behaviour is scripted from the
//! outside.
//!
//! Next to the DLL it reads `fixture.cfg` (the plugin directory comes from the
//! host table, which exercises `plugin_dir`). Each line is one action:
//!
//! ```text
//! register=<name>        register an interface table 1.2 under <name>
//! query=<name>           look <name> up and log what was found
//! handler=<kind>         register a kind handler for <kind>
//! unregister=<name>      remove an interface registered by this plugin
//! fail=<code>            make init return <code>
//! ```
//!
//! Every action logs one `fixture[<id>]: ...` line so a test can assert on it.
//!
//! It is written against the raw ABI on purpose (the deprecated
//! `parsec_orbit_plugin!` macro included), so the end-to-end suite keeps
//! covering plugins that do not use the `Plugin` trait.

#![allow(deprecated)]

use std::ffi::CString;
use std::path::Path;

use parsec_orbit_plugin_sdk::{
    ParsecOrbitHost, ParsecOrbitInterfaceHeader, ParsecOrbitKindHandlerV1,
    ParsecOrbitPluginLoadInfo, LOG_INFO,
};

static INTERFACE: ParsecOrbitInterfaceHeader = ParsecOrbitInterfaceHeader::new(8, 1, 2);

static HANDLER: ParsecOrbitKindHandlerV1 = ParsecOrbitKindHandlerV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitKindHandlerV1>(), 1, 0),
    load_plugin: Some(load_plugin),
    unload_plugin: None,
};

/// The handler runs on the loader thread, long after `init` returned, so it
/// logs through the process wide log instead of a host table.
static HOST: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "system" fn load_plugin(info: *const ParsecOrbitPluginLoadInfo) -> i32 {
    let info = unsafe { &*info };
    let text = |pointer: *const u8| {
        if pointer.is_null() {
            return String::from("(null)");
        }
        unsafe { std::ffi::CStr::from_ptr(pointer as *const _) }
            .to_string_lossy()
            .into_owned()
    };
    let id = text(info.id);
    if let Some(host) = HOST.get() {
        let host = unsafe { &*(*host as *const ParsecOrbitHost) };
        host.log(
            LOG_INFO,
            &format!(
                "fixture-handler: load id={id} entry={} data_dir={} manifest_bytes={}",
                Path::new(&text(info.entry))
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                if info.data_dir.is_null() {
                    "none".to_string()
                } else {
                    "set".to_string()
                },
                info.manifest_len
            ),
        );
    }
    if id.contains("rejected") {
        7
    } else {
        0
    }
}

unsafe extern "system" fn init(host: *const ParsecOrbitHost) -> i32 {
    let host = unsafe { &*host };
    let _ = HOST.set(host as *const ParsecOrbitHost as usize);
    let id = host.plugin_id().unwrap_or("no-revision-2").to_string();
    let say = |message: String| host.log_with_target(LOG_INFO, &id, &message);

    let dir = host.plugin_dir().unwrap_or("").to_string();
    say(format!(
        "fixture[{id}]: revision2={} dir={} data_dir={}",
        host.has_revision_2(),
        Path::new(&dir)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        host.data_dir().map_or("none", |dir| {
            if dir.ends_with(&id) {
                "ends-with-id"
            } else {
                "other"
            }
        })
    ));

    let script = std::fs::read_to_string(Path::new(&dir).join("fixture.cfg")).unwrap_or_default();
    let mut result = 0;
    for line in script
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some((action, argument)) = line.split_once('=') else {
            continue;
        };
        let name = CString::new(argument).unwrap_or_default();
        match action {
            "register" => {
                let code = host.register(&name, &INTERFACE);
                say(format!("fixture[{id}]: register {argument} -> {code}"));
            }
            "handler" => {
                let interface = CString::new(format!("parsec-orbit.kind.{argument}")).unwrap();
                let code = host.register(&interface, &HANDLER.header);
                say(format!("fixture[{id}]: handler {argument} -> {code}"));
            }
            "unregister" => {
                let code = match host.unregister_interface {
                    Some(unregister) => unsafe { unregister(host, name.as_ptr() as *const u8) },
                    None => -4,
                };
                say(format!("fixture[{id}]: unregister {argument} -> {code}"));
            }
            "query" => match unsafe { host.interface(&name) } {
                Some(found) => say(format!(
                    "fixture[{id}]: query {argument} -> {}.{} ({} bytes)",
                    found.major(),
                    found.minor(),
                    found.size
                )),
                None => say(format!("fixture[{id}]: query {argument} -> not found")),
            },
            "fail" => result = argument.parse().unwrap_or(1),
            _ => say(format!("fixture[{id}]: unknown action {action}")),
        }
    }
    result
}

parsec_orbit_plugin_sdk::parsec_orbit_plugin! {
    id = "com.example.fixture",
    name = "Fixture",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Scripted by fixture.cfg",
    license = "MIT",
    init = init,
}
