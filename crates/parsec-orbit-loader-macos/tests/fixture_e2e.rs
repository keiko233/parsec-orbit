//! The macOS end-to-end check of the formal platform code.
//!
//! Builds the loader dylib (with the harness entry points) and the std and
//! `no_std` SDK fixtures, then runs one scan from this test process through
//! the *dylib product*: the loader is `dlopen`ed, the fixtures are mapped by
//! its `dlopen` platform, `Query`/`Init` run and the registry is queried
//! again through the loader. The test process is a non-Parsec host; the real
//! bootstrap path is exercised separately with a patched app copy.
#![cfg(target_os = "macos")]

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::process::Command;

use parsec_orbit_core::testutil::TempDir;

// The macOS values (`dyld` headers).
const RTLD_NOW: i32 = 0x2;
const RTLD_LOCAL: i32 = 0x4;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type HarnessRun = extern "C" fn(*const c_char, *const c_char, *const c_char) -> i32;
type HarnessFind = extern "C" fn(*const c_char) -> i32;

fn workspace_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/parsec-orbit-loader-macos sits two levels below the workspace")
        .to_path_buf()
}

/// Builds the dylib and the fixtures into a private target directory, so the
/// nested cargo never waits for the lock of the cargo that runs the tests.
fn build_artifacts() -> PathBuf {
    let root = workspace_root();
    let target = root.join("target").join("macos-e2e");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
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
        .args(["-p", "parsec-orbit-fixture-plugin"])
        .args(["-p", "parsec-orbit-fixture-nostd"])
        .output()
        .expect("start cargo");
    assert!(
        output.status.success(),
        "building the artifacts failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    target.join("debug")
}

fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
    let pointer = unsafe { dlsym(handle, name.as_ptr()) };
    assert!(
        !pointer.is_null(),
        "{} is not exported",
        name.to_string_lossy()
    );
    pointer
}

/// Copies `source` into a fresh plugin directory with the given manifest.
fn plugin_dir(
    plugins: &std::path::Path,
    directory: &str,
    id: &str,
    source: &std::path::Path,
) -> PathBuf {
    let dir = plugins.join(directory);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = source.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::copy(source, dir.join(&entry)).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"Fixture\"\nversion = \"0.1.0\"\nkind = \"dylib\"\nentry = \"{entry}\"\n"
        ),
    )
    .unwrap();
    dir
}

#[test]
fn the_loader_maps_the_std_and_no_std_fixtures_through_the_registry() {
    let debug = build_artifacts();
    let loader = debug.join("libparsec_orbit_loader.dylib");
    let std_fixture = debug.join("libparsec_orbit_fixture_plugin.dylib");
    let nostd_fixture = debug.join("libparsec_orbit_fixture_nostd.dylib");
    for path in [&loader, &std_fixture, &nostd_fixture] {
        assert!(path.is_file(), "{} was not built", path.display());
    }

    let dir = TempDir::new("macos-e2e");
    let root = dir.path().to_path_buf();
    let plugins = root.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();

    // The std fixture: scripted to register an interface for the assertions.
    let std_dir = plugin_dir(&plugins, "a-std", "com.example.fixture", &std_fixture);
    std::fs::write(
        std_dir.join("fixture.cfg"),
        "register=com.example.macos-e2e\n",
    )
    .unwrap();

    // The no_std fixture, twice: the allocator/alignment path and the big
    // stack frame path (which on macOS goes through the system's
    // `___chkstk_darwin`, not the Windows stand-in).
    plugin_dir(
        &plugins,
        "b-nostd-alloc",
        "com.example.fixture-nostd.alloc",
        &nostd_fixture,
    );
    plugin_dir(
        &plugins,
        "c-nostd-stack",
        "com.example.fixture-nostd.stack",
        &nostd_fixture,
    );

    // A Windows-native plugin on macOS: skipped, never mapped.
    let foreign = plugins.join("d-windows");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(
        foreign.join("plugin.toml"),
        "id = \"com.example.windows\"\nkind = \"dll\"\nentry = \"explicit.dll\"\n",
    )
    .unwrap();

    let data_root = root.join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let log = root.join("parsec-orbit.log");

    // The loader dylib is mapped with the same flags it will see inside the
    // patched app; it is never dlclosed (no unload).
    let loader_path = CString::new(loader.to_string_lossy().as_bytes()).unwrap();
    let handle = unsafe { dlopen(loader_path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
    assert!(!handle.is_null(), "could not dlopen the loader dylib");

    let run: HarnessRun = unsafe {
        std::mem::transmute::<*mut c_void, HarnessRun>(symbol(
            handle,
            c"ParsecOrbitMacosHarnessRun",
        ))
    };
    let find: HarnessFind = unsafe {
        std::mem::transmute::<*mut c_void, HarnessFind>(symbol(
            handle,
            c"ParsecOrbitMacosHarnessFind",
        ))
    };

    let log_arg = CString::new(log.to_string_lossy().as_bytes()).unwrap();
    let dirs_arg = CString::new(plugins.to_string_lossy().as_bytes()).unwrap();
    let data_arg = CString::new(data_root.to_string_lossy().as_bytes()).unwrap();
    let failed = run(log_arg.as_ptr(), dirs_arg.as_ptr(), data_arg.as_ptr());
    assert_eq!(failed, 0, "a plugin failed to load:\n{log:?}");

    let content = std::fs::read_to_string(&log).unwrap();
    for needle in [
        // Discovery, mapping and lifecycle of the fixtures.
        "loaded com.example.fixture 0.1.0 (Fixture) ABI 1",
        "loaded com.example.fixture-nostd.alloc",
        "loaded com.example.fixture-nostd.stack",
        // The std fixture ran against the revision-2 host table and its data
        // directory ended with its id.
        "fixture[com.example.fixture]: revision2=true",
        "data_dir=ends-with-id",
        // Real registration through the loader's registry.
        "fixture[com.example.fixture]: register com.example.macos-e2e -> 0",
        // The no_std fixture exercised the libSystem allocator and the
        // over-aligned block path.
        "fixture-nostd: alloc ok sum=199990000 zeros=true text=140 copy=true aligned=true",
        // ... and a 200 KB stack frame.
        "fixture-nostd: stack ok sum=",
        // The other platform's native kind is skipped, not mapped.
        "skipped, kind \"dll\" is not supported on macOS (use \"dylib\")",
        "ready: 3 loaded, 1 skipped, 0 failed",
    ] {
        assert!(
            content.contains(needle),
            "the log does not contain {needle:?}:\n{content}"
        );
    }

    assert_eq!(
        find(c"com.example.macos-e2e".as_ptr()),
        1,
        "the interface the fixture registered is in the loader's registry:\n{content}"
    );
    assert_eq!(find(c"com.example.not-registered".as_ptr()), 0);
}
