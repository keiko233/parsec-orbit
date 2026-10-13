//! The loader as a `bcrypt.dll` proxy: its export table, its thunks, and the
//! existing verification tools (`callpaths`, `roundtrip`) run as tests.
#![cfg(windows)]

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use common::{artifacts, Sandbox};
use parsec_orbit_e2e::export_names;

fn system_bcrypt() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join("bcrypt.dll")
}

#[test]
fn the_loader_exports_everything_the_system_bcrypt_exports() {
    let loader = export_names(&artifacts().loader);
    let system = export_names(&system_bcrypt());
    assert!(!system.is_empty());

    let missing: Vec<&String> = system
        .iter()
        .filter(|name| !loader.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "the client would fail to resolve these imports: {missing:?} (run scripts/gen-bcrypt-exports.py)"
    );
}

#[test]
fn the_loader_exports_only_bcrypt_functions_and_the_diagnostics_hook() {
    let allowed: BTreeSet<&str> = ["DllMain", "ParsecOrbitLoaderLastArgs"]
        .into_iter()
        .collect();
    let loader = export_names(&artifacts().loader);
    let unique: BTreeSet<&String> = loader.iter().collect();
    assert_eq!(unique.len(), loader.len(), "no duplicate exports");

    for name in &loader {
        assert!(
            name.starts_with("BCrypt") || allowed.contains(name.as_str()),
            "unexpected export {name}"
        );
    }
    assert!(
        loader.contains(&"ParsecOrbitLoaderLastArgs".to_string()),
        "built with diagnostics"
    );
    assert_eq!(
        loader
            .iter()
            .filter(|name| name.starts_with("BCrypt"))
            .count(),
        60
    );
}

/// Every thunk is `lea rax, [slot]; jmp resolve_stub`, the slots are
/// consecutive in export-name order and all thunks share one stub.
#[test]
fn thunks_point_at_consecutive_slots_and_share_one_resolver() {
    let sandbox = Sandbox::new("thunks");
    sandbox.install_loader().install_host("parsecd.exe");
    let run = sandbox.run("parsecd.exe", "ready:", |command| {
        command.arg("thunks");
    });
    run.assert_success();

    let thunks: Vec<(String, usize, usize)> = run
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix("thunk="))
        // DllMain and the diagnostics hook are ordinary functions, not thunks.
        .filter(|line| line.starts_with("BCrypt"))
        .map(|line| {
            let parts: Vec<&str> = line.split(' ').collect();
            assert_eq!(parts.len(), 3, "bad thunk: {line}\n{}", run.describe());
            (
                parts[0].to_string(),
                usize::from_str_radix(parts[1], 16).unwrap_or_else(|_| panic!("bad thunk: {line}")),
                usize::from_str_radix(parts[2], 16).unwrap_or_else(|_| panic!("bad thunk: {line}")),
            )
        })
        .collect();

    assert_eq!(thunks.len(), 60);
    let first_slot = thunks[0].1;
    let stub = thunks[0].2;
    for (index, (name, slot, target)) in thunks.iter().enumerate() {
        assert_eq!(*slot, first_slot + index * 8, "slot of {name} (#{index})");
        assert_eq!(*target, stub, "{name} jumps to a different stub");
    }
}

#[test]
fn a_call_through_the_loader_reaches_the_real_function() {
    let sandbox = Sandbox::new("genrandom");
    sandbox.install_loader().install_host("parsecd.exe");
    let run = sandbox.run("parsecd.exe", "ready:", |_| {});
    run.assert_success();
    // BCryptGenRandom(NULL, buffer, 32, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
    assert_eq!(
        run.value("genrandom").as_deref(),
        Some("0"),
        "{}",
        run.describe()
    );
}

fn run_tool(tool: &std::path::Path, label: &str) {
    let sandbox = Sandbox::new(label);
    let mut command = Command::new(tool);
    command.arg(&artifacts().loader);
    sandbox.base_environment(&mut command);
    let output = command.output().expect("start the tool");
    assert!(
        output.status.success(),
        "{label} failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Four ways of calling one export, plus the argument registers the real
/// function receives.
#[test]
fn verify_callpaths() {
    run_tool(&artifacts().callpaths, "callpaths");
}

/// SHA-256 and HMAC-SHA256 known answers through the loader.
#[test]
fn verify_roundtrip() {
    run_tool(&artifacts().roundtrip, "roundtrip");
}
