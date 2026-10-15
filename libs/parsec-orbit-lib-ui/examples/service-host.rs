//! Owned host exercises the real UI plugin init and ABI without loading Parsec.
//! Pass a scratch plugin directory containing parsec-orbit-ui-companion[.exe].
use parsec_orbit_lib_ui::OrbitUiPlugin;
use parsec_orbit_plugin_sdk::services::UiClient;
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
    let host = MockHost::new("com.parsec-orbit.ui").with_plugin_dir(&directory);
    host.init::<OrbitUiPlugin>()
        .1
        .map_err(|error| error.to_string())?;
    let consumer = MockHost::new("com.example.ui-probe");
    let client = UiClient::new(&consumer.context()).ok_or("UI service was not registered")?;
    client.register_section(r#"{"id":"abi.probe","title":"ABI probe","items":[{"key":"value","type":"readonly_text","label":"State","default":"Ready"}]}"#).map_err(|error| error.to_string())?;
    client.show().map_err(|error| error.to_string())?;
    assert!(client.is_visible().map_err(|error| error.to_string())?);
    std::thread::sleep(Duration::from_secs(1));
    client
        .update_state("abi.probe", r#"{"value":"Updated through ABI"}"#)
        .map_err(|error| error.to_string())?;
    client.hide().map_err(|error| error.to_string())?;
    assert!(!client.is_visible().map_err(|error| error.to_string())?);
    println!("service_probe: registered=1 show_hide=1 state_update=1");
    for line in host.logs() {
        println!("service_log: {}", line.message);
    }
    // Process exit closes the plugin-owned input pipe and the renderer exits.
    Ok(())
}
