//! `plugin.toml`.

use std::path::Path;

use parsec_orbit_plugin_abi::ABI_VERSION;

/// File name of the plugin manifest inside a plugin directory.
pub const MANIFEST: &str = "plugin.toml";

/// Plugin kinds named in the manifest.
pub const KIND_DLL: &str = "dll";
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
        assert_eq!(KIND_JS, "js");
        assert_eq!(MANIFEST, "plugin.toml");
    }
}
