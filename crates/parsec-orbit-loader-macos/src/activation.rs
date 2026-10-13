//! Installation-bound activation for the macOS loader.
//!
//! Activation is limited to the loader dylib beside the patched `parsecd`
//! in one app bundle. The installer owns creation of the record and private
//! state directories; this module only validates them.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

const EXECUTABLE_NAME: &str = "parsecd";
const EXECUTABLE_DIRECTORY: &str = "MacOS";
const CONTENTS_DIRECTORY: &str = "Contents";
const LOADER_NAME: &str = "libparsec_orbit_loader.dylib";
const RECORD_DIRECTORY: &str = "Resources";
const RECORD_NAME: &str = "orbit-install.json";
const ISOLATED_HOME_NAME: &str = "home";
const MAX_RECORD_BYTES: u64 = 16 * 1024;

/// A validated installation and the private home exposed to its client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    /// Canonical path to the running `parsecd` executable.
    pub executable: PathBuf,
    /// Canonical path to the loader image reported by `dladdr`.
    pub loader: PathBuf,
    /// Canonical, private Orbit state root.
    pub orbit_root: PathBuf,
    /// Canonical, private home at `<orbit_root>/home`.
    pub isolated_home: PathBuf,
}

/// Checks whether this process is the installed sibling host and validates
/// the install record and its isolated state.
///
/// `loader` is the path for an address inside this dylib from `dladdr`, and
/// `executable` is the path returned by `current_exe`. An unrelated process
/// returns `Ok(None)` before the record or any state directory is inspected.
/// A recognized sibling host with a missing or invalid install fails closed.
/// This function never creates files or directories.
pub fn inspect(
    executable: &Path,
    loader: &Path,
    uid: u32,
    real_home: &Path,
) -> Result<Option<Activation>, String> {
    let canonical_executable = match fs::canonicalize(executable) {
        Ok(path) => path,
        Err(_) if bundle_contents(executable).is_none() => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot resolve executable {}: {error}",
                executable.display()
            ));
        }
    };
    let Some(contents) = bundle_contents(&canonical_executable) else {
        return Ok(None);
    };

    let canonical_loader = fs::canonicalize(loader)
        .map_err(|error| format!("cannot resolve loader {}: {error}", loader.display()))?;
    let expected_loader = contents.join("Frameworks").join(LOADER_NAME);
    if canonical_loader.as_os_str() != expected_loader.as_os_str() {
        return Err(format!(
            "loader {} is not the sibling {}",
            canonical_loader.display(),
            expected_loader.display()
        ));
    }

    let record_path = install_record_path(contents)?;
    let record = read_install_record(&record_path, uid)?;
    if record.schema_version != 1 {
        return Err(format!(
            "{} has unsupported schema_version {}",
            record_path.display(),
            record.schema_version
        ));
    }
    if record.uid != uid {
        return Err(format!(
            "{} is bound to uid {}, expected {uid}",
            record_path.display(),
            record.uid
        ));
    }

    let executable_text = canonical_path_text(&canonical_executable, "executable")?;
    if record.executable != executable_text {
        return Err(format!(
            "{} is bound to executable {}, expected {}",
            record_path.display(),
            record.executable,
            executable_text
        ));
    }
    let loader_text = canonical_path_text(&canonical_loader, "loader")?;
    if record.loader != loader_text {
        return Err(format!(
            "{} is bound to loader {}, expected {}",
            record_path.display(),
            record.loader,
            loader_text
        ));
    }

    let canonical_real_home = canonical_real_home(real_home)?;
    let orbit_root_path = Path::new(&record.orbit_root);
    reject_shared_state(orbit_root_path, &canonical_real_home)?;
    let orbit_root = canonical_absolute_directory(&record.orbit_root, "Orbit root", uid)?;

    let isolated_home_path = orbit_root.join(ISOLATED_HOME_NAME);
    let isolated_home = validate_private_directory(&isolated_home_path, "isolated home", uid)?;

    Ok(Some(Activation {
        executable: canonical_executable,
        loader: canonical_loader,
        orbit_root,
        isolated_home,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRecord {
    schema_version: u32,
    executable: String,
    loader: String,
    orbit_root: String,
    uid: u32,
}

/// Returns the `Contents` directory only for the one supported executable
/// location: `Contents/MacOS/parsecd`.
fn bundle_contents(executable: &Path) -> Option<&Path> {
    if executable.file_name()? != EXECUTABLE_NAME {
        return None;
    }
    let macos = executable.parent()?;
    if macos.file_name()? != EXECUTABLE_DIRECTORY {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != CONTENTS_DIRECTORY {
        return None;
    }
    Some(contents)
}

fn install_record_path(contents: &Path) -> Result<PathBuf, String> {
    let resources = contents.join(RECORD_DIRECTORY);
    let metadata = fs::symlink_metadata(&resources)
        .map_err(|error| format!("cannot inspect {}: {error}", resources.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{} must be a real Resources directory",
            resources.display()
        ));
    }
    let canonical_resources = fs::canonicalize(&resources)
        .map_err(|error| format!("cannot resolve {}: {error}", resources.display()))?;
    if canonical_resources.as_os_str() != resources.as_os_str() {
        return Err(format!("{} is not canonical", resources.display()));
    }
    Ok(resources.join(RECORD_NAME))
}

fn read_install_record(path: &Path, uid: u32) -> Result<InstallRecord, String> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = options
        .open(path)
        .map_err(|error| format!("cannot open install record {}: {error}", path.display()))?;
    validate_record_file(&file, path, uid)?;

    let mut contents = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| format!("cannot read install record {}: {error}", path.display()))?;
    if contents.len() as u64 > MAX_RECORD_BYTES {
        return Err(format!(
            "install record {} exceeds {MAX_RECORD_BYTES} bytes",
            path.display()
        ));
    }
    serde_json::from_slice(&contents)
        .map_err(|error| format!("invalid install record {}: {error}", path.display()))
}

fn validate_record_file(file: &File, path: &Path, uid: u32) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect install record {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "install record {} must be a regular file",
            path.display()
        ));
    }
    if metadata.uid() != 0 && metadata.uid() != uid {
        return Err(format!(
            "install record {} is owned by uid {}, expected root or {uid}",
            path.display(),
            metadata.uid()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "install record {} is group or world writable",
            path.display()
        ));
    }
    Ok(())
}

/// Requires an absolute canonical spelling whose every component is a real,
/// safe directory. Only the final directory must be owned by `uid` and mode
/// 0700; writable ancestors are accepted only when the sticky bit protects
/// their entries (as with canonical `/private/tmp` on macOS).
fn canonical_absolute_directory(value: &str, label: &str, uid: u32) -> Result<PathBuf, String> {
    validate_private_directory(Path::new(value), label, uid)
}

fn validate_private_directory(path: &Path, label: &str, uid: u32) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{label} must be absolute: {}", path.display()));
    }

    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => normalized.push(name),
            _ => return Err(format!("{label} has an unsafe path: {}", path.display())),
        }
    }
    if normalized.as_os_str() != path.as_os_str() {
        return Err(format!(
            "{label} {} is not a canonical absolute path",
            path.display()
        ));
    }

    let mut current = PathBuf::from("/");
    let mut final_metadata = inspect_directory_component(&current, label, uid)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current.push(name);
                final_metadata = inspect_directory_component(&current, label, uid)?;
            }
            _ => return Err(format!("{label} has an unsafe path: {}", path.display())),
        }
    }
    if final_metadata.uid() != uid {
        return Err(format!(
            "{label} {} is owned by uid {}, expected {uid}",
            path.display(),
            final_metadata.uid()
        ));
    }
    if final_metadata.mode() & 0o7777 != 0o700 {
        return Err(format!("{label} {} must have mode 0700", path.display()));
    }
    Ok(path.to_path_buf())
}

fn inspect_directory_component(path: &Path, label: &str, uid: u32) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect {label} ancestor {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{label} ancestor {} is not a real directory",
            path.display()
        ));
    }
    if metadata.uid() != 0 && metadata.uid() != uid {
        return Err(format!(
            "{label} ancestor {} is owned by uid {}, expected root or the current uid",
            path.display(),
            metadata.uid()
        ));
    }
    let mode = metadata.mode();
    if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        return Err(format!(
            "{label} ancestor {} is writable without the sticky bit",
            path.display()
        ));
    }
    Ok(metadata)
}

fn canonical_real_home(real_home: &Path) -> Result<PathBuf, String> {
    if !real_home.is_absolute() {
        return Err(format!(
            "real home must be absolute: {}",
            real_home.display()
        ));
    }
    fs::canonicalize(real_home)
        .map_err(|error| format!("cannot resolve real home {}: {error}", real_home.display()))
}

fn reject_shared_state(orbit_root: &Path, real_home: &Path) -> Result<(), String> {
    if orbit_root == real_home || real_home.starts_with(orbit_root) {
        return Err(format!(
            "Orbit root {} cannot be the real home or its ancestor",
            orbit_root.display()
        ));
    }

    // These are lexical exclusions. Do not resolve or stat account-state
    // paths here: this gate must not inspect real Parsec credentials. A
    // symlink alias can evade this list until the kernel guard is installed.
    let protected_paths = [
        real_home.join(".parsec"),
        real_home.join(".parsec-persistent"),
        real_home.join("Library/Keychains"),
        real_home.join("Library/Application Support/Parsec"),
        PathBuf::from("/Users/Shared/.parsec"),
        PathBuf::from("/Users/Shared/.parsec-persistent"),
        PathBuf::from("/Users/Shared/Library/Keychains"),
        PathBuf::from("/Users/Shared/Library/Application Support/Parsec"),
        PathBuf::from("/Library/Keychains"),
    ];
    for shared_state in protected_paths {
        if overlaps(orbit_root, &shared_state) {
            return Err(format!(
                "Orbit root {} overlaps shared state {}",
                orbit_root.display(),
                shared_state.display()
            ));
        }
    }
    Ok(())
}

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn canonical_path_text(path: &Path, label: &str) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("canonical {label} path is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;
    use serde_json::json;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::time::{Duration, Instant};

    struct Fixture {
        _temp: TempDir,
        executable: PathBuf,
        loader: PathBuf,
        record: PathBuf,
        orbit_root: PathBuf,
        real_home: PathBuf,
        uid: u32,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = TempDir::new("activation");
            let bundle = temp.path().join("Parsec.app");
            let executable = bundle.join("Contents/MacOS/parsecd");
            let loader = bundle.join("Contents/Frameworks").join(LOADER_NAME);
            let record = bundle.join("Contents/Resources").join(RECORD_NAME);
            fs::create_dir_all(executable.parent().expect("executable parent")).unwrap();
            fs::create_dir_all(loader.parent().expect("loader parent")).unwrap();
            fs::create_dir_all(record.parent().expect("record parent")).unwrap();
            fs::write(&executable, "stub").unwrap();
            fs::write(&loader, "loader").unwrap();

            let orbit_root = temp.path().join("orbit");
            let isolated_home = orbit_root.join(ISOLATED_HOME_NAME);
            fs::create_dir_all(&isolated_home).unwrap();
            set_mode(&orbit_root, 0o700);
            set_mode(&isolated_home, 0o700);

            let real_home = temp.path().join("real-home");
            fs::create_dir(&real_home).unwrap();
            set_mode(&real_home, 0o700);

            let uid = unsafe { libc::getuid() };
            let fixture = Self {
                executable: fs::canonicalize(executable).unwrap(),
                loader: fs::canonicalize(loader).unwrap(),
                record,
                orbit_root: fs::canonicalize(orbit_root).unwrap(),
                real_home: fs::canonicalize(real_home).unwrap(),
                uid,
                _temp: temp,
            };
            fixture.write_record(
                &fixture.executable,
                &fixture.loader,
                &fixture.orbit_root,
                fixture.uid,
            );
            fixture
        }

        fn write_record(&self, executable: &Path, loader: &Path, orbit_root: &Path, uid: u32) {
            let contents = json!({
                "schema_version": 1,
                "executable": executable.to_str().unwrap(),
                "loader": loader.to_str().unwrap(),
                "orbit_root": orbit_root.to_str().unwrap(),
                "uid": uid,
            });
            fs::write(&self.record, contents.to_string()).unwrap();
            set_mode(&self.record, 0o600);
        }

        fn inspect(&self) -> Result<Option<Activation>, String> {
            inspect(&self.executable, &self.loader, self.uid, &self.real_home)
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn unrelated_executable_is_inactive_without_a_record_or_loader() {
        let temp = TempDir::new("activation-inactive");
        let executable = temp.path().join("unrelated-process");
        assert_eq!(
            inspect(
                &executable,
                &temp.path().join("missing-loader.dylib"),
                unsafe { libc::getuid() },
                &temp.path().join("missing-home"),
            ),
            Ok(None)
        );
    }

    #[test]
    fn valid_sibling_record_returns_canonical_activation() {
        let fixture = Fixture::new();
        let activation = fixture.inspect().unwrap().unwrap();
        assert_eq!(activation.executable, fixture.executable);
        assert_eq!(activation.loader, fixture.loader);
        assert_eq!(activation.orbit_root, fixture.orbit_root);
        assert_eq!(
            activation.isolated_home,
            fixture.orbit_root.join(ISOLATED_HOME_NAME)
        );
    }

    #[test]
    fn wrong_executable_or_loader_binding_fails_closed() {
        let fixture = Fixture::new();
        let wrong_executable = fixture.executable.parent().unwrap().join("other-parsecd");
        fixture.write_record(
            &wrong_executable,
            &fixture.loader,
            &fixture.orbit_root,
            fixture.uid,
        );
        assert!(fixture.inspect().is_err());

        fixture.write_record(
            &fixture.executable,
            &fixture.orbit_root.join("wrong-loader.dylib"),
            &fixture.orbit_root,
            fixture.uid,
        );
        assert!(fixture.inspect().is_err());
    }

    #[test]
    fn missing_record_in_the_sibling_host_fails_closed() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.record).unwrap();
        assert!(fixture.inspect().is_err());
    }

    #[test]
    fn rejects_writable_and_symlinked_records() {
        let fixture = Fixture::new();
        set_mode(&fixture.record, 0o660);
        assert!(fixture.inspect().is_err());

        let target = fixture._temp.path().join("record-target.json");
        fs::rename(&fixture.record, &target).unwrap();
        symlink(&target, &fixture.record).unwrap();
        assert!(fixture.inspect().is_err());
    }

    #[test]
    fn rejects_relative_roots_and_non_private_permissions() {
        let fixture = Fixture::new();
        fixture.write_record(
            &fixture.executable,
            &fixture.loader,
            Path::new("relative/orbit"),
            fixture.uid,
        );
        assert!(fixture.inspect().is_err());

        fixture.write_record(
            &fixture.executable,
            &fixture.loader,
            &fixture.orbit_root,
            fixture.uid,
        );
        set_mode(&fixture.orbit_root, 0o755);
        assert!(fixture.inspect().is_err());
    }

    #[test]
    fn rejects_symlinked_isolated_home() {
        let fixture = Fixture::new();
        let isolated_home = fixture.orbit_root.join(ISOLATED_HOME_NAME);
        let target = fixture.orbit_root.join("real-home-target");
        fs::create_dir(&target).unwrap();
        set_mode(&target, 0o700);
        fs::remove_dir(&isolated_home).unwrap();
        symlink(&target, &isolated_home).unwrap();
        assert!(fixture.inspect().is_err());
    }

    #[test]
    fn rejects_a_symlinked_orbit_root() {
        let fixture = Fixture::new();
        let alias = fixture._temp.path().join("orbit-alias");
        symlink(&fixture.orbit_root, &alias).unwrap();
        fixture.write_record(&fixture.executable, &fixture.loader, &alias, fixture.uid);
        assert!(fixture.inspect().is_err());
    }

    #[test]
    fn rejects_fifo_install_records_without_blocking() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.record).unwrap();
        let path = CString::new(fixture.record.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);

        let started = Instant::now();
        assert!(fixture.inspect().is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn rejects_writable_nonsticky_ancestors_and_shared_state_roots() {
        let fixture = Fixture::new();
        let unsafe_parent = fixture._temp.path().join("unsafe-parent");
        let unsafe_root = unsafe_parent.join("orbit");
        let unsafe_home = unsafe_root.join(ISOLATED_HOME_NAME);
        fs::create_dir_all(&unsafe_home).unwrap();
        set_mode(&unsafe_parent, 0o777);
        set_mode(&unsafe_root, 0o700);
        set_mode(&unsafe_home, 0o700);
        let unsafe_root = fs::canonicalize(unsafe_root).unwrap();
        fixture.write_record(
            &fixture.executable,
            &fixture.loader,
            &unsafe_root,
            fixture.uid,
        );
        assert!(fixture.inspect().is_err());

        let shared_root = fixture.real_home.join(".parsec");
        fs::create_dir(&shared_root).unwrap();
        set_mode(&shared_root, 0o700);
        let shared_home = shared_root.join(ISOLATED_HOME_NAME);
        fs::create_dir(&shared_home).unwrap();
        set_mode(&shared_home, 0o700);
        fixture.write_record(
            &fixture.executable,
            &fixture.loader,
            &shared_root,
            fixture.uid,
        );
        assert!(fixture.inspect().is_err());

        for protected in [
            "/Users/Shared",
            "/Users/Shared/.parsec",
            "/Users/Shared/.parsec-persistent",
            "/Users/Shared/Library/Keychains",
            "/Users/Shared/Library/Application Support/Parsec",
            "/Library/Keychains",
        ] {
            assert!(
                reject_shared_state(Path::new(protected), &fixture.real_home).is_err(),
                "{protected} should be excluded"
            );
        }
        for protected in [
            fixture.real_home.join("Library/Keychains"),
            fixture.real_home.join("Library/Application Support/Parsec"),
        ] {
            assert!(reject_shared_state(&protected, &fixture.real_home).is_err());
        }
    }

    #[test]
    fn strict_record_rejects_unknown_fields() {
        let fixture = Fixture::new();
        let contents = json!({
            "schema_version": 1,
            "executable": fixture.executable.to_str().unwrap(),
            "loader": fixture.loader.to_str().unwrap(),
            "orbit_root": fixture.orbit_root.to_str().unwrap(),
            "uid": fixture.uid,
            "unexpected": true,
        });
        fs::write(&fixture.record, contents.to_string()).unwrap();
        set_mode(&fixture.record, 0o600);
        assert!(fixture.inspect().is_err());
    }
}
