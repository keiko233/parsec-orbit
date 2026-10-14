//! End-to-end integration test for the macOS companion window.
//!
//! Builds the loader harness dylib and the UI plugin artifacts into a private
//! target directory, runs the `macos-ui-host` probe, and asserts on its
//! machine-readable probe lines.
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/parsec-orbit-plugin-ui sits two levels below the workspace")
        .to_path_buf()
}

#[test]
fn macos_ui_window_activates_via_distributed_notification() {
    let root = workspace_root();
    let target = root.join("target").join("macos-ui-test");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let build_output = Command::new(&cargo)
        .current_dir(&root)
        .arg("build")
        .arg("--target-dir")
        .arg(&target)
        .args([
            "-p",
            "parsec-orbit-loader-macos",
            "--features",
            "parsec-orbit-loader-macos/harness",
        ])
        .args([
            "-p",
            "parsec-orbit-plugin-ui",
            "--lib",
            "--example",
            "macos-ui-host",
        ])
        .output()
        .expect("start cargo build for macos-ui test");

    assert!(
        build_output.status.success(),
        "building test artifacts failed:\n{}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let example_bin = target.join("debug").join("examples").join("macos-ui-host");
    assert!(
        example_bin.is_file(),
        "example binary not found at {}",
        example_bin.display()
    );

    let run_output = Command::new(&example_bin)
        .output()
        .expect("execute macos-ui-host");

    let stdout = String::from_utf8_lossy(&run_output.stdout);
    let stderr = String::from_utf8_lossy(&run_output.stderr);

    assert!(
        run_output.status.success(),
        "macos-ui-host failed (status={:?}):\nstdout:\n{}\nstderr:\n{}",
        run_output.status.code(),
        stdout,
        stderr
    );

    assert!(
        stdout.contains("probe: init_failures=0"),
        "expected init_failures=0, got:\n{stdout}"
    );
    assert!(
        stdout.contains("probe: interface_found=1"),
        "expected interface_found=1, got:\n{stdout}"
    );
    assert!(
        stdout.contains("probe: initial_on_screen_windows=0"),
        "expected initial_on_screen_windows=0, got:\n{stdout}"
    );
    assert!(
        stdout.contains("probe: on_screen_windows_after_toggle1=1"),
        "expected on_screen_windows_after_toggle1=1, got:\n{stdout}"
    );
    assert!(
        stdout.contains("probe: on_screen_windows_after_toggle2=0"),
        "expected on_screen_windows_after_toggle2=0, got:\n{stdout}"
    );
    assert!(
        stdout.contains("probe: on_screen_windows_after_toggle3=1"),
        "expected on_screen_windows_after_toggle3=1, got:\n{stdout}"
    );
}
