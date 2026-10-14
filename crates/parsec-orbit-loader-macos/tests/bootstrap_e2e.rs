//! Dependency and NSS gates in disposable C hosts. No vendor code/state.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use parsec_orbit_core::testutil::TempDir;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dependency_constructor_forwards_and_selected_hosts_fail_before_main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let target = root.join("target/macos-bootstrap-e2e");
    run(
        Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .current_dir(root)
            .args([
                "build",
                "--locked",
                "-p",
                "parsec-orbit-loader-macos",
                "--features",
                "harness",
                "--target-dir",
            ])
            .arg(&target),
    );
    let scratch = TempDir::new("bootstrap");
    let contents = scratch.path().join("Toy.app/Contents");
    let frameworks = contents.join("Frameworks");
    let macos = contents.join("MacOS");
    fs::create_dir_all(&frameworks).unwrap();
    fs::create_dir_all(&macos).unwrap();
    let loader = frameworks.join("libparsec_orbit_loader.dylib");
    fs::copy(target.join("debug/libparsec_orbit_loader.dylib"), &loader).unwrap();
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&loader));
    let probe = scratch.path().join("probe.c");
    fs::write(
        &probe,
        r#"
#include <pwd.h>
#include <unistd.h>
#include <string.h>
#include <errno.h>
static int missing_code, missing_present;
int probe_baseline(void) {
    struct passwd value, *result = 0; char buffer[8192];
    missing_code = getpwuid_r((uid_t)-2, &value, buffer, sizeof buffer, &result);
    missing_present = result != 0;
    return 0;
}
int probe_home(const char *expected) {
    struct passwd *p = getpwuid(getuid());
    if (!p || strcmp(p->pw_dir, expected)) return 1;
    struct passwd value, *result = 0; char buffer[8192];
    int rc = getpwuid_r(getuid(), &value, buffer, sizeof buffer, &result);
    if (rc || !result || strcmp(value.pw_dir, expected)) return 2;
    if (value.pw_dir < buffer || value.pw_dir >= buffer + sizeof buffer) return 3;
    result = (void *)1;
    rc = getpwuid_r(getuid(), &value, buffer, 1, &result);
    if (rc != ERANGE || result) return 4;
    result = (void *)1;
    rc = getpwuid_r((uid_t)-2, &value, buffer, sizeof buffer, &result);
    if (rc != missing_code || (result != 0) != missing_present) return 5;
    return 0;
}
"#,
    )
    .unwrap();
    let probe_lib = frameworks.join("libprobe.dylib");
    run(Command::new("clang")
        .args(["-arch", "arm64", "-dynamiclib"])
        .arg(&probe)
        .arg("-o")
        .arg(&probe_lib)
        .args([
            "-install_name",
            "@executable_path/../Frameworks/libprobe.dylib",
        ]));
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&probe_lib));
    let source = scratch.path().join("host.c");
    fs::write(
        &source,
        r#"
#include <pwd.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
extern int ParsecOrbitMacosHarnessHome(const char *);
extern int probe_home(const char *);
extern int probe_baseline(void);
int main(int argc, char **argv) {
    if (argc != 2) return 10;
    probe_baseline();
    struct passwd *p = getpwuid(getuid());
    if (!p) return 11;
    if (!strcmp(p->pw_dir, argv[1])) {
        // The product constructor already armed the isolated home; a separate
        // image must observe it through both NSS entry points.
        int rc = probe_home(argv[1]);
        if (rc) return 30 + rc;
        puts("SELECTED_HOST_MAIN_OK");
        return 0;
    }
    if (ParsecOrbitMacosHarnessHome(argv[1])) return 12;
    int rc = probe_home(argv[1]);
    if (rc) return 20 + rc;
    p = getpwuid(getuid());
    if (!p || strcmp(p->pw_dir, argv[1])) return 13;
    puts("NSS_MAIN_AND_SEPARATE_IMAGE_OK");
    return 0;
}
"#,
    )
    .unwrap();
    let host = macos.join("probe");
    run(Command::new("clang")
        .args(["-arch", "arm64"])
        .arg(&source)
        .arg(&loader)
        .arg(&probe_lib)
        .args(["-Wl,-rpath,@executable_path/../Frameworks", "-o"])
        .arg(&host));
    // Rewrite the loader's build-directory install name on this owned toy
    // executable. No instruction/vendor bytes are part of this fixture.
    let install_id = Command::new("otool")
        .arg("-D")
        .arg(&loader)
        .output()
        .unwrap();
    assert!(install_id.status.success());
    let install_id = String::from_utf8(install_id.stdout).unwrap();
    let build_name = install_id.lines().nth(1).expect("loader install id").trim();
    run(Command::new("install_name_tool")
        .arg("-change")
        .arg(build_name)
        .arg("@executable_path/../Frameworks/libparsec_orbit_loader.dylib")
        .arg(&host));
    let entitlements = scratch.path().join("entitlements.plist");
    fs::write(&entitlements, "<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>com.apple.security.cs.disable-library-validation</key><true/></dict></plist>").unwrap();
    run(Command::new("codesign")
        .args([
            "--force",
            "--sign",
            "-",
            "--options",
            "runtime",
            "--entitlements",
        ])
        .arg(&entitlements)
        .arg(&host));
    run(Command::new("codesign")
        .args(["--verify", "--strict"])
        .arg(&host));
    let output = Command::new(&host)
        .arg("/private/orbit-fixture/home")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NSS_MAIN_AND_SEPARATE_IMAGE_OK"));
    assert!(
        !contents.join("Resources").exists(),
        "inactive host must not create records"
    );
    let selected = macos.join("parsecd");
    fs::copy(&host, &selected).unwrap();
    let output = Command::new(&selected)
        .arg("/private/orbit-fixture/home")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(78),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "selected host must not enter main"
    );
    // A valid record activates the guarded host: the constructor installs the
    // redirect, the isolation profile, the sentinel gate and the home
    // replacement, then vendor main runs.
    let orbit = scratch.path().join("orbit");
    fs::create_dir(&orbit).unwrap();
    fs::create_dir(orbit.join("home")).unwrap();
    for path in [&orbit, &orbit.join("home")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::create_dir(contents.join("Resources")).unwrap();
    let record = serde_json::json!({"schema_version": 1, "executable": fs::canonicalize(&selected).unwrap(),
        "loader": fs::canonicalize(&loader).unwrap(), "orbit_root": fs::canonicalize(&orbit).unwrap(), "uid": unsafe { libc::getuid() }});
    let path = contents.join("Resources/orbit-install.json");
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let isolated = fs::canonicalize(orbit.join("home")).unwrap();
    let output = Command::new(&selected).arg(&isolated).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("SELECTED_HOST_MAIN_OK"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    // The debug loader also mirrors its file log to stderr; a constructor
    // failure would use the `Parsec Orbit:` prefix instead.
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("Parsec Orbit:"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // An invalid binding still refuses before main.
    let wrong_uid = serde_json::json!({"schema_version": 1, "executable": fs::canonicalize(&selected).unwrap(),
        "loader": fs::canonicalize(&loader).unwrap(), "orbit_root": fs::canonicalize(&orbit).unwrap(),
        "uid": unsafe { libc::getuid() } + 1});
    fs::write(&path, serde_json::to_vec(&wrong_uid).unwrap()).unwrap();
    let output = Command::new(&selected).arg(&isolated).output().unwrap();
    assert_eq!(output.status.code(), Some(78));
    assert!(output.stdout.is_empty());
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let leftovers: Vec<_> = fs::read_dir(&orbit)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.to_string_lossy().contains(".isolation-sentinel")),
        "{leftovers:?}"
    );
    // Strong loading is the selected safety policy: a missing dependency
    // must not let the unprotected vendor entry continue with real state.
    let withheld = frameworks.join("withheld-loader.dylib");
    fs::rename(&loader, &withheld).unwrap();
    let missing = Command::new(&selected).output().unwrap();
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    fs::write(&loader, b"not a Mach-O image").unwrap();
    let corrupt = Command::new(&selected).output().unwrap();
    assert!(!corrupt.status.success());
    assert!(corrupt.stdout.is_empty());
    fs::remove_file(&loader).unwrap();
    fs::rename(withheld, loader).unwrap();
}
