//! The dependency law between the workspace crates
//! (`docs/design/restructure.md`, section 2.4).
//!
//! The crates are grouped by directory - `crates/` (sdk, host, shared), `libs/`
//! (service providers), `plugins/` (features), `tools/` - and a crate may only
//! depend on the groups below it. The check reads `cargo metadata`, so it runs
//! on every OS with `cargo test --workspace` and needs no script.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

struct Package {
    name: String,
    group: String,
    /// Workspace dependencies: (name, kind) with kind `normal`, `dev` or `build`.
    deps: Vec<(String, String)>,
}

fn workspace() -> BTreeMap<String, Package> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("metadata is JSON");
    let root = Path::new(metadata["workspace_root"].as_str().unwrap()).to_path_buf();
    let members: Vec<&str> = metadata["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();

    let mut packages = BTreeMap::new();
    for package in metadata["packages"].as_array().unwrap() {
        if !members.contains(&package["id"].as_str().unwrap()) {
            continue;
        }
        let path = Path::new(package["manifest_path"].as_str().unwrap());
        let relative = path.strip_prefix(&root).unwrap();
        let group = relative.components().next().unwrap().as_os_str();
        packages.insert(
            package["name"].as_str().unwrap().to_string(),
            Package {
                name: package["name"].as_str().unwrap().to_string(),
                group: group.to_string_lossy().into_owned(),
                deps: Vec::new(),
            },
        );
    }
    for package in metadata["packages"].as_array().unwrap() {
        let name = package["name"].as_str().unwrap();
        if !packages.contains_key(name) {
            continue;
        }
        let mut deps = Vec::new();
        for dep in package["dependencies"].as_array().unwrap() {
            let dep_name = dep["name"].as_str().unwrap();
            if dep.get("path").map_or(true, Value::is_null) || !packages.contains_key(dep_name) {
                continue;
            }
            let kind = dep["kind"].as_str().unwrap_or("normal").to_string();
            deps.push((dep_name.to_string(), kind));
        }
        packages.get_mut(name).unwrap().deps = deps;
    }
    packages
}

/// Normal and build dependencies; dev-dependencies are the tests' own business.
fn linked<'a>(package: &'a Package) -> impl Iterator<Item = &'a str> {
    package
        .deps
        .iter()
        .filter(|(_, kind)| kind != "dev")
        .map(|(name, _)| name.as_str())
}

const ABI: &str = "parsec-orbit-plugin-abi";
const SDK: &str = "parsec-orbit-plugin-sdk";
const TESTING: &str = "parsec-orbit-plugin-sdk-testing";
const CORE: &str = "parsec-orbit-core";

#[test]
fn the_sdk_stays_at_the_bottom() {
    let packages = workspace();
    let abi = &packages[ABI];
    assert_eq!(
        linked(abi).count(),
        0,
        "the ABI crate has no workspace dependency"
    );
    let sdk = &packages[SDK];
    assert_eq!(
        linked(sdk).collect::<Vec<_>>(),
        [ABI],
        "the SDK depends on the ABI crate only"
    );
}

#[test]
fn the_host_side_never_links_the_author_api() {
    let packages = workspace();
    for package in packages.values() {
        let host = package.name == CORE || package.name.starts_with("parsec-orbit-loader");
        if !host {
            continue;
        }
        for dep in linked(package) {
            assert!(
                dep != SDK
                    && dep != TESTING
                    && !packages[dep].group.starts_with("libs")
                    && !packages[dep].group.starts_with("plugins"),
                "{} must not link {dep}",
                package.name
            );
        }
    }
}

#[test]
fn libs_and_plugins_link_the_sdk_and_nothing_above_it() {
    let packages = workspace();
    for package in packages.values() {
        if package.group != "libs" && package.group != "plugins" {
            continue;
        }
        for dep in linked(package) {
            let group = packages[dep].group.as_str();
            // The market plugin may also use the registry library.
            let allowed = dep == SDK
                || dep == ABI
                || (package.name == "parsec-orbit-plugin-market"
                    && (dep == "parsec-orbit-registry" || dep == "parsec-orbit-layout"));
            assert!(
                allowed,
                "{} ({}) links {dep} ({group}); libs and plugins meet only through services",
                package.name, package.group
            );
        }
    }
}

#[test]
fn the_test_double_is_only_ever_a_dev_dependency() {
    let packages = workspace();
    for package in packages.values() {
        for (dep, kind) in &package.deps {
            if dep == TESTING {
                assert_eq!(
                    kind, "dev",
                    "{} links the std test double; it would pull std into a no_std build",
                    package.name
                );
            }
        }
    }
}

#[test]
fn a_tool_links_at_most_one_plugin_rlib() {
    let packages = workspace();
    for package in packages.values() {
        let plugins = linked(package)
            .filter(|dep| matches!(packages[*dep].group.as_str(), "libs" | "plugins"))
            .count();
        assert!(
            plugins <= 1,
            "{} links {plugins} plugin crates; every plugin exports the same ABI symbols and an LTO build fails",
            package.name
        );
    }
}
