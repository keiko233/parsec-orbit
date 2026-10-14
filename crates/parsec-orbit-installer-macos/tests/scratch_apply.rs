//! Scratch apply/re-apply/uninstall and fault-recovery measurements.
//!
//! Every test builds a disposable toy bundle with `clang` (arm64 executable
//! with linker header padding, a toy loader dylib and a toy client payload),
//! signs it ad-hoc, and runs the real installer against it. Nothing touches
//! `/Applications`, vendor bytes, real state or a privilege boundary; the
//! bundles live in the test scratch directory and the assertions are the
//! contract from the M1a checkpoint:
//!
//! - a supported stub is patched, signed, verified and swapped in;
//! - the inserted `LC_LOAD_DYLIB` is honored by dyld when the patched stub
//!   actually runs (the toy loader writes a marker from its constructor);
//! - re-apply is a verified no-op; uninstall restores a byte/metadata-equal
//!   pristine bundle and keeps the Orbit data;
//! - an injected failure after the backup rolls back and the next run
//!   recovers; an interrupted transaction is recovered by the next apply;
//! - unsupported stubs refuse before any change, a tampered app refuses
//!   re-apply and uninstall, and a running app refuses mutation;
//! - a failing preseed is a recorded warning, never a rollback.
//!
//! The tests are macOS/arm64-only, like the loader e2e suite.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use parsec_orbit_core::testutil::TempDir;
use parsec_orbit_installer::apply::{
    apply, apply_with, ApplyOptions, ApplyOutcome, Faults, PreseedOutcome, Step,
};
use parsec_orbit_installer::digest::{self, Tree};
use parsec_orbit_installer::journal::{self, InstallDir, InstallState, Manifest, SCHEMA_VERSION};
use parsec_orbit_installer::platform;
use parsec_orbit_installer::uninstall::{
    uninstall, uninstall_with, UninstallFaults, UninstallOptions, UninstallStep,
};
use parsec_orbit_installer::whitelist::Whitelist;
use parsec_orbit_installer::{
    LOADER_DEPENDENCY, LOADER_RELATIVE, RECORD_RELATIVE, SKEL_RELATIVE, STUB_RELATIVE,
};

/// Paths a supported apply may add or change inside the bundle.
const EXPECTED_CHANGES: &[&str] = &[
    STUB_RELATIVE,
    LOADER_RELATIVE,
    "Contents/Frameworks",
    RECORD_RELATIVE,
    "Contents/_CodeSignature",
    "Contents/_CodeSignature/CodeResources",
];

const LOADER_SOURCE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
__attribute__((constructor)) static void toy_loader_loaded(void) {
    const char *marker = getenv("TOY_LOADER_MARKER");
    if (!marker) return;
    FILE *file = fopen(marker, "w");
    if (!file) return;
    fputs("toy loader loaded\n", file);
    fclose(file);
}
"#;

const CLIENT_SOURCE: &str = r#"
int wx_main(void *one, void *two, void *three, void *four) {
    (void)one; (void)two; (void)three; (void)four;
    return 0;
}
"#;

const STUB_SOURCE: &str = r#"
#include <stdlib.h>
#include <unistd.h>
int main(void) {
    const char *sleeping = getenv("TOY_STUB_SLEEP");
    if (sleeping) sleep(20);
    return 0;
}
"#;

const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.example.toy</string>
	<key>CFBundleExecutable</key>
	<string>parsecd</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
</dict>
</plist>
"#;

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

struct Toy {
    scratch: TempDir,
    app: PathBuf,
    loader: PathBuf,
    orbit: PathBuf,
    whitelist: Whitelist,
    /// Snapshot of the pristine bundle taken before any mutation.
    tree: Tree,
    stub_sha256: String,
    client_sha256: Option<String>,
}

impl Toy {
    fn options(&self) -> ApplyOptions {
        ApplyOptions::new(&self.orbit).with_whitelist(self.whitelist.clone())
    }

    fn install_dir(&self) -> InstallDir {
        InstallDir::new(
            &self.orbit,
            &journal::installation_id("com.example.toy", &self.stub_sha256),
        )
    }
}

/// Builds a disposable, ad-hoc-signed toy bundle.
fn build_toy(label: &str, with_skel: bool) -> Toy {
    let scratch = TempDir::new(label);
    let sources = scratch.path().join("sources");
    fs::create_dir_all(&sources).unwrap();

    let loader = scratch.path().join("libparsec_orbit_loader.dylib");
    fs::write(sources.join("loader.c"), LOADER_SOURCE).unwrap();
    run(Command::new("clang")
        .args(["-arch", "arm64", "-dynamiclib"])
        .arg(sources.join("loader.c"))
        .arg("-o")
        .arg(&loader)
        .args([
            "-install_name",
            "@executable_path/../Frameworks/libparsec_orbit_loader.dylib",
        ]));

    let client = sources.join("parsecd-toy.dylib");
    fs::write(sources.join("client.c"), CLIENT_SOURCE).unwrap();
    run(Command::new("clang")
        .args(["-arch", "arm64", "-dynamiclib"])
        .arg(sources.join("client.c"))
        .arg("-o")
        .arg(&client));

    let stub = sources.join("parsecd");
    fs::write(sources.join("stub.c"), STUB_SOURCE).unwrap();
    run(Command::new("clang")
        .args(["-arch", "arm64", "-Wl,-headerpad,0x200"])
        .arg(sources.join("stub.c"))
        .arg("-o")
        .arg(&stub));

    let app = scratch.path().join("Parsec.app");
    let macos = app.join("Contents/MacOS");
    let skel = app.join(SKEL_RELATIVE);
    fs::create_dir_all(&macos).unwrap();
    fs::create_dir_all(&skel).unwrap();
    fs::write(app.join("Contents/Info.plist"), INFO_PLIST).unwrap();
    fs::copy(&stub, app.join(STUB_RELATIVE)).unwrap();

    let mut client_sha256 = None;
    if with_skel {
        let payload = skel.join("parsecd-toy.dylib");
        fs::copy(&client, &payload).unwrap();
        let hash = digest::sha256_file(&payload).unwrap();
        fs::write(
            skel.join("appdata.json"),
            format!(
                "{{\"entry_symbol\":\"wx_main\",\"hash\":\"{hash}\",\"so_name\":\"parsecd-toy.dylib\"}}"
            ),
        )
        .unwrap();
        client_sha256 = Some(hash);
    }

    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app));

    let stub_sha256 = digest::sha256_file(&app.join(STUB_RELATIVE)).unwrap();
    let tree = digest::tree(&app).unwrap();
    let whitelist = Whitelist::from_hashes([stub_sha256.clone()]);
    let orbit = scratch.path().join("orbit");
    Toy {
        scratch,
        app,
        loader,
        orbit,
        whitelist,
        tree,
        stub_sha256,
        client_sha256,
    }
}

#[derive(Debug)]
struct FailAt(Step);

impl Faults for FailAt {
    fn check(&self, step: Step) -> Result<(), String> {
        if step == self.0 {
            Err(format!("injected failure at {}", step.name()))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct UninstallFailAt(UninstallStep);

impl UninstallFaults for UninstallFailAt {
    fn check(&self, step: UninstallStep) -> Result<(), String> {
        if step == self.0 {
            Err(format!("injected failure at {}", step.name()))
        } else {
            Ok(())
        }
    }
}

fn assert_only_expected_changes(toy: &Toy) {
    let patched = digest::tree(&toy.app).unwrap();
    let changed = digest::changed_paths_for_signing(
        &toy.tree,
        &patched,
        &["", "Contents", "Contents/Resources", "Contents/MacOS"],
    );
    for path in changed.keys() {
        assert!(
            EXPECTED_CHANGES.contains(&path.as_str()),
            "unexpected bundle change: {path}"
        );
    }
    assert!(changed.contains_key(STUB_RELATIVE));
    assert!(changed.contains_key(LOADER_RELATIVE));
    assert!(changed.contains_key(RECORD_RELATIVE));
}

#[test]
fn apply_reapply_and_uninstall_round_trip() {
    let toy = build_toy("installer-round-trip", true);
    let options = toy.options();

    let report = apply(&toy.app, &toy.loader, &options).expect("apply");
    assert_eq!(report.outcome, ApplyOutcome::Applied);
    assert_eq!(
        report.preseed,
        PreseedOutcome::Seeded,
        "{}",
        report.preseed_detail
    );
    assert_eq!(report.stub_sha256, toy.stub_sha256);
    let applied_stub_sha256 = report
        .applied_stub_sha256
        .clone()
        .expect("applied stub hash");
    assert!(report.patch.is_some());

    let stub = fs::read(toy.app.join(STUB_RELATIVE)).unwrap();
    assert!(stub
        .windows(LOADER_DEPENDENCY.len())
        .any(|window| window == LOADER_DEPENDENCY.as_bytes()));
    assert!(toy.app.join(LOADER_RELATIVE).is_file());
    assert_eq!(mode_of(&toy.app.join(LOADER_RELATIVE)), 0o755);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(toy.app.join(RECORD_RELATIVE)).unwrap()).unwrap();
    assert_eq!(record["schema_version"], 1);
    assert_eq!(record["uid"], platform::effective_uid());
    assert_eq!(mode_of(&toy.app.join(RECORD_RELATIVE)), 0o600);
    assert_only_expected_changes(&toy);

    // Records are private.
    assert_eq!(mode_of(&toy.orbit), 0o700);
    let install = toy.install_dir();
    assert_eq!(mode_of(install.root()), 0o700);
    assert_eq!(mode_of(&install.manifest_path()), 0o600);
    assert_eq!(mode_of(&install.journal_path()), 0o600);
    assert_eq!(mode_of(&install.entitlements_path()), 0o600);
    let manifest = journal::read_manifest(&install).unwrap().unwrap();
    assert_eq!(manifest.state, InstallState::Installed);
    assert_eq!(manifest.stub_sha256, toy.stub_sha256);
    let events: Vec<String> = journal::Journal::new(&install)
        .entries()
        .unwrap()
        .into_iter()
        .map(|entry| entry.event)
        .collect();
    for event in [
        "transaction-begin",
        "backed-up",
        "staged",
        "patched",
        "signed-loader",
        "signed-bundle",
        "verified-staged",
        "swapped",
        "verified-installed",
        "preseeded",
        "committed",
    ] {
        assert!(
            events.contains(&event.to_string()),
            "journal is missing {event}: {events:?}"
        );
    }

    // The isolated state was seeded from the verified skel.
    let state = toy.orbit.join("home/.parsec");
    assert_eq!(mode_of(&toy.orbit.join("home")), 0o700);
    assert_eq!(
        digest::sha256_file(&state.join("parsecd-toy.dylib")).unwrap(),
        toy.client_sha256.clone().unwrap()
    );

    // dyld must honor the inserted dependency when the patched stub runs.
    let marker = toy.scratch.path().join("loader-marker");
    let status = Command::new(toy.app.join(STUB_RELATIVE))
        .env("TOY_LOADER_MARKER", &marker)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        marker.is_file(),
        "the patched stub did not load the inserted dependency"
    );

    // Re-apply is a verified no-op.
    let again = apply(&toy.app, &toy.loader, &options).expect("re-apply");
    assert_eq!(again.outcome, ApplyOutcome::AlreadyApplied);
    assert_eq!(
        digest::sha256_file(&toy.app.join(STUB_RELATIVE)).unwrap(),
        applied_stub_sha256
    );

    // Uninstall restores the recorded pristine bundle, byte for byte.
    let restored = uninstall(&toy.app, &UninstallOptions::new(&toy.orbit)).expect("uninstall");
    assert_eq!(restored.restored_stub_sha256, toy.stub_sha256);
    assert_eq!(
        digest::sha256_file(&toy.app.join(STUB_RELATIVE)).unwrap(),
        toy.stub_sha256
    );
    assert_eq!(digest::tree(&toy.app).unwrap(), toy.tree);
    assert!(restored.removed.is_dir(), "the patched bundle was not kept");
    platform::codesign_verify(&toy.app).unwrap();
    assert_eq!(
        journal::read_manifest(&install).unwrap().unwrap().state,
        InstallState::Uninstalled
    );
    // Orbit data survives an uninstall by default.
    assert!(state.join("parsecd-toy.dylib").is_file());

    // A second uninstall has nothing to restore and refuses.
    assert!(uninstall(&toy.app, &UninstallOptions::new(&toy.orbit)).is_err());

    // Re-applying after the uninstall keeps the existing isolated state.
    let third = apply(&toy.app, &toy.loader, &options).expect("re-apply after uninstall");
    assert_eq!(third.outcome, ApplyOutcome::Applied);
    assert_eq!(third.preseed, PreseedOutcome::Skipped);
    assert_eq!(
        digest::sha256_file(&state.join("parsecd-toy.dylib")).unwrap(),
        toy.client_sha256.unwrap()
    );
}

#[test]
fn an_injected_failure_rolls_back_and_the_next_run_recovers() {
    let toy = build_toy("installer-rollback", true);
    let options = toy.options();

    let failure = apply_with(&toy.app, &toy.loader, &options, &FailAt(Step::VerifyStaged))
        .expect_err("the injected failure must abort the apply");
    assert_eq!(failure.step, Step::VerifyStaged);
    assert!(failure.rolled_back, "{failure}");
    assert_eq!(digest::tree(&toy.app).unwrap(), toy.tree);
    assert_eq!(
        digest::sha256_file(&toy.app.join(STUB_RELATIVE)).unwrap(),
        toy.stub_sha256
    );
    platform::codesign_verify(&toy.app).unwrap();

    let install = toy.install_dir();
    assert_eq!(
        journal::read_manifest(&install).unwrap().unwrap().state,
        InstallState::RolledBack
    );
    assert!(
        !install.pristine().exists(),
        "the pristine backup was consumed"
    );
    assert!(!install.staged().exists());
    assert!(journal::Journal::new(&install)
        .entries()
        .unwrap()
        .iter()
        .any(|entry| entry.event == "rolled-back"));
    assert!(!toy.orbit.join("home").exists(), "no state was seeded");

    // The ordinary next run completes.
    let report = apply(&toy.app, &toy.loader, &options).expect("apply after rollback");
    assert_eq!(report.outcome, ApplyOutcome::Applied);
    assert_eq!(
        journal::read_manifest(&install).unwrap().unwrap().state,
        InstallState::Installed
    );
}

#[test]
fn a_signable_data_file_is_not_accepted_as_a_loader() {
    let toy = build_toy("installer-signing-failure", true);
    fs::write(&toy.loader, b"not a Mach-O library").unwrap();
    // codesign accepts ordinary data using xattrs. A valid code signature
    // therefore cannot substitute for a dylib architecture/type check.
    platform::codesign_sign(&toy.loader, None, false).unwrap();
    let failure = apply(&toy.app, &toy.loader, &toy.options()).unwrap_err();
    assert_eq!(failure.step, Step::Inspect);
    assert!(!failure.rolled_back, "{failure:?}");
    assert!(!toy.orbit.exists());
    assert!(digest::differences(&toy.tree, &digest::tree(&toy.app).unwrap()).is_empty());
    platform::codesign_verify(&toy.app).unwrap();
    assert!(!toy.orbit.join("home/.parsec").exists());
}

/// Crafts the on-disk state an interrupted transaction leaves behind.
fn craft_interruption(toy: &Toy, after_swap: bool) {
    let install = toy.install_dir();
    fs::create_dir_all(&toy.orbit).unwrap();
    platform::set_mode(&toy.orbit, 0o700).unwrap();
    install.ensure().unwrap();
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        installation_id: journal::installation_id("com.example.toy", &toy.stub_sha256),
        bundle_id: "com.example.toy".into(),
        app: fs::canonicalize(&toy.app).unwrap(),
        executable: fs::canonicalize(toy.app.join(STUB_RELATIVE)).unwrap(),
        orbit_root: fs::canonicalize(&toy.orbit).unwrap(),
        stub_sha256: toy.stub_sha256.clone(),
        loader_sha256: digest::sha256_file(&toy.loader).unwrap(),
        applied_loader_sha256: None,
        applied_stub_sha256: None,
        pristine_tree: toy.tree.clone(),
        applied_tree: None,
        state: InstallState::BackedUp,
        updated_unix: journal::unix_time(),
    };
    journal::write_manifest(&install, &manifest).unwrap();
    fs::rename(&toy.app, install.pristine()).unwrap();
    // A half-built staged copy, exactly like a crash during staging.
    fs::create_dir(install.staged()).unwrap();
    fs::write(install.staged().join("partial"), b"half").unwrap();
    if after_swap {
        fs::rename(install.staged(), &toy.app).unwrap();
        // A real swap is preceded by a persisted verified-staged snapshot.
        let mut manifest = manifest;
        manifest.applied_tree = Some(digest::tree(&toy.app).unwrap());
        journal::write_manifest(&install, &manifest).unwrap();
    }
}

#[test]
fn an_external_bundle_during_recovery_is_preserved() {
    let toy = build_toy("installer-external-recovery", true);
    craft_interruption(&toy, false);
    fs::create_dir(&toy.app).unwrap();
    fs::write(toy.app.join("vendor-update"), b"new installation").unwrap();
    let external = digest::tree(&toy.app).unwrap();
    let failure = apply(&toy.app, &toy.loader, &toy.options()).unwrap_err();
    assert_eq!(failure.step, Step::Recover);
    assert!(!failure.rolled_back);
    assert_eq!(digest::tree(&toy.app).unwrap(), external);
    assert_eq!(
        digest::tree(&toy.install_dir().pristine()).unwrap(),
        toy.tree
    );
}

#[test]
fn a_vendor_replacement_before_swap_is_not_overwritten_by_rollback() {
    struct ReplaceAtSwap(PathBuf);
    impl Faults for ReplaceAtSwap {
        fn check(&self, step: Step) -> Result<(), String> {
            if step == Step::Swap {
                fs::create_dir(&self.0).unwrap();
                fs::write(self.0.join("vendor-update"), b"replacement").unwrap();
            }
            Ok(())
        }
    }
    let toy = build_toy("installer-update-at-swap", true);
    let failure = apply_with(
        &toy.app,
        &toy.loader,
        &toy.options(),
        &ReplaceAtSwap(toy.app.clone()),
    )
    .unwrap_err();
    assert_eq!(failure.step, Step::Swap);
    assert!(!failure.rolled_back);
    assert_eq!(
        fs::read(toy.app.join("vendor-update")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        digest::tree(&toy.install_dir().pristine()).unwrap(),
        toy.tree
    );
}

#[test]
fn competing_apply_and_uninstall_refuse_without_mutating_the_bundle() {
    let toy = build_toy("installer-concurrent", true);
    let lock = parsec_orbit_installer::lock::AppLock::acquire(&toy.app).unwrap();
    let alternate = ApplyOptions::new(toy.scratch.path().join("other-root"))
        .with_whitelist(toy.whitelist.clone());
    assert!(apply(&toy.app, &toy.loader, &alternate).is_err());
    assert!(uninstall(&toy.app, &UninstallOptions::new(&toy.orbit)).is_err());
    assert_eq!(digest::tree(&toy.app).unwrap(), toy.tree);
    drop(lock);
    apply(&toy.app, &toy.loader, &toy.options()).expect("apply after releasing lock");
}

#[test]
fn a_universal_stub_keeps_both_slices_and_restores_exactly() {
    let mut toy = build_toy("installer-universal", true);
    let sources = toy.scratch.path().join("sources");
    let x86 = sources.join("parsecd-x86");
    run(Command::new("clang")
        .args(["-arch", "x86_64", "-Wl,-headerpad,0x200"])
        .arg(sources.join("stub.c"))
        .arg("-o")
        .arg(&x86));
    let universal = sources.join("parsecd-universal");
    run(Command::new("lipo")
        .arg("-create")
        .arg(toy.app.join(STUB_RELATIVE))
        .arg(x86)
        .arg("-output")
        .arg(&universal));
    fs::copy(universal, toy.app.join(STUB_RELATIVE)).unwrap();
    platform::codesign_sign(&toy.app, None, false).unwrap();
    toy.stub_sha256 = digest::sha256_file(&toy.app.join(STUB_RELATIVE)).unwrap();
    toy.whitelist = Whitelist::from_hashes([toy.stub_sha256.clone()]);
    toy.tree = digest::tree(&toy.app).unwrap();
    let report = apply(&toy.app, &toy.loader, &toy.options()).expect("universal apply");
    let stub = fs::read(toy.app.join(STUB_RELATIVE)).unwrap();
    parsec_orbit_installer::macho::verify_dependency(&stub, LOADER_DEPENDENCY).unwrap();
    parsec_orbit_installer::macho::sections_preserved(
        &stub,
        &report.patch.unwrap().file_backed_sections,
    )
    .unwrap();
    let archs = Command::new("lipo")
        .arg("-archs")
        .arg(toy.app.join(STUB_RELATIVE))
        .output()
        .unwrap();
    let archs = String::from_utf8(archs.stdout).unwrap();
    assert!(
        archs.contains("arm64") && archs.contains("x86_64"),
        "{archs}"
    );
    assert!(Command::new(toy.app.join(STUB_RELATIVE))
        .status()
        .unwrap()
        .success());
    uninstall(&toy.app, &UninstallOptions::new(&toy.orbit)).unwrap();
    assert_eq!(digest::tree(&toy.app).unwrap(), toy.tree);
}

#[test]
fn assessment_keeps_a_synthetic_quarantine_attribute_and_bundle_identity() {
    let toy = build_toy("installer-assessment", true);
    let quarantine = "0081;65000000;OrbitScratch;";
    run(Command::new("/usr/bin/xattr")
        .args(["-w", "com.apple.quarantine", quarantine])
        .arg(&toy.app));
    let before = digest::tree(&toy.app).unwrap();
    let report = parsec_orbit_installer::assessment::assess(&toy.app).unwrap();
    assert!(report.strict_signature.success);
    assert_eq!(report.bundle_id, "com.example.toy");
    assert_eq!(report.quarantine.as_deref(), Some(quarantine.as_bytes()));
    assert!(report.signing_details.stderr.contains("Signature=adhoc"));
    assert!(report.gatekeeper.exit_code.is_some());
    assert!(digest::differences(&before, &digest::tree(&toy.app).unwrap()).is_empty());
}

#[test]
fn an_interrupted_transaction_is_recovered_by_the_next_apply() {
    for after_swap in [false, true] {
        let toy = build_toy(
            if after_swap {
                "installer-interrupt-swap"
            } else {
                "installer-interrupt-backup"
            },
            true,
        );
        craft_interruption(&toy, after_swap);

        let options = toy.options();
        let report = apply(&toy.app, &toy.loader, &options).expect("recovered apply");
        assert_eq!(report.outcome, ApplyOutcome::Applied);
        let install = toy.install_dir();
        let manifest = journal::read_manifest(&install).unwrap().unwrap();
        assert_eq!(manifest.state, InstallState::Installed);
        assert_eq!(digest::tree(&install.pristine()).unwrap(), toy.tree);
        assert!(journal::Journal::new(&install)
            .entries()
            .unwrap()
            .iter()
            .any(|entry| entry.event == "rolled-back"));
        if after_swap {
            assert!(
                install.root().join("applied-removed").is_dir(),
                "the interrupted bundle was not kept aside"
            );
        }
    }
}

#[test]
fn unsupported_stubs_and_missing_apps_refuse_before_any_change() {
    let toy = build_toy("installer-unknown-stub", true);
    let options =
        ApplyOptions::new(&toy.orbit).with_whitelist(Whitelist::from_hashes(["0".repeat(64)]));
    let failure = apply(&toy.app, &toy.loader, &options).expect_err("unsupported stub");
    assert_eq!(failure.step, Step::Inspect);
    assert!(!failure.rolled_back);
    assert!(failure.message.contains("not a supported vendor bootstrap"));
    assert!(!toy.orbit.exists(), "the refused apply created state");
    assert_eq!(digest::tree(&toy.app).unwrap(), toy.tree);

    let failure = apply(
        &toy.scratch.path().join("missing.app"),
        &toy.loader,
        &toy.options(),
    )
    .expect_err("missing app");
    assert_eq!(failure.step, Step::Inspect);
    assert!(!toy.orbit.exists());
}

#[test]
fn a_tampered_app_refuses_reapply_and_uninstall() {
    let toy = build_toy("installer-stale", true);
    let options = toy.options();
    apply(&toy.app, &toy.loader, &options).expect("apply");

    let extra = toy.app.join("Contents/Resources/extra.txt");
    fs::write(&extra, b"unrecorded").unwrap();
    let failure = apply(&toy.app, &toy.loader, &options).expect_err("stale re-apply");
    assert_eq!(failure.step, Step::Recover);
    assert!(!failure.rolled_back);
    assert!(extra.is_file(), "the refused apply changed the bundle");

    let failure =
        uninstall(&toy.app, &UninstallOptions::new(&toy.orbit)).expect_err("stale uninstall");
    assert_eq!(failure.step, UninstallStep::CheckApplied);
    assert!(!failure.restored);
    assert!(extra.is_file());
    let install = toy.install_dir();
    assert_eq!(digest::tree(&install.pristine()).unwrap(), toy.tree);
    assert_eq!(
        journal::read_manifest(&install).unwrap().unwrap().state,
        InstallState::Installed
    );
}

#[test]
fn a_running_app_refuses_mutation_until_it_exits() {
    let toy = build_toy("installer-running", true);
    let stub = toy.app.join(STUB_RELATIVE);
    let mut child = Command::new(&stub)
        .env("TOY_STUB_SLEEP", "1")
        .spawn()
        .unwrap();

    let mut visible = false;
    for _ in 0..50 {
        if platform::running_executables(&stub)
            .unwrap()
            .contains(&child.id())
        {
            visible = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(visible, "the toy stub did not become visible to libproc");

    let failure = apply(&toy.app, &toy.loader, &toy.options()).expect_err("running app");
    assert_eq!(failure.step, Step::Inspect);
    assert!(failure.message.contains("running"), "{failure}");
    assert!(!toy.orbit.exists());

    // Identity-bound cleanup: only the child this test started.
    let _ = child.kill();
    let _ = child.wait();

    let report = apply(&toy.app, &toy.loader, &toy.options()).expect("apply after exit");
    assert_eq!(report.outcome, ApplyOutcome::Applied);
}

#[test]
fn a_failing_preseed_is_a_warning_not_a_rollback() {
    let toy = build_toy("installer-preseed-warning", false);
    let report = apply(&toy.app, &toy.loader, &toy.options()).expect("apply");
    assert_eq!(report.outcome, ApplyOutcome::Applied);
    assert_eq!(report.preseed, PreseedOutcome::Warned);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("preseed")),
        "{:?}",
        report.warnings
    );
    assert_eq!(mode_of(&toy.orbit.join("home")), 0o700);
    assert!(!toy.orbit.join("home/.parsec").exists());
    assert_eq!(
        journal::read_manifest(&toy.install_dir())
            .unwrap()
            .unwrap()
            .state,
        InstallState::Installed
    );

    // The uninstall still restores the pristine bundle.
    uninstall(&toy.app, &UninstallOptions::new(&toy.orbit)).expect("uninstall");
    assert_eq!(digest::tree(&toy.app).unwrap(), toy.tree);
}

#[test]
fn an_uninstall_failure_restores_the_pristine_bundle() {
    let toy = build_toy("installer-uninstall-failure", true);
    apply(&toy.app, &toy.loader, &toy.options()).expect("apply");
    let failure = uninstall_with(
        &toy.app,
        &UninstallOptions::new(&toy.orbit),
        &UninstallFailAt(UninstallStep::VerifyRestored),
    )
    .expect_err("injected uninstall failure");
    assert_eq!(failure.step, UninstallStep::VerifyRestored);
    assert!(failure.restored, "{failure}");
    assert_eq!(digest::tree(&toy.app).unwrap(), toy.tree);
    assert!(toy.install_dir().root().join("applied-removed").is_dir());
}
