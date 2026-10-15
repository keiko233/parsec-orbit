//! Owned host exercises the real UI plugin init and ABI without loading Parsec.
//! Pass a scratch plugin directory containing parsec-orbit-ui-companion[.exe].
use parsec_orbit_lib_ui::OrbitUiPlugin;
use parsec_orbit_plugin_sdk::services::ui::UiClient;
use parsec_orbit_plugin_sdk_testing::MockHost;
use std::path::PathBuf;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("expected scratch plugin directory")?,
    )
    .canonicalize()?;
    // The built-in Overview and Log pages need a data directory for their preferences.
    let data = directory.join("data");
    std::fs::create_dir_all(&data)?;
    let host = MockHost::new("com.parsec-orbit.ui")
        .with_plugin_dir(&directory)
        .with_data_dir(&data);
    host.init::<OrbitUiPlugin>()
        .1
        .map_err(|error| error.to_string())?;
    let consumer = MockHost::new("com.example.ui-probe");
    let client = UiClient::new(&consumer.context()).ok_or("UI service was not registered")?;
    client.register_page(r#"{"schema":2,"id":"abi.probe","title":"ABI probe","blocks":[{"type":"group","settings":[{"key":"value","label":"State","control":{"type":"value","default":"Ready"}}]}]}"#).map_err(|error| error.to_string())?;
    client.show().map_err(|error| error.to_string())?;
    assert!(client.is_visible().map_err(|error| error.to_string())?);
    std::thread::sleep(Duration::from_secs(1));
    client
        .update_page("abi.probe", r#"{"values":{"value":"Updated through ABI"}}"#)
        .map_err(|error| error.to_string())?;
    // Keep the window up to look at it: ORBIT_UI_PROBE_SECONDS=60 holds it a minute.
    let hold: u64 = std::env::var("ORBIT_UI_PROBE_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .unwrap_or(0);
    std::thread::sleep(Duration::from_secs(hold));
    client.hide().map_err(|error| error.to_string())?;
    assert!(!client.is_visible().map_err(|error| error.to_string())?);
    println!("service_probe: registered=1 show_hide=1 state_update=1");
    for line in host.logs() {
        println!("service_log: {}", line.message);
    }
    // Process exit closes the plugin-owned input pipe and the renderer exits.
    Ok(())
}
