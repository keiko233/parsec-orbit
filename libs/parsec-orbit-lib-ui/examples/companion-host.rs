//! Disposable standalone service host for a real UI companion. No Parsec is loaded.
//! cargo run -p parsec-orbit-lib-ui --example companion-host -- <helper-path>
use parsec_orbit_lib_ui::{companion::Companion, registry::Registry};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

unsafe extern "system" fn action(_: *mut c_void, text: *const u8, length: usize) {
    // Safety: the registry supplies a UTF-8 payload valid for this callback.
    let text = unsafe { std::slice::from_raw_parts(text, length) };
    println!("action: {}", String::from_utf8_lossy(text));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("expected companion executable path")?,
    )
    .canonicalize()?;
    let registry = Arc::new(Registry::new());
    registry
        .register_page(
            r#"{
        "schema":2,"id":"demo","title":"Plugin settings","icon":"plugin",
        "description":"A page with one control of every kind.",
        "blocks":[
            {"type":"banner","key":"note","severity":"info","title":"Demo","message":"Disposable host; nothing here is saved."},
            {"type":"group","title":"Controls","settings":[
                {"key":"enabled","label":"Enable plugin","description":"A switch.","control":{"type":"toggle","default":true}},
                {"key":"name","label":"Name / 中文输入","control":{"type":"text","default":"Orbit"}},
                {"key":"limit","label":"Limit","control":{"type":"number","min":0,"max":100,"unit":"Mbps","default":50}},
                {"key":"slider","label":"Quality","control":{"type":"slider","min":0,"max":100,"default":50}},
                {"key":"codec","label":"Codec","control":{"type":"select","options":[{"label":"HEVC","value":"hevc"},{"label":"AV1","value":"av1"}],"default":"hevc"}},
                {"key":"notes","label":"Notes","control":{"type":"text","multiline":true,"rows":4}},
                {"key":"json","label":"JSON","control":{"type":"json","default":{"enabled":true}}},
                {"key":"status","label":"Status","control":{"type":"value","default":"Ready"}}
            ]},
            {"type":"actions","buttons":[{"key":"apply","text":"Apply","style":"accent"}]}
        ]}"#,
            false,
        )
        .map_err(|failure| failure.reason)?;
    registry.on_action("demo", action, std::ptr::null_mut());
    let visible = Arc::new(AtomicBool::new(false));
    let companion = Companion::start(
        executable,
        registry,
        visible.clone(),
        Arc::new(|text| eprintln!("{text}")),
    )?;
    companion.set_visible(true);
    let seconds = std::env::var("ORBIT_UI_PROBE_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    for _ in 0..seconds {
        std::thread::sleep(Duration::from_secs(1));
        println!(
            "companion_pid={} visible={}",
            companion.child_pid(),
            visible.load(Ordering::SeqCst)
        );
    }
    Ok(())
}
