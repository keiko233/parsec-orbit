#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn run(program: &str, args: &[&str], current_dir: Option<&Path>) -> Output {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(directory) = current_dir {
        command.current_dir(directory);
    }
    let output = command.output().unwrap_or_else(|error| {
        panic!("could not start {program}: {error}");
    });
    assert!(
        output.status.success(),
        "{program} {args:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("scratch paths are UTF-8")
}

#[test]
fn hardened_runtime_rebinds_owned_imports_and_late_images() {
    let root = std::env::temp_dir().join(format!(
        "parsec-orbit-hooks-scratch-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("create scratch root");
    println!("macOS hooks scratch root: {}", path_text(&root));

    let bridge_project = root.join("bridge-project");
    fs::create_dir_all(bridge_project.join("src")).expect("create bridge project");
    let crate_path = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::write(
        bridge_project.join("Cargo.toml"),
        format!(
            "[package]\nname = \"orbit-hooks-scratch-bridge\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[dependencies]\nparsec-orbit-plugin-hooks = {{ path = {:?} }}\n",
            path_text(crate_path)
        ),
    )
    .expect("write bridge manifest");
    fs::write(
        bridge_project.join("src/lib.rs"),
        r#"
use std::ffi::c_void;
use parsec_orbit_plugin_hooks::HOOKS;

#[no_mangle]
pub unsafe extern "C" fn orbit_hooks_resolve(module: *const u8, module_len: usize,
                                              symbol: *const u8, symbol_len: usize)
                                              -> *const c_void {
    unsafe { (HOOKS.resolve.unwrap())(module, module_len, symbol, symbol_len) }
}

#[no_mangle]
pub unsafe extern "C" fn orbit_hooks_install(target: *const c_void, detour: *const c_void,
                                              original: *mut *const c_void) -> i64 {
    unsafe { (HOOKS.install.unwrap())(target, detour, original) }
}

#[no_mangle]
pub unsafe extern "C" fn orbit_hooks_enable(id: i64) -> i32 {
    unsafe { (HOOKS.enable.unwrap())(id) }
}

#[no_mangle]
pub unsafe extern "C" fn orbit_hooks_disable(id: i64) -> i32 {
    unsafe { (HOOKS.disable.unwrap())(id) }
}

#[no_mangle]
pub unsafe extern "C" fn orbit_hooks_remove(id: i64) -> i32 {
    unsafe { (HOOKS.remove.unwrap())(id) }
}
"#,
    )
    .expect("write bridge source");

    let manifest = bridge_project.join("Cargo.toml");
    let target_dir = root.join("cargo-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let build_output = Command::new(cargo)
        .args([
            "build",
            "--release",
            "--offline",
            "--manifest-path",
            path_text(&manifest),
            "--target-dir",
            path_text(&target_dir),
        ])
        .env("RUSTFLAGS", "-C link-arg=-Wl,-no_fixup_chains")
        .output()
        .expect("build the hooks bridge");
    assert!(
        build_output.status.success(),
        "scratch bridge build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build_output.stdout),
        String::from_utf8_lossy(&build_output.stderr),
    );
    let bridge_artifact = target_dir.join("release/liborbit_hooks_scratch_bridge.dylib");
    assert!(
        bridge_artifact.is_file(),
        "missing bridge dylib at {}",
        path_text(&bridge_artifact)
    );
    let bridge = root.join("liborbit_hooks_scratch_bridge.dylib");
    fs::copy(&bridge_artifact, &bridge).expect("copy bridge beside scratch host");

    let owned_source = root.join("owned.c");
    fs::write(
        &owned_source,
        r#"
__attribute__((visibility("default"), noinline))
int orbit_owned_add(int value) { return value + 1; }
__attribute__((visibility("default"), noinline))
int orbit_owned_chained(int value) { return value + 3; }
"#,
    )
    .expect("write owned dylib source");
    let owned_dylib = root.join("libowned.dylib");
    run(
        "clang",
        &[
            "-dynamiclib",
            path_text(&owned_source),
            "-o",
            path_text(&owned_dylib),
            "-Wl,-install_name,@rpath/libowned.dylib",
            "-Wl,-no_fixup_chains",
        ],
        None,
    );

    let chained_source = root.join("chained.c");
    fs::write(
        &chained_source,
        r#"
extern int orbit_owned_chained(int);
__attribute__((visibility("default"), noinline))
int orbit_chained_call(int value) { return orbit_owned_chained(value); }
"#,
    )
    .expect("write chained-fixup dylib source");
    let chained_dylib = root.join("libchained.dylib");
    run(
        "clang",
        &[
            "-dynamiclib",
            path_text(&chained_source),
            "-L",
            path_text(&root),
            "-lowned",
            "-o",
            path_text(&chained_dylib),
            "-Wl,-install_name,@rpath/libchained.dylib",
            "-Wl,-rpath,@loader_path",
        ],
        None,
    );

    let lifetime_source = root.join("lifetime.c");
    fs::write(
        &lifetime_source,
        r#"
__attribute__((visibility("default"), noinline))
int orbit_lifetime(int value) { return value + 30; }
"#,
    )
    .expect("write lifetime dylib source");
    let lifetime_dylib = root.join("liblifetime.dylib");
    run(
        "clang",
        &[
            "-dynamiclib",
            path_text(&lifetime_source),
            "-o",
            path_text(&lifetime_dylib),
            "-Wl,-install_name,@rpath/liblifetime.dylib",
            "-Wl,-no_fixup_chains",
        ],
        None,
    );

    let late_lazy_source = root.join("late_lazy.c");
    fs::write(
        &late_lazy_source,
        r#"
extern int orbit_owned_add(int);
__attribute__((visibility("default"), noinline))
int orbit_late_lazy_call(int value) { return orbit_owned_add(value); }
"#,
    )
    .expect("write lazy late dylib source");
    let late_lazy_dylib = root.join("liblate_lazy.dylib");
    run(
        "clang",
        &[
            "-dynamiclib",
            path_text(&late_lazy_source),
            "-L",
            path_text(&root),
            "-lowned",
            "-o",
            path_text(&late_lazy_dylib),
            "-Wl,-install_name,@rpath/liblate_lazy.dylib",
            "-Wl,-rpath,@loader_path",
            "-Wl,-no_fixup_chains",
        ],
        None,
    );

    let app_source = root.join("main.c");
    fs::write(
        &app_source,
        r#"
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

extern int orbit_owned_add(int);
extern const void *orbit_hooks_resolve(const uint8_t *, size_t, const uint8_t *, size_t);
extern int64_t orbit_hooks_install(const void *, const void *, const void **);
extern int32_t orbit_hooks_enable(int64_t);
extern int32_t orbit_hooks_disable(int64_t);
extern int32_t orbit_hooks_remove(int64_t);

static int (*saved_original)(int);
static int orbit_detour(int value) { return saved_original(value) + 100; }
__attribute__((noinline)) static int local_function(int value) { return value + 7; }

#define REQUIRE(test, code) do { if (!(test)) { fprintf(stderr, "scratch assertion failed: %d\n", (code)); return (code); } } while (0)

int main(void) {
    REQUIRE(orbit_owned_add(5) == 6, 10); /* Resolve its lazy import first. */
    const void *target = orbit_hooks_resolve((const uint8_t *)"", 0,
                                             (const uint8_t *)"orbit_owned_add", 15);
    REQUIRE(target != NULL, 11);
    REQUIRE(orbit_hooks_resolve((const uint8_t *)"libowned.dylib", 14,
                                (const uint8_t *)"orbit_owned_add", 15) == target, 12);
    const void *original = NULL;
    int64_t hook = orbit_hooks_install(target, (const void *)orbit_detour, &original);
    REQUIRE(hook > 0 && original == target, 13);
    REQUIRE(orbit_hooks_install(target, (const void *)orbit_detour, NULL) == -2, 14);
    saved_original = (int (*)(int))original;
    REQUIRE(orbit_owned_add(5) == 6, 15); /* Installed hooks start disabled. */
    REQUIRE(orbit_hooks_enable(hook) == 0, 16);
    REQUIRE(orbit_owned_add(5) == 106, 17);
    REQUIRE(saved_original(5) == 6, 18);

    void *late_lazy = dlopen("./liblate_lazy.dylib", RTLD_LAZY | RTLD_LOCAL);
    REQUIRE(late_lazy != NULL, 55);
    int (*late_lazy_call)(int) = (int (*)(int))dlsym(late_lazy, "orbit_late_lazy_call");
    REQUIRE(late_lazy_call != NULL && late_lazy_call(8) == 109, 56);

    REQUIRE(orbit_hooks_disable(hook) == 0, 21);
    REQUIRE(orbit_owned_add(5) == 6 && late_lazy_call(8) == 9, 22);
    REQUIRE(orbit_hooks_enable(hook) == 0, 23);
    REQUIRE(orbit_owned_add(5) == 106 && late_lazy_call(8) == 109, 24);

    REQUIRE(orbit_hooks_remove(hook) == 0, 25);
    REQUIRE(orbit_owned_add(5) == 6 && late_lazy_call(8) == 9, 26);
    REQUIRE(saved_original(5) == 6, 27); /* The unmodified target stays valid. */

    void *chained = dlopen("./libchained.dylib", RTLD_LAZY | RTLD_LOCAL);
    REQUIRE(chained != NULL, 39);
    int (*chained_call)(int) = (int (*)(int))dlsym(chained, "orbit_chained_call");
    REQUIRE(chained_call != NULL && chained_call(4) == 7, 40);
    const void *chained_target = orbit_hooks_resolve((const uint8_t *)"libowned.dylib", 14,
                                                      (const uint8_t *)"orbit_owned_chained", 19);
    REQUIRE(chained_target != NULL, 41);
    const void *unchanged = (const void *)1;
    REQUIRE(orbit_hooks_install(chained_target, (const void *)orbit_detour, &unchanged) == -4, 42);
    REQUIRE(unchanged == (const void *)1, 43);

    void *lifetime = dlopen("./liblifetime.dylib", RTLD_LAZY | RTLD_LOCAL);
    REQUIRE(lifetime != NULL, 47);
    int (*lifetime_direct)(int) = (int (*)(int))dlsym(lifetime, "orbit_lifetime");
    REQUIRE(lifetime_direct != NULL && lifetime_direct(2) == 32, 48);
    const void *lifetime_resolved = orbit_hooks_resolve((const uint8_t *)"liblifetime.dylib", 17,
                                                         (const uint8_t *)"orbit_lifetime", 14);
    REQUIRE(lifetime_resolved == (const void *)lifetime_direct, 49);
    REQUIRE(dlclose(lifetime) == 0, 50);
    REQUIRE(((int (*)(int))lifetime_resolved)(2) == 32, 51); /* resolve pins the image. */

    const void *unused = NULL;
    REQUIRE(orbit_hooks_install((const void *)local_function,
                                (const void *)orbit_detour, &unused) == -4, 28);
    printf("HOOKS_SCRATCH_OK hardened=1 late_load=1 lifetime_pinned=1 chained_rejected=1 lifecycle=1 unsupported=-4\n");
    return 0;
}
"#,
    )
    .expect("write scratch executable source");
    let app = root.join("hooks-scratch");
    run(
        "clang",
        &[
            path_text(&app_source),
            "-L",
            path_text(&root),
            "-lowned",
            "-lorbit_hooks_scratch_bridge",
            "-o",
            path_text(&app),
            "-Wl,-rpath,@executable_path",
            "-Wl,-no_fixup_chains",
        ],
        None,
    );

    let entitlements = root.join("entitlements.plist");
    fs::write(
        &entitlements,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.cs.disable-library-validation</key><true/>
</dict></plist>
"#,
    )
    .expect("write the single library-validation entitlement");

    for dylib in [
        &owned_dylib,
        &late_lazy_dylib,
        &chained_dylib,
        &lifetime_dylib,
        &bridge,
    ] {
        run(
            "codesign",
            &[
                "--force",
                "--sign",
                "-",
                "--timestamp=none",
                path_text(dylib),
            ],
            None,
        );
    }
    run(
        "codesign",
        &[
            "--force",
            "--sign",
            "-",
            "--options",
            "runtime",
            "--entitlements",
            path_text(&entitlements),
            "--timestamp=none",
            path_text(&app),
        ],
        None,
    );
    run("codesign", &["--verify", "--strict", path_text(&app)], None);
    let signature = Command::new("codesign")
        .args(["--display", "--verbose=4", path_text(&app)])
        .output()
        .expect("inspect hardened runtime signature");
    assert!(
        signature.status.success(),
        "codesign could not inspect scratch host"
    );
    let signature_text = format!(
        "{}{}",
        String::from_utf8_lossy(&signature.stdout),
        String::from_utf8_lossy(&signature.stderr)
    );
    assert!(
        signature_text.contains("runtime"),
        "host is not signed for Hardened Runtime: {signature_text}"
    );
    let entitlements = Command::new("codesign")
        .args(["--display", "--entitlements", ":-", path_text(&app)])
        .output()
        .expect("inspect scratch host entitlements");
    assert!(
        entitlements.status.success(),
        "codesign could not inspect entitlements"
    );
    let entitlement_text = format!(
        "{}{}",
        String::from_utf8_lossy(&entitlements.stdout),
        String::from_utf8_lossy(&entitlements.stderr)
    );
    assert_eq!(
        entitlement_text.matches("<key>").count(),
        1,
        "scratch host must have only the library-validation entitlement: {entitlement_text}"
    );
    assert!(entitlement_text.contains("com.apple.security.cs.disable-library-validation"));

    let output = run(path_text(&app), &[], Some(&root));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("HOOKS_SCRATCH_OK hardened=1 late_load=1 lifetime_pinned=1 chained_rejected=1 lifecycle=1 unsupported=-4"),
        "scratch host did not report success: {stdout}"
    );
}
