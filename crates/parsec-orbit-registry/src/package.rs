//! Plugin packages: zip files that hold one plugin folder.
//!
//! ```text
//! my-plugin/plugin.toml
//! my-plugin/my_plugin.dll
//! ```
//!
//! A package comes from the network, so nothing in it is trusted: names are
//! checked before anything is written, sizes are bounded by what is actually
//! read (not by what the headers claim), and the folder has to be a plugin
//! the index promised.

use std::io::Read;
use std::path::Path;

use crate::index::valid_id;
use crate::Error;

/// Entries a package may hold.
pub const MAX_ENTRIES: usize = 4096;
/// Bytes of one file, uncompressed.
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
/// Bytes of a whole package, uncompressed.
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// A file of the plugin folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Path below the plugin folder, with `/`.
    pub path: String,
    pub data: Vec<u8>,
}

/// What the package's `plugin.toml` says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub entry: Option<String>,
    pub requires: Vec<String>,
    pub name: String,
    pub description: String,
    pub author: String,
    pub license: String,
    /// Loader ABI version the plugin needs.
    pub api: u32,
}

/// A package, read and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Name of the folder in the zip.
    pub folder: String,
    pub files: Vec<File>,
    pub manifest: Manifest,
}

fn reserved(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

/// The components of a path from a zip, or `None` if it is not a plain
/// relative path that is safe on Windows too.
pub fn safe_components(name: &str) -> Option<Vec<&str>> {
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.is_empty() || name.contains('\\') || name.starts_with('/') {
        return None;
    }
    let components: Vec<&str> = trimmed.split('/').collect();
    for component in &components {
        if component.is_empty()
            || *component == "."
            || *component == ".."
            || component.ends_with('.')
            || component.ends_with(' ')
            || reserved(component)
            || component
                .chars()
                .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return None;
        }
    }
    Some(components)
}

/// Reads `plugin.toml` text.
pub fn parse_manifest(text: &str) -> Result<Manifest, Error> {
    let document = toml::de::DeTable::parse(text)
        .map_err(|error| Error::Package(format!("plugin.toml: {error}")))?;
    let table = document.get_ref();
    let string = |key: &str| {
        table
            .get(key)
            .and_then(|value| value.get_ref().as_str())
            .map(str::to_string)
    };
    let requires = table
        .get("requires")
        .and_then(|value| value.get_ref().as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get_ref().as_str())
                // `id@constraint` is reserved syntax; the id is what counts.
                .map(|item| item.split('@').next().unwrap_or("").trim().to_string())
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(Manifest {
        id: string("id").unwrap_or_default(),
        version: string("version").unwrap_or_default(),
        kind: string("kind").unwrap_or_else(|| "dll".to_string()),
        entry: string("entry"),
        requires,
        name: string("name").unwrap_or_default(),
        description: string("description").unwrap_or_default(),
        author: string("author").unwrap_or_default(),
        license: string("license").unwrap_or_default(),
        api: table
            .get("api")
            .and_then(|value| value.get_ref().as_integer())
            .and_then(|value| value.as_str().parse::<u32>().ok())
            .unwrap_or(1),
    })
}

/// Reads a package from the bytes of a zip.
pub fn read(bytes: &[u8]) -> Result<Package, Error> {
    let bad = |message: String| Err(Error::Package(message));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| Error::Package(format!("not a zip file: {error}")))?;
    if archive.len() > MAX_ENTRIES {
        return bad(format!(
            "{} entries, at most {MAX_ENTRIES} are accepted",
            archive.len()
        ));
    }

    let mut folder: Option<String> = None;
    let mut files: Vec<File> = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| Error::Package(format!("entry {index}: {error}")))?;
        let name = entry
            .name()
            .map_err(|error| Error::Package(format!("entry {index}: {error}")))?
            .to_string();
        let Some(components) = safe_components(&name) else {
            return bad(format!("unsafe path in the package: {name:?}"));
        };
        // Links would let a later entry write somewhere else.
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return bad(format!("{name:?} is a symbolic link"));
        }
        let top = components[0];
        match &folder {
            None => {
                if !valid_id(top) {
                    return bad(format!("the folder name {top:?} is not acceptable"));
                }
                folder = Some(top.to_string());
            }
            Some(existing) if existing != top => {
                return bad(format!(
                    "the package holds more than one folder ({existing:?} and {top:?})"
                ));
            }
            Some(_) => {}
        }
        if entry.is_dir() {
            continue;
        }
        if components.len() < 2 {
            return bad(format!("{name:?} is outside the plugin folder"));
        }
        let mut data = Vec::new();
        let read = (&mut entry)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|error| Error::Package(format!("{name:?}: {error}")))?
            as u64;
        if read > MAX_FILE_BYTES {
            return bad(format!("{name:?} is larger than {MAX_FILE_BYTES} bytes"));
        }
        total += read;
        if total > MAX_TOTAL_BYTES {
            return bad(format!(
                "the package unpacks to more than {MAX_TOTAL_BYTES} bytes"
            ));
        }
        let path = components[1..].join("/");
        if files
            .iter()
            .any(|file| file.path.eq_ignore_ascii_case(&path))
        {
            return bad(format!("{path:?} appears twice"));
        }
        files.push(File { path, data });
    }

    let Some(folder) = folder else {
        return bad("the package is empty".to_string());
    };
    let manifest_file = files
        .iter()
        .find(|file| file.path == "plugin.toml")
        .ok_or_else(|| Error::Package(format!("{folder}/plugin.toml is missing")))?;
    let manifest = parse_manifest(
        std::str::from_utf8(&manifest_file.data)
            .map_err(|_| Error::Package("plugin.toml is not UTF-8".to_string()))?,
    )?;
    if !valid_id(&manifest.id) {
        return bad(format!(
            "plugin.toml has the id {:?}, which is not acceptable",
            manifest.id
        ));
    }
    if let Some(entry) = &manifest.entry {
        let Some(parts) = safe_components(entry) else {
            return bad(format!("plugin.toml names the unsafe entry {entry:?}"));
        };
        let path = parts.join("/");
        if !files
            .iter()
            .any(|file| file.path.eq_ignore_ascii_case(&path))
        {
            return bad(format!("the entry {entry:?} is not in the package"));
        }
    }
    Ok(Package {
        folder,
        files,
        manifest,
    })
}

/// Writes the files below `dir` (which must not exist yet).
pub fn write_to(package: &Package, dir: &Path) -> Result<(), Error> {
    std::fs::create_dir(dir).map_err(|error| Error::Io(format!("{}: {error}", dir.display())))?;
    for file in &package.files {
        let target = dir.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &file.data)
            .map_err(|error| Error::Io(format!("{}: {error}", target.display())))?;
    }
    Ok(())
}

/// Zips the plugin folder `dir` into a package (the folder name is kept).
pub fn pack(dir: &Path) -> Result<Vec<u8>, Error> {
    use std::io::Write;
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_id(name))
        .ok_or_else(|| {
            Error::Usage(format!(
                "{} is not a usable plugin folder name",
                dir.display()
            ))
        })?;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut stack = vec![dir.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else {
                paths.push(path);
            }
        }
    }
    paths.sort();
    for path in paths {
        let relative = path
            .strip_prefix(dir)
            .map_err(|_| Error::Io("path outside the folder".to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        writer
            .start_file(format!("{name}/{relative}"), options)
            .map_err(|error| Error::Package(error.to_string()))?;
        writer.write_all(&std::fs::read(&path)?)?;
    }
    let cursor = writer
        .finish()
        .map_err(|error| Error::Package(error.to_string()))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A zip with the given `(name, contents)` entries, in order.
    pub fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    pub const MANIFEST: &str =
        "id = \"com.example.p\"\nversion = \"1.0.0\"\nentry = \"p.dll\"\nrequires = [\"com.example.base@^1\"]\n";

    pub fn good() -> Vec<u8> {
        zip_of(&[
            ("p/", b""),
            ("p/plugin.toml", MANIFEST.as_bytes()),
            ("p/p.dll", b"MZ"),
            ("p/data/notes.txt", b"hello"),
        ])
    }

    #[test]
    fn a_good_package_is_read() {
        let package = read(&good()).unwrap();
        assert_eq!(package.folder, "p");
        assert_eq!(package.manifest.id, "com.example.p");
        assert_eq!(package.manifest.version, "1.0.0");
        assert_eq!(package.manifest.kind, "dll");
        assert_eq!(package.manifest.entry.as_deref(), Some("p.dll"));
        assert_eq!(package.manifest.requires, ["com.example.base"]);
        let paths: Vec<&str> = package.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["plugin.toml", "p.dll", "data/notes.txt"]);
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for name in [
            "../evil.dll",
            "p/../../evil.dll",
            "/abs/plugin.toml",
            "p\\plugin.toml",
            "C:/windows/x.dll",
            "p/a:b",
            "p/con.txt",
            "p/aux",
            "p/lpt1.dll",
            "p/trailing.",
            "p/space ",
            "p//double",
            "p/./x",
            "p/we?rd",
        ] {
            let zip = zip_of(&[(name, b"x")]);
            let error = read(&zip).unwrap_err().to_string();
            assert!(
                error.contains("unsafe path") || error.contains("not acceptable"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn the_package_must_hold_exactly_one_folder() {
        let two = zip_of(&[("a/plugin.toml", MANIFEST.as_bytes()), ("b/x.dll", b"x")]);
        assert!(read(&two)
            .unwrap_err()
            .to_string()
            .contains("more than one folder"));
        let loose = zip_of(&[("plugin.toml", MANIFEST.as_bytes())]);
        assert!(read(&loose).is_err());
        let empty = zip_of(&[]);
        assert!(read(&empty).unwrap_err().to_string().contains("empty"));
        let bad_folder = zip_of(&[("bad folder/plugin.toml", MANIFEST.as_bytes())]);
        assert!(read(&bad_folder).is_err());
    }

    #[test]
    fn the_manifest_and_entry_have_to_be_there_and_sane() {
        let no_manifest = zip_of(&[("p/p.dll", b"x")]);
        assert!(read(&no_manifest)
            .unwrap_err()
            .to_string()
            .contains("plugin.toml is missing"));
        let no_entry = zip_of(&[("p/plugin.toml", MANIFEST.as_bytes())]);
        assert!(read(&no_entry)
            .unwrap_err()
            .to_string()
            .contains("not in the package"));
        let evil_entry = zip_of(&[(
            "p/plugin.toml",
            b"id = \"com.example.p\"\nentry = \"../x.dll\"\n".as_slice(),
        )]);
        assert!(read(&evil_entry)
            .unwrap_err()
            .to_string()
            .contains("unsafe entry"));
        let no_id = zip_of(&[("p/plugin.toml", b"version = \"1\"\n".as_slice())]);
        assert!(read(&no_id)
            .unwrap_err()
            .to_string()
            .contains("not acceptable"));
        let bad_toml = zip_of(&[("p/plugin.toml", b"id = ".as_slice())]);
        assert!(read(&bad_toml).is_err());
    }

    #[test]
    fn duplicate_names_ignoring_case_are_refused() {
        let zip = zip_of(&[
            ("p/plugin.toml", MANIFEST.as_bytes()),
            ("p/P.dll", b"a"),
            ("p/p.DLL", b"b"),
        ]);
        assert!(read(&zip)
            .unwrap_err()
            .to_string()
            .contains("appears twice"));
    }

    #[test]
    fn not_a_zip_is_an_error() {
        assert!(read(b"MZ this is not a zip").is_err());
        assert!(read(b"").is_err());
    }

    #[test]
    fn a_file_larger_than_the_limit_is_refused_by_what_is_read() {
        // 129 MiB of zeros compresses to almost nothing: the headers say how
        // big it is, but the limit is on what is really read.
        let zeros = vec![0u8; (MAX_FILE_BYTES + 1) as usize];
        let zip = zip_of(&[("p/plugin.toml", MANIFEST.as_bytes()), ("p/p.dll", &zeros)]);
        assert!(zip.len() < 1024 * 1024);
        assert!(read(&zip).unwrap_err().to_string().contains("larger than"));
    }

    #[test]
    fn packing_and_reading_round_trip() {
        let dir = std::env::temp_dir().join(format!("parsec-orbit-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("p");
        std::fs::create_dir_all(folder.join("data")).unwrap();
        std::fs::write(folder.join("plugin.toml"), MANIFEST).unwrap();
        std::fs::write(folder.join("p.dll"), b"MZ").unwrap();
        std::fs::write(folder.join("data/notes.txt"), b"hello").unwrap();

        let package = read(&pack(&folder).unwrap()).unwrap();
        assert_eq!(package.folder, "p");
        assert_eq!(package.files.len(), 3);

        let target = dir.join("out");
        write_to(&package, &target).unwrap();
        assert_eq!(
            std::fs::read(target.join("data").join("notes.txt")).unwrap(),
            b"hello"
        );
        assert!(
            write_to(&package, &target).is_err(),
            "never into an existing folder"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
