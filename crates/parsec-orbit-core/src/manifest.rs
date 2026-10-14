//! `plugin.toml`.

use std::collections::HashSet;
use std::path::Path;

use parsec_orbit_plugin_abi::ABI_VERSION;

/// File name of the plugin manifest inside a plugin directory.
pub const MANIFEST: &str = "plugin.toml";

/// Plugin kinds named in the manifest.
pub const KIND_DLL: &str = "dll";
pub const KIND_DYLIB: &str = "dylib";
pub const KIND_JS: &str = "js";

/// Metadata read from `plugin.toml`. Every field is optional except the ones
/// the loader needs to pick an entry point; missing values fall back to
/// defaults that keep a hand-written plugin working.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub kind: Option<String>,
    pub entry: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub api: u32,
    pub enabled: bool,
    /// Plugin ids (or `provides` aliases) that have to be loaded before this
    /// plugin; a missing or failed one means this plugin is skipped.
    pub requires: Vec<String>,
    /// Soft dependencies: they only influence the load order.
    pub optional: Vec<String>,
    /// Virtual ids this plugin satisfies besides its own `id`.
    pub provides: Vec<String>,
    /// Capabilities requested by sandboxed kinds; ignored for the native
    /// kinds `dll` and `dylib`.
    pub permissions: Vec<String>,
    /// Things the loader tolerated but the author should know about, logged
    /// once when the manifest is accepted.
    pub warnings: Vec<String>,
    /// The manifest text as read, handed to kind handlers verbatim.
    pub source: String,
}

/// Splits `id@constraint` only when the constraint has a non-empty tail.
/// Constraints are reserved syntax: the id is used and the caller is told the
/// constraint was not applied.
fn bare_id(entry: &str) -> Option<(&str, bool)> {
    let (id, constraint) = entry.split_once('@')?;
    if constraint.trim().is_empty() {
        return None;
    }
    let id = id.trim();
    Some((id, !id.is_empty()))
}

/// Reads an array of ids, dropping junk with a warning. One copy for all four
/// lists: the loader is size sensitive (ADR 0004). Version constraints are
/// only meaningful on dependency fields; other lists keep `@` verbatim.
#[inline(never)]
fn id_list(
    table: &toml::de::DeTable<'_>,
    key: &str,
    allow_constraints: bool,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let Some(items) = table.get(key).and_then(|value| value.get_ref().as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let Some(entry) = item.get_ref().as_str() else {
            warnings.push(format!("`{key}` entry {index} is not a string, ignored"));
            continue;
        };
        let mut id = entry.trim();
        let mut constrained = false;
        if allow_constraints {
            if let Some((bare, found)) = bare_id(id) {
                id = bare;
                constrained = found;
            }
        }
        if id.is_empty() {
            warnings.push(format!("`{key}` entry {index} is empty, ignored"));
            continue;
        }
        if !seen.insert(id.to_string()) {
            warnings.push(format!(
                "`{key}` entry {index} \"{entry}\" repeats \"{id}\", ignored"
            ));
            continue;
        }
        if constrained {
            warnings.push(format!(
                "`{key}` entry {index} \"{entry}\": version constraints are not supported yet, using \"{id}\""
            ));
        }
        out.push(id.to_string());
    }
    out
}

/// Parses `plugin.toml`.
pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let source = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let document = toml::de::DeTable::parse(&source).map_err(|error| error.to_string())?;
    let table = document.get_ref();

    let string = |key: &str| {
        table
            .get(key)
            .and_then(|value| value.get_ref().as_str())
            .map(str::to_string)
    };

    let mut warnings = Vec::new();
    let requires = id_list(table, "requires", true, &mut warnings);
    let optional = id_list(table, "optional", true, &mut warnings);
    let provides = id_list(table, "provides", false, &mut warnings);
    let permissions = id_list(table, "permissions", false, &mut warnings);

    Ok(Manifest {
        id: string("id"),
        name: string("name"),
        version: string("version"),
        kind: string("kind"),
        entry: string("entry"),
        author: string("author"),
        description: string("description"),
        license: string("license"),
        api: table
            .get("api")
            .and_then(|value| value.get_ref().as_integer())
            .map(
                |value| match i64::from_str_radix(value.as_str(), value.radix()) {
                    // Invalid or overflowing values must be incompatible rather than
                    // truncate to a value the loader happens to implement.
                    Ok(api) if api >= 0 => u32::try_from(api).unwrap_or(u32::MAX),
                    _ => u32::MAX,
                },
            )
            .unwrap_or(ABI_VERSION),
        enabled: table
            .get("enabled")
            .and_then(|value| value.get_ref().as_bool())
            .unwrap_or(true),
        requires,
        optional,
        provides,
        permissions,
        warnings,
        source,
    })
}

pub fn describe(manifest: Option<&Manifest>, path: &Path) -> String {
    match manifest {
        Some(manifest) => format!(
            "{} {}",
            manifest
                .id
                .clone()
                .unwrap_or_else(|| path.display().to_string()),
            manifest.version.clone().unwrap_or_else(|| "-".into())
        ),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn manifest_of(toml: &str) -> Result<Manifest, String> {
        let dir = TempDir::new("manifest");
        let path = dir.write(MANIFEST, toml);
        read_manifest(&path)
    }

    #[test]
    fn manifest_reads_every_field() {
        let manifest = manifest_of(
            r#"
            id = "com.example.full"
            name = "Full"
            version = "1.2.3"
            kind = "dll"
            entry = "full.dll"
            author = "Someone"
            description = "Everything set"
            license = "MIT"
            api = 1
            enabled = false
            "#,
        )
        .unwrap();
        assert_eq!(manifest.id.as_deref(), Some("com.example.full"));
        assert_eq!(manifest.name.as_deref(), Some("Full"));
        assert_eq!(manifest.version.as_deref(), Some("1.2.3"));
        assert_eq!(manifest.kind.as_deref(), Some("dll"));
        assert_eq!(manifest.entry.as_deref(), Some("full.dll"));
        assert_eq!(manifest.author.as_deref(), Some("Someone"));
        assert_eq!(manifest.description.as_deref(), Some("Everything set"));
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.api, 1);
        assert!(!manifest.enabled);
    }

    #[test]
    fn manifest_defaults_match_the_documented_ones() {
        let manifest = manifest_of("").unwrap();
        assert_eq!(manifest.id, None);
        assert_eq!(manifest.kind, None);
        assert_eq!(manifest.entry, None);
        assert_eq!(manifest.api, ABI_VERSION);
        assert!(manifest.enabled);
    }

    #[test]
    fn manifest_ignores_values_of_the_wrong_type() {
        let manifest = manifest_of("id = 5\nname = true\nenabled = \"no\"\napi = \"1\"\n").unwrap();
        assert_eq!(manifest.id, None);
        assert_eq!(manifest.name, None);
        assert!(manifest.enabled, "a non-bool `enabled` keeps the default");
        assert_eq!(manifest.api, ABI_VERSION);
    }

    #[test]
    fn manifest_with_a_negative_api_is_not_accepted_as_compatible() {
        let manifest = manifest_of("api = -1\n").unwrap();
        assert!(manifest.api > ABI_VERSION);
    }

    #[test]
    fn manifest_api_uses_the_toml_radix() {
        assert_eq!(manifest_of("api = 0x10\n").unwrap().api, 16);
    }

    #[test]
    fn manifest_api_rejects_values_outside_u32() {
        assert!(manifest_of("api = 4294967296\n").unwrap().api > ABI_VERSION);
        assert!(manifest_of("api = 18446744073709551616\n").unwrap().api > ABI_VERSION);
    }

    #[test]
    fn manifest_errors_are_reported_not_panicked() {
        assert!(manifest_of("id = ").is_err());
        assert!(manifest_of("[unterminated").is_err());
        let dir = TempDir::new("manifest-missing");
        assert!(read_manifest(&dir.path().join(MANIFEST)).is_err());
    }

    #[test]
    fn describe_prefers_the_manifest_id_and_version() {
        let path = Path::new("C:\\plugins\\x");
        let manifest = Manifest {
            id: Some("com.example.x".into()),
            version: Some("2.0.0".into()),
            ..Manifest::default()
        };
        assert_eq!(describe(Some(&manifest), path), "com.example.x 2.0.0");

        let bare = Manifest::default();
        assert_eq!(describe(Some(&bare), path), "C:\\plugins\\x -");
        assert_eq!(describe(None, path), "C:\\plugins\\x");
    }

    #[test]
    fn plugin_kinds_are_the_documented_ones() {
        assert_eq!(KIND_DLL, "dll");
        assert_eq!(KIND_DYLIB, "dylib");
        assert_eq!(KIND_JS, "js");
        assert_eq!(MANIFEST, "plugin.toml");
    }

    #[test]
    fn manifest_reads_the_dependency_lists() {
        let manifest = manifest_of(
            r#"
            id = "com.example.js-thing"
            requires = ["com.parsec-orbit.js-runtime", "com.example.base"]
            optional = ["com.example.nice"]
            provides = ["com.example.alias"]
            permissions = ["webview", "config"]
            "#,
        )
        .unwrap();
        assert_eq!(
            manifest.requires,
            ["com.parsec-orbit.js-runtime", "com.example.base"]
        );
        assert_eq!(manifest.optional, ["com.example.nice"]);
        assert_eq!(manifest.provides, ["com.example.alias"]);
        assert_eq!(manifest.permissions, ["webview", "config"]);
        assert!(manifest.warnings.is_empty());
    }

    #[test]
    fn manifest_lists_default_to_empty() {
        let manifest = manifest_of(
            "id = \"x\"
",
        )
        .unwrap();
        assert!(manifest.requires.is_empty());
        assert!(manifest.optional.is_empty());
        assert!(manifest.provides.is_empty());
        assert!(manifest.permissions.is_empty());
        assert!(manifest.warnings.is_empty());
    }

    #[test]
    fn version_constraints_are_reserved_and_fall_back_to_the_bare_id() {
        let manifest =
            manifest_of(r#"requires = ["com.example.base@^0.1", "com.example.other"]"#).unwrap();
        assert_eq!(manifest.requires, ["com.example.base", "com.example.other"]);
        assert_eq!(manifest.warnings.len(), 1);
        assert!(
            manifest.warnings[0].contains("version constraints are not supported"),
            "{:?}",
            manifest.warnings
        );
    }

    #[test]
    fn dependency_lists_drop_junk_and_duplicates_with_a_warning() {
        let manifest = manifest_of(r#"requires = ["a", 5, "", " a ", "b"]"#).unwrap();
        assert_eq!(manifest.requires, ["a", "b"]);
        assert_eq!(manifest.warnings.len(), 3, "{:?}", manifest.warnings);
        assert!(manifest
            .warnings
            .iter()
            .any(|warning| warning.contains("repeats \"a\"")));
    }

    #[test]
    fn constraints_are_only_stripped_from_dependency_fields() {
        let manifest = manifest_of(
            r#"
            requires = ["com.example.base@^1"]
            optional = ["com.example.opt@2"]
            provides = ["net@v2"]
            permissions = ["net@v2", "webview"]
            "#,
        )
        .unwrap();
        assert_eq!(manifest.requires, ["com.example.base"]);
        assert_eq!(manifest.optional, ["com.example.opt"]);
        assert_eq!(manifest.provides, ["net@v2"]);
        assert_eq!(manifest.permissions, ["net@v2", "webview"]);
        assert_eq!(manifest.warnings.len(), 2, "{:?}", manifest.warnings);
    }

    #[test]
    fn a_constraint_without_a_value_is_kept_as_part_of_the_id() {
        let manifest = manifest_of(r#"requires = ["a@", "@v1"]"#).unwrap();
        assert_eq!(manifest.requires, ["a@"]);
        assert_eq!(manifest.warnings.len(), 1, "{:?}", manifest.warnings);
        assert!(manifest.warnings[0].contains("empty"));
    }

    #[test]
    fn constrained_duplicates_are_dropped_without_a_misleading_warning() {
        let manifest = manifest_of(r#"requires = ["a@^1", "a@^2"]"#).unwrap();
        assert_eq!(manifest.requires, ["a"]);
        assert_eq!(manifest.warnings.len(), 2, "{:?}", manifest.warnings);
        assert!(manifest
            .warnings
            .iter()
            .any(|warning| warning.contains("repeats \"a\"")));
    }

    #[test]
    fn a_dependency_field_that_is_not_an_array_is_ignored() {
        let manifest = manifest_of(r#"requires = "com.example.base""#).unwrap();
        assert!(manifest.requires.is_empty());
    }

    #[test]
    fn the_manifest_text_is_kept_for_kind_handlers() {
        let text = "id = \"keep\"
# a comment
";
        assert_eq!(manifest_of(text).unwrap().source, text);
    }
}
