//! The registries a client reads: `<Orbit root>/market/sources.toml`.
//!
//! ```toml
//! [[source]]
//! name = "Parsec Orbit"
//! location = "https://example.com/orbit/index.json"
//! ```
//!
//! Names are unique and not empty; a location is whatever [`crate::source::Fetch`]
//! accepts. A missing file is an empty list, so a client with no sources offers
//! nothing rather than failing. The specification is `docs/spec/registry.md`.

use std::path::Path;

use serde::Deserialize;

use crate::Error;

/// The file in the market folder that lists the sources.
pub const SOURCES_TOML: &str = "sources.toml";

/// A registry index this client may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Shown to the user; unique among the sources.
    pub name: String,
    /// Where the index is (`https://`, or a path to a file).
    pub location: String,
}

#[derive(Deserialize)]
struct SourceList {
    #[serde(default)]
    source: Vec<SourceTable>,
}

#[derive(Deserialize)]
struct SourceTable {
    name: String,
    location: String,
}

/// Reads a `sources.toml`. Unknown keys are ignored, as elsewhere.
pub fn parse_sources_toml(text: &str) -> Result<Vec<Source>, Error> {
    let list: SourceList =
        toml::from_str(text).map_err(|error| Error::Io(format!("sources: {error}")))?;
    let mut sources: Vec<Source> = Vec::new();
    for table in list.source {
        if table.name.trim().is_empty() {
            return Err(Error::Io("a source has an empty name".to_string()));
        }
        if table.location.trim().is_empty() {
            return Err(Error::Io(format!(
                "source {:?} has no location",
                table.name
            )));
        }
        if sources.iter().any(|source| source.name == table.name) {
            return Err(Error::Io(format!(
                "source {:?} is listed twice",
                table.name
            )));
        }
        sources.push(Source {
            name: table.name,
            location: table.location,
        });
    }
    Ok(sources)
}

/// The sources listed in `dir/sources.toml`; no file is no sources.
pub fn load_sources(dir: &Path) -> Result<Vec<Source>, Error> {
    let path = dir.join(SOURCES_TOML);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_sources_toml(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(Error::Io(format!("{}: {error}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_read_in_file_order() {
        let text = r#"
            [[source]]
            name = "Parsec Orbit"
            location = "https://example.com/orbit/index.json"

            [[source]]
            name = "Local"
            location = "C:\\market\\index.json"
            comment = "ignored"
        "#;
        let sources = parse_sources_toml(text).unwrap();
        assert_eq!(
            sources,
            [
                Source {
                    name: "Parsec Orbit".into(),
                    location: "https://example.com/orbit/index.json".into(),
                },
                Source {
                    name: "Local".into(),
                    location: "C:\\market\\index.json".into(),
                },
            ]
        );
    }

    #[test]
    fn an_empty_file_lists_nothing() {
        assert!(parse_sources_toml("").unwrap().is_empty());
        assert!(parse_sources_toml("# no sources yet\n").unwrap().is_empty());
    }

    #[test]
    fn broken_source_lists_are_refused_with_a_reason() {
        let refused = |text: &str, needle: &str| {
            let error = parse_sources_toml(text).unwrap_err().to_string();
            assert!(error.contains(needle), "{error}");
        };
        refused("[[source]\n", "sources:");
        refused(
            "[[source]]\nname = \" \"\nlocation = \"https://a.example/i.json\"\n",
            "empty name",
        );
        refused("[[source]]\nname = \"a\"\n", "missing field");
        refused("[[source]]\nname = \"a\"\nlocation = \"\"\n", "no location");
        refused(
            "[[source]]\nname = \"a\"\nlocation = \"https://a.example/1.json\"\n\
             [[source]]\nname = \"a\"\nlocation = \"https://a.example/2.json\"\n",
            "listed twice",
        );
    }

    #[test]
    fn a_missing_sources_file_is_an_empty_list() {
        let dir = std::env::temp_dir().join(format!(
            "parsec-orbit-sources-{}-missing",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load_sources(&dir).unwrap().is_empty());

        std::fs::write(
            dir.join(SOURCES_TOML),
            "[[source]]\nname = \"a\"\nlocation = \"https://a.example/i.json\"\n",
        )
        .unwrap();
        assert_eq!(load_sources(&dir).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
