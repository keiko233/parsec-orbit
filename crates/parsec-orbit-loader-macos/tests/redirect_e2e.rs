//! Shared-persistent redirect gates in a disposable C host. No vendor code or
//! real account path is used: the harness installs an owned scratch prefix.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use parsec_orbit_core::testutil::TempDir;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const HOST_C: &str = r#"
#include <dirent.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

extern int ParsecOrbitMacosHarnessRedirect(const char *, const char *);

static int join(char *out, size_t size, const char *root, const char *leaf) {
    int written = snprintf(out, size, "%s/%s", root, leaf);
    return written < 0 || (size_t)written >= size;
}

int main(int argc, char **argv) {
    if (argc != 4) return 90;
    const char *prefix = argv[1];
    const char *target = argv[2];
    if (!strcmp(argv[3], "install") && ParsecOrbitMacosHarnessRedirect(prefix, target)) return 91;
    if (strcmp(argv[3], "install") && strcmp(argv[3], "control")) return 92;
    char a[PATH_MAX], b[PATH_MAX], dir[PATH_MAX], escape[PATH_MAX];
    if (join(a, sizeof a, prefix, "a.txt") || join(b, sizeof b, prefix, "b.txt")
        || join(dir, sizeof dir, prefix, "dir")) return 93;
    int written = snprintf(escape, sizeof escape, "%s/../escape.txt", prefix);
    if (written < 0 || (size_t)written >= sizeof escape) return 94;

    int fd = open(a, O_CREAT | O_RDWR, 0600);
    if (fd < 0) return 10;
    if (write(fd, "redirect\n", 9) != 9) return 11;
    close(fd);

    FILE *file = fopen(a, "r");
    if (!file) return 12;
    char buffer[16] = {0};
    if (!fgets(buffer, (int)sizeof buffer, file)) return 13;
    fclose(file);
    if (strcmp(buffer, "redirect\n")) return 14;

    struct stat status;
    if (stat(a, &status)) return 15;
    if (!S_ISREG(status.st_mode)) return 16;
    if (access(a, F_OK)) return 17;
    if (mkdir(dir, 0700)) return 18;

    DIR *directory = opendir(prefix);
    if (!directory) return 19;
    int found = 0;
    struct dirent *entry;
    while ((entry = readdir(directory))) {
        if (!strcmp(entry->d_name, "a.txt")) found = 1;
    }
    closedir(directory);
    if (!found) return 20;

    if (rename(a, b)) return 21;
    if (access(b, F_OK)) return 22;

    // A `..` spelling is never rewritten and lands beside the prefix.
    int escaped = open(escape, O_CREAT | O_WRONLY, 0600);
    if (escaped < 0) return 23;
    close(escaped);

    puts("REDIRECT_FIXTURE_OK");
    return 0;
}
"#;

fn build_harness_host(scratch: &Path) -> PathBuf {
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
    let contents = scratch.join("Toy.app/Contents");
    let frameworks = contents.join("Frameworks");
    let macos = contents.join("MacOS");
    fs::create_dir_all(&frameworks).unwrap();
    fs::create_dir_all(&macos).unwrap();
    let loader = frameworks.join("libparsec_orbit_loader.dylib");
    fs::copy(target.join("debug/libparsec_orbit_loader.dylib"), &loader).unwrap();
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&loader));
    let source = scratch.join("host.c");
    fs::write(&source, HOST_C).unwrap();
    let host = macos.join("probe");
    run(Command::new("clang")
        .args(["-arch", "arm64", "-Wall", "-Werror"])
        .arg(&source)
        .arg(&loader)
        .args(["-Wl,-rpath,@executable_path/../Frameworks", "-o"])
        .arg(&host));
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
    let entitlements = scratch.join("entitlements.plist");
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
    host
}

fn entries(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn shared_persistent_redirect_rewrites_an_owned_prefix() {
    let scratch = TempDir::new("redirect");
    let host = build_harness_host(scratch.path());
    let root = scratch.path();

    let prefix = root.join("prefix");
    let nested = root.join("nested");
    let target = nested.join("target");
    fs::create_dir_all(&prefix).unwrap();
    fs::create_dir_all(&target).unwrap();
    let output = Command::new(&host)
        .arg(&prefix)
        .arg(&target)
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install run: {:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("REDIRECT_FIXTURE_OK"));
    assert_eq!(entries(&target), ["b.txt", "dir"]);
    assert!(entries(&prefix).is_empty(), "{:?}", entries(&prefix));
    assert!(root.join("escape.txt").exists());
    assert!(!nested.join("escape.txt").exists());

    // Control: the same host without the redirect keeps the literal paths.
    let prefix = root.join("control-prefix");
    let nested = root.join("control-nested");
    let target = nested.join("target");
    fs::create_dir_all(&prefix).unwrap();
    fs::create_dir_all(&target).unwrap();
    let output = Command::new(&host)
        .arg(&prefix)
        .arg(&target)
        .arg("control")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "control run: {:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(entries(&prefix), ["b.txt", "dir"]);
    assert!(entries(&target).is_empty(), "{:?}", entries(&target));
    assert!(root.join("escape.txt").exists());
}
