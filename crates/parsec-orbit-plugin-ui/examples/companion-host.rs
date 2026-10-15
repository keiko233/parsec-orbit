//! Disposable standalone service host for a real UI companion. No Parsec is loaded.
//! cargo run -p parsec-orbit-plugin-ui --example companion-host -- <helper-path>
use parsec_orbit_plugin_ui::{companion::Companion, registry::Registry};
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
    assert_eq!(registry.register_section(r#"{
        "id":"demo","title":"Plugin settings","items":[
            {"key":"enabled","type":"toggle","label":"Enable plugin","default":true},
            {"key":"name","type":"text_input","label":"Name / 中文输入","default":"Orbit"},
            {"key":"limit","type":"number_input","label":"Limit","min":0,"max":100,"default":50},
            {"key":"slider","type":"slider","label":"Quality","min":0,"max":100,"default":50},
            {"key":"codec","type":"select","label":"Codec","options":[{"label":"HEVC","value":"hevc"},{"label":"AV1","value":"av1"}],"default":"hevc"},
            {"key":"apply","type":"button","label":"Apply"},
            {"key":"json","type":"json_editor","label":"JSON","default":{"enabled":true}},
            {"key":"status","type":"readonly_text","label":"Status","default":"Ready"}
        ]}"#), 0);
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
