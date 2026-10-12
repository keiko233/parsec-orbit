//! `tracing` events end up in the loader log.
#![cfg(feature = "tracing")]

use parsec_orbit_plugin_sdk_std::testing::MockHost;
use parsec_orbit_plugin_sdk_std::TracingExt;

#[test]
fn tracing_events_end_up_in_the_loader_log() {
    let host = MockHost::new("com.example.tracing");
    host.context().install_tracing().unwrap();
    tracing::info!(answer = 42, "the message");
    tracing::warn!(target: "my::target", "a warning");
    let logs = host.logs();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].level, parsec_orbit_plugin_sdk_std::LOG_INFO);
    assert_eq!(logs[0].target.as_deref(), Some("com.example.tracing"));
    assert!(
        logs[0].message.ends_with(": the message answer=42"),
        "{}",
        logs[0].message
    );
    assert_eq!(logs[1].level, parsec_orbit_plugin_sdk_std::LOG_WARN);
    assert_eq!(logs[1].message, "my::target: a warning");
    // The global subscriber is set once.
    assert!(host.context().install_tracing().is_err());
}
