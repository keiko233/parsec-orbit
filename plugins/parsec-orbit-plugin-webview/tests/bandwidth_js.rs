//! Runs `tests/bandwidth.test.js` (the behaviour of `assets/bandwidth.js`)
//! with Node's built-in test runner.
//!
//! Without Node the test is skipped with a note, unless
//! `PARSEC_ORBIT_REQUIRE_NODE` is set, which CI does.

use std::path::Path;
use std::process::Command;

#[test]
fn bandwidth_script_behaves_in_a_fake_browser() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let test_file = manifest_dir.join("tests").join("bandwidth.test.js");

    let output = match Command::new("node").arg("--test").arg(&test_file).output() {
        Ok(output) => output,
        Err(error) => {
            if std::env::var_os("PARSEC_ORBIT_REQUIRE_NODE").is_some() {
                panic!("node is required but could not be started: {error}");
            }
            eprintln!("skipping: node is not available ({error})");
            return;
        }
    };

    assert!(
        output.status.success(),
        "node --test failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
