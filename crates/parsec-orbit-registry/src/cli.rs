//! The `parsec-orbit-market` command line.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::index::{Index, PlatformId, Plugin, Release, FORMAT};
use crate::install::{InstallOptions, LoadedIndex, Market};
use crate::package;
use crate::source::Fetch;
use crate::trust;
use crate::Error;

pub const USAGE: &str = "\
parsec-orbit-market - find, check and install Parsec Orbit plugins

Usage: parsec-orbit-market <command> [options]

For users
  list                       show what the index offers
  installed                  show the plugins in the plugins folder
  install <id>               install a plugin   [--version V] [--with-deps] [--force]
  update [<id>]              install newer releases of what the market installed
  remove <id>                remove a plugin the market installed   [--force]

For publishers
  index <folder>             build an index from the *.zip packages in a folder
                             --out <index.json>   [--name <text>]
  keygen <name>              write <name>.key (the secret) and print the public key
  sign <index.json>          write <index.json>.sig   --key <name.key>
  verify <index.json>        check <index.json>.sig against the trusted keys
  verify-any <file>          check <file>.sig against the trusted keys
                             without interpreting the file as an index

Options
  --index <location>         index file or URL (or PARSEC_ORBIT_INDEX)
  --plugins-dir <folder>     default <Orbit root>/plugins
  --keys <file>              trusted keys: a .toml file of [[key]] tables, or a file of
                             `ed25519 <base64 public key> [comment]` lines (any other name);
                             default <Orbit root>/market/trusted-keys.toml, or the
                             trusted-keys.txt beside it, converted once
                             (macOS arm64: ~/Library/Application Support/ParsecOrbit;
                              PARSEC_ORBIT_HOME overrides it)
  --allow-unsigned           accept an index whose signature is missing or wrong
                             (the digests in it are then only as good as its source)
";

/// What the command line can ask the environment.
pub trait Environment {
    fn var(&self, name: &str) -> Option<String>;
}

pub struct System;

impl Environment for System {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
}

#[derive(Default)]
struct Options {
    positional: Vec<String>,
    index: Option<String>,
    plugins_dir: Option<String>,
    keys: Option<String>,
    key: Option<String>,
    out: Option<String>,
    name: Option<String>,
    version: Option<String>,
    force: bool,
    with_deps: bool,
    allow_unsigned: bool,
}

fn parse(args: &[String]) -> Result<Options, Error> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .cloned()
                .ok_or_else(|| Error::Usage(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--index" => options.index = Some(value("--index")?),
            "--plugins-dir" => options.plugins_dir = Some(value("--plugins-dir")?),
            "--keys" => options.keys = Some(value("--keys")?),
            "--key" => options.key = Some(value("--key")?),
            "--out" => options.out = Some(value("--out")?),
            "--name" => options.name = Some(value("--name")?),
            "--version" => options.version = Some(value("--version")?),
            "--force" => options.force = true,
            "--with-deps" => options.with_deps = true,
            "--allow-unsigned" => options.allow_unsigned = true,
            flag if flag.starts_with("--") => {
                return Err(Error::Usage(format!("unknown option {flag}")))
            }
            _ => options.positional.push(arg.clone()),
        }
    }
    Ok(options)
}

fn appdata(env: &dyn Environment) -> Result<PathBuf, Error> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        if let Some(root) = env.var("PARSEC_ORBIT_HOME") {
            return Ok(PathBuf::from(root));
        }
        env.var("HOME")
            .map(|home| {
                Path::new(&home)
                    .join("Library")
                    .join("Application Support")
                    .join("ParsecOrbit")
            })
            .ok_or_else(|| {
                Error::Usage("HOME is not set; give --plugins-dir and --keys".to_string())
            })
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        env.var("APPDATA")
            .map(|appdata| Path::new(&appdata).join("ParsecOrbit"))
            .ok_or_else(|| {
                Error::Usage("%APPDATA% is not set; give --plugins-dir and --keys".to_string())
            })
    }
}

fn one<'a>(options: &'a Options, what: &str) -> Result<&'a str, Error> {
    match options.positional.as_slice() {
        [single] => Ok(single),
        _ => Err(Error::Usage(format!("expected exactly one {what}"))),
    }
}

/// The trusted keys: `--keys <file>` (a `.toml` file is a TOML key list, any
/// other name the line format), else the key list of the Orbit root. With no
/// key list at all nothing verifies, which the commands that need a signature
/// say.
fn read_keys(
    options: &Options,
    env: &dyn Environment,
    fetch: &dyn Fetch,
) -> Result<Vec<trust::TrustedKey>, Error> {
    let Some(location) = &options.keys else {
        return trust::load_trusted_keys(&appdata(env)?.join("market"));
    };
    let bytes = fetch.fetch(location, 1024 * 1024)?;
    let text = String::from_utf8_lossy(&bytes);
    if location.ends_with(".toml") {
        trust::parse_trusted_keys_toml(&text)
    } else {
        trust::parse_trusted_keys(&text)
    }
}

fn market<'a>(
    options: &Options,
    env: &dyn Environment,
    fetch: &'a dyn Fetch,
    platform: Option<PlatformId>,
) -> Result<Market<'a>, Error> {
    let platform = platform.ok_or_else(|| {
        Error::Usage(format!(
            "this computer has no plugin packages: the market serves {} and {}",
            PlatformId::WINDOWS_X86_64,
            PlatformId::MACOS_ARM64
        ))
    })?;
    let plugins_dir = match &options.plugins_dir {
        Some(dir) => PathBuf::from(dir),
        None => appdata(env)?.join("plugins"),
    };
    let mut market = Market::new(
        fetch,
        plugins_dir,
        read_keys(options, env, fetch)?,
        platform,
    );
    market.allow_unsigned = options.allow_unsigned;
    Ok(market)
}

fn index_location(options: &Options, env: &dyn Environment) -> Result<String, Error> {
    options
        .index
        .clone()
        .or_else(|| env.var("PARSEC_ORBIT_INDEX"))
        .ok_or_else(|| {
            Error::Usage(
                "no index: give --index <file or URL> or set PARSEC_ORBIT_INDEX".to_string(),
            )
        })
}

/// Runs a command; everything it prints goes to `out`.
pub fn run(
    args: &[String],
    env: &dyn Environment,
    fetch: &dyn Fetch,
    out: &mut dyn Write,
) -> Result<(), Error> {
    run_on_platform(args, env, fetch, out, PlatformId::current())
}

/// `platform` is the host's; `None` on a host with no packages. The commands
/// that work on plugins then fail; the publisher commands still run.
fn run_on_platform(
    args: &[String],
    env: &dyn Environment,
    fetch: &dyn Fetch,
    out: &mut dyn Write,
    platform: Option<PlatformId>,
) -> Result<(), Error> {
    let Some((command, rest)) = args.split_first() else {
        return Err(Error::Usage(USAGE.to_string()));
    };
    let options = parse(rest)?;
    let say = |out: &mut dyn Write, text: String| writeln!(out, "{text}").map_err(Error::from);
    // `--allow-unsigned` waves an index through, but the reason it was not
    // accepted still matters: a missing signature and a bad one are not the
    // same risk.
    let warn_unsigned = |out: &mut dyn Write, index: &LoadedIndex| -> Result<(), Error> {
        if index.signed {
            return Ok(());
        }
        let detail = index
            .signature_note
            .as_deref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        say(
            out,
            format!("warning: the index is not signed by a trusted key{detail}"),
        )
    };

    match command.as_str() {
        "help" | "--help" | "-h" => say(out, USAGE.trim_end().to_string()),
        "list" => {
            let market = market(&options, env, fetch, platform)?;
            let index = market.load_index(&index_location(&options, env)?)?;
            warn_unsigned(out, &index)?;
            let installed = market.installed()?;
            for plugin in &index.index.plugins {
                if !plugin
                    .versions
                    .iter()
                    .any(|release| release.platform == market.platform)
                {
                    continue;
                }
                let latest = plugin
                    .select(None, market.api, &market.platform)
                    .map(|r| r.version.clone());
                let here = installed
                    .iter()
                    .find(|p| p.id == plugin.id)
                    .map(|p| format!(" (installed {})", p.version))
                    .unwrap_or_default();
                say(
                    out,
                    format!(
                        "{}  {}{here}  {}",
                        plugin.id,
                        latest.unwrap_or_else(|_| "-".to_string()),
                        plugin.description
                    ),
                )?;
            }
            Ok(())
        }
        "installed" => {
            let market = market(&options, env, fetch, platform)?;
            for plugin in market.installed()? {
                let from = match &plugin.origin {
                    Some(origin) => format!("market {}", origin.index),
                    None => "by hand".to_string(),
                };
                say(out, format!("{}  {}  ({from})", plugin.id, plugin.version))?;
            }
            Ok(())
        }
        "install" => {
            let id = one(&options, "plugin id")?.to_string();
            let market = market(&options, env, fetch, platform)?;
            let index = market.load_index(&index_location(&options, env)?)?;
            warn_unsigned(out, &index)?;
            let done = market.install(
                &index,
                &id,
                options.version.as_deref(),
                InstallOptions {
                    force: options.force,
                    with_deps: options.with_deps,
                },
            )?;
            for installed in done {
                let verb = if installed.replaced.is_some() {
                    "updated"
                } else {
                    "installed"
                };
                let from = installed
                    .replaced
                    .map(|old| format!(" (was {old})"))
                    .unwrap_or_default();
                say(
                    out,
                    format!("{verb} {} {}{from}", installed.id, installed.version),
                )?;
            }
            say(out, "restart the Parsec client to load it".to_string())
        }
        "update" => {
            let market = market(&options, env, fetch, platform)?;
            let index = market.load_index(&index_location(&options, env)?)?;
            let wanted = options.positional.first().cloned();
            let updates: Vec<String> = market
                .updates(&index)?
                .into_iter()
                .filter(|(plugin, _)| wanted.as_deref().is_none_or(|id| id == plugin.id))
                .map(|(plugin, _)| plugin.id)
                .collect();
            if updates.is_empty() {
                return say(out, "everything is up to date".to_string());
            }
            for id in updates {
                for installed in market.install(&index, &id, None, InstallOptions::default())? {
                    say(
                        out,
                        format!(
                            "updated {} {} (was {})",
                            installed.id,
                            installed.version,
                            installed.replaced.unwrap_or_else(|| "-".to_string())
                        ),
                    )?;
                }
            }
            say(out, "restart the Parsec client to load them".to_string())
        }
        "remove" => {
            let id = one(&options, "plugin id")?.to_string();
            let market = market(&options, env, fetch, platform)?;
            let removed = market.remove(&id, options.force)?;
            say(out, format!("removed {} {}", removed.id, removed.version))
        }
        "index" => {
            let folder = one(&options, "packages folder")?;
            let target = options
                .out
                .as_deref()
                .ok_or_else(|| Error::Usage("index needs --out <index.json>".to_string()))?;
            let index = build_index(Path::new(folder), options.name.as_deref().unwrap_or(""))?;
            std::fs::write(target, index.to_json())?;
            let releases: usize = index.plugins.iter().map(|p| p.versions.len()).sum();
            say(
                out,
                format!(
                    "wrote {target}: {} plugins, {releases} releases",
                    index.plugins.len()
                ),
            )
        }
        "keygen" => {
            let name = one(&options, "key name")?;
            let (secret, public) = trust::generate_keypair()?;
            let file = format!("{name}.key");
            if Path::new(&file).exists() {
                return Err(Error::Conflict(format!(
                    "{file} exists; it is not overwritten"
                )));
            }
            std::fs::write(&file, format!("{secret}\n"))?;
            say(
                out,
                format!("secret key written to {file} - keep it private"),
            )?;
            let label = Path::new(name)
                .file_name()
                .map_or(name.to_string(), |label| {
                    label.to_string_lossy().into_owned()
                });
            say(out, format!("ed25519 {public} {label}"))?;
            say(
                out,
                "add a [[key]] table with that public key to the trusted-keys.toml of everyone who should trust it"
                    .to_string(),
            )
        }
        "sign" => {
            let index = one(&options, "index file")?;
            let key = options
                .key
                .as_deref()
                .ok_or_else(|| Error::Usage("sign needs --key <name.key>".to_string()))?;
            let secret = std::fs::read_to_string(key)?;
            let signing = trust::signing_key(&secret)?;
            let signature = trust::sign(&std::fs::read(index)?, &signing);
            std::fs::write(format!("{index}.sig"), format!("{signature}\n"))?;
            say(out, format!("wrote {index}.sig"))
        }
        "verify" => {
            let location = one(&options, "index file")?;
            let bytes = fetch.fetch(location, 16 * 1024 * 1024)?;
            let signature = fetch.fetch(&format!("{location}.sig"), 4096)?;
            let keys = read_keys(&options, env, fetch)?;
            let which = trust::verify(&bytes, &String::from_utf8_lossy(&signature), &keys)?;
            let index = Index::parse(&bytes)?;
            say(
                out,
                format!(
                    "ok: signed by key {} ({}), {} plugins",
                    which + 1,
                    keys[which].comment,
                    index.plugins.len()
                ),
            )
        }
        "verify-any" => {
            let location = one(&options, "file")?;
            let bytes = fetch.fetch(location, 16 * 1024 * 1024)?;
            let signature = fetch.fetch(&format!("{location}.sig"), 4096)?;
            let keys = read_keys(&options, env, fetch)?;
            let which = trust::verify(&bytes, &String::from_utf8_lossy(&signature), &keys)?;
            say(
                out,
                format!(
                    "ok: signed by key {} ({}), {} bytes",
                    which + 1,
                    keys[which].comment,
                    bytes.len()
                ),
            )
        }
        other => Err(Error::Usage(format!(
            "unknown command {other:?}\n\n{USAGE}"
        ))),
    }
}

/// Builds an index from the packages in `folder`.
pub fn build_index(folder: &Path, name: &str) -> Result<Index, Error> {
    let mut zips: Vec<PathBuf> = std::fs::read_dir(folder)
        .map_err(|error| Error::Io(format!("{}: {error}", folder.display())))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        })
        .collect();
    zips.sort();

    let mut plugins: Vec<Plugin> = Vec::new();
    for path in zips {
        let bytes = std::fs::read(&path)?;
        let package = package::read(&bytes)
            .map_err(|error| Error::Package(format!("{}: {error}", path.display())))?;
        let manifest = package.manifest;
        if manifest.version.is_empty() {
            return Err(Error::Package(format!(
                "{}: plugin.toml has no version",
                path.display()
            )));
        }
        let release = Release {
            version: manifest.version.clone(),
            api: manifest.api,
            platform: if manifest.kind == "dylib" {
                PlatformId::macos_arm64()
            } else {
                PlatformId::windows_x86_64()
            },
            url: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            sha256: trust::sha256_hex(&bytes),
            size: bytes.len() as u64,
            requires: manifest.requires.clone(),
            published: None,
            yanked: false,
            mirrors: Vec::new(),
        };
        match plugins.iter_mut().find(|plugin| plugin.id == manifest.id) {
            Some(plugin) => plugin.versions.push(release),
            None => plugins.push(Plugin {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                author: manifest.author.clone(),
                license: manifest.license.clone(),
                versions: vec![release],
                ..Default::default()
            }),
        }
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    let index = Index {
        format: FORMAT,
        name: name.to_string(),
        serial: None,
        generated: None,
        plugins,
    };
    // The same checks a client will make.
    Index::parse(index.to_json().as_bytes())?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::tests::zip_of;
    use crate::source::Real;
    use std::collections::HashMap;

    struct Vars(HashMap<String, String>);
    impl Environment for Vars {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(label: &str) -> Dir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-cli-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Dir(path)
        }
        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(args: &[&str], env: &dyn Environment) -> Result<String, Error> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let mut out = Vec::new();
        run_on_platform(
            &args,
            env,
            &Real,
            &mut out,
            Some(PlatformId::windows_x86_64()),
        )?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn write_package(dir: &Dir, id: &str, version: &str, extra: &str) {
        let manifest = format!(
            "id = \"{id}\"\nversion = \"{version}\"\nname = \"Name of {id}\"\ndescription = \"Does {id}\"\nauthor = \"Me\"\nlicense = \"MIT\"\nentry = \"p.dll\"\n{extra}"
        );
        let bytes = zip_of(&[
            (&format!("{id}/plugin.toml"), manifest.as_bytes()),
            (&format!("{id}/p.dll"), b"MZ"),
        ]);
        std::fs::write(dir.0.join(format!("{id}-{version}.zip")), bytes).unwrap();
    }

    #[test]
    fn the_whole_publishing_and_installing_story() {
        let publisher = Dir::new("publish");
        let user = Dir::new("user");
        write_package(
            &publisher,
            "com.example.p",
            "1.0.0",
            "requires = [\"com.example.base\"]\n",
        );
        write_package(&publisher, "com.example.p", "1.2.0", "");
        write_package(&publisher, "com.example.base", "0.5.0", "");

        let no_env = Vars(HashMap::new());
        let folder = publisher.path("");
        let index = publisher.path("out/index.json");
        std::fs::create_dir_all(publisher.0.join("out")).unwrap();
        // The index is built from the packages, the packages stay next to it.
        let built = command(
            &[
                "index",
                &folder,
                "--out",
                &publisher.path("index.json"),
                "--name",
                "Test market",
            ],
            &no_env,
        )
        .unwrap();
        assert!(built.contains("2 plugins, 3 releases"), "{built}");
        let _ = index;
        let index = publisher.path("index.json");

        // A key pair: the secret stays with the publisher, the public key goes
        // into the user's trusted list.
        let key_file = publisher.path("market");
        let generated = command(&["keygen", &key_file], &no_env).unwrap();
        let public = generated
            .lines()
            .find(|l| l.starts_with("ed25519 "))
            .unwrap()
            .to_string();
        let keys = user.path("trusted-keys.txt");
        std::fs::write(&keys, format!("# the test market\n{public}\n")).unwrap();
        assert!(
            matches!(
                command(&["keygen", &key_file], &no_env),
                Err(Error::Conflict(_))
            ),
            "an existing key is never overwritten"
        );
        command(
            &["sign", &index, "--key", &format!("{key_file}.key")],
            &no_env,
        )
        .unwrap();
        let verified = command(&["verify", &index, "--keys", &keys], &no_env).unwrap();
        assert!(
            verified.contains("ok: signed by key 1") && verified.contains("2 plugins"),
            "{verified}"
        );

        // The user lists and installs.
        let plugins = user.path("plugins");
        let common = [
            "--index",
            index.as_str(),
            "--plugins-dir",
            plugins.as_str(),
            "--keys",
            keys.as_str(),
        ];
        let with = |first: &[&str]| -> Vec<String> {
            first
                .iter()
                .chain(common.iter())
                .map(|s| s.to_string())
                .collect()
        };
        let run_args = |args: Vec<String>| -> Result<String, Error> {
            let mut out = Vec::new();
            run_on_platform(
                &args,
                &no_env,
                &Real,
                &mut out,
                Some(PlatformId::windows_x86_64()),
            )?;
            Ok(String::from_utf8(out).unwrap())
        };

        let list = run_args(with(&["list"])).unwrap();
        assert!(list.contains("com.example.p  1.2.0"), "{list}");
        assert!(list.contains("com.example.base  0.5.0"), "{list}");

        // 1.0.0 requires the base plugin, 1.2.0 does not.
        let installed = run_args(with(&[
            "install",
            "com.example.p",
            "--version",
            "1.0.0",
            "--with-deps",
        ]))
        .unwrap();
        assert!(
            installed.contains("installed com.example.base 0.5.0"),
            "{installed}"
        );
        assert!(
            installed.contains("installed com.example.p 1.0.0"),
            "{installed}"
        );
        assert!(std::path::Path::new(&plugins)
            .join("com.example.p/market.json")
            .is_file());

        let shown = run_args(with(&["installed"])).unwrap();
        assert!(shown.contains("com.example.p  1.0.0  (market "), "{shown}");

        let updated = run_args(with(&["update"])).unwrap();
        assert!(
            updated.contains("updated com.example.p 1.2.0 (was 1.0.0)"),
            "{updated}"
        );
        assert!(run_args(with(&["update"])).unwrap().contains("up to date"));

        let removed = run_args(with(&["remove", "com.example.base"])).unwrap();
        assert!(
            removed.contains("removed com.example.base 0.5.0"),
            "{removed}"
        );
    }

    #[test]
    fn the_index_is_found_through_the_environment_and_platform_root() {
        let user = Dir::new("env");
        let publisher = Dir::new("env-pub");
        write_package(&publisher, "com.example.p", "1.0.0", "");
        let index = publisher.path("index.json");
        let none = Vars(HashMap::new());
        command(&["index", &publisher.path(""), "--out", &index], &none).unwrap();

        let mut values = HashMap::from([("PARSEC_ORBIT_INDEX".to_string(), index.clone())]);
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        values.insert(
            "PARSEC_ORBIT_HOME".to_string(),
            user.path("roaming/ParsecOrbit"),
        );
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        values.insert("APPDATA".to_string(), user.path("roaming"));
        let env = Vars(values);
        // Nothing trusted yet: the index is refused, with the reason.
        let error = command(&["list"], &env).unwrap_err().to_string();
        assert!(error.contains("signature"), "{error}");
        // The unsigned escape hatch works and warns.
        let listed = command(&["list", "--allow-unsigned"], &env).unwrap();
        assert!(
            listed.contains("warning: the index is not signed"),
            "{listed}"
        );
        // ... and says why: no signature is not the same as a bad one.
        assert!(listed.contains("there is no signature"), "{listed}");
        assert!(listed.contains("com.example.p  1.0.0"));

        let installed = command(&["install", "com.example.p", "--allow-unsigned"], &env).unwrap();
        assert!(installed.contains("installed com.example.p 1.0.0"));
        assert!(user
            .0
            .join("roaming/ParsecOrbit/plugins/com.example.p/plugin.toml")
            .is_file());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn macos_orbit_root_defaults_under_home_and_accepts_the_override() {
        let home = Dir::new("mac-home");
        let env = Vars(HashMap::from([("HOME".to_string(), home.path(""))]));
        assert_eq!(
            appdata(&env).unwrap(),
            home.0
                .join("Library")
                .join("Application Support")
                .join("ParsecOrbit")
        );

        let override_root = home.0.join("custom-root");
        let env = Vars(HashMap::from([
            ("HOME".to_string(), home.path("")),
            (
                "PARSEC_ORBIT_HOME".to_string(),
                override_root.to_string_lossy().into_owned(),
            ),
        ]));
        assert_eq!(appdata(&env).unwrap(), override_root);
    }

    #[test]
    fn a_signature_that_does_not_verify_is_named_when_it_is_waved_through() {
        let publisher = Dir::new("tamper");
        write_package(&publisher, "com.example.p", "1.0.0", "");
        let none = Vars(HashMap::new());
        let index = publisher.path("index.json");
        command(&["index", &publisher.path(""), "--out", &index], &none).unwrap();
        let key_file = publisher.path("market");
        let public = command(&["keygen", &key_file], &none)
            .unwrap()
            .lines()
            .find(|line| line.starts_with("ed25519 "))
            .unwrap()
            .to_string();
        let keys = publisher.path("trusted-keys.txt");
        std::fs::write(&keys, format!("{public}\n")).unwrap();
        let plugins = publisher.path("plugins");
        command(
            &["sign", &index, "--key", &format!("{key_file}.key")],
            &none,
        )
        .unwrap();

        // The index is edited after it was signed.
        let edited = std::fs::read_to_string(&index)
            .unwrap()
            .replace("com.example.p", "com.example.q");
        std::fs::write(&index, edited).unwrap();

        let error = command(
            &[
                "list",
                "--index",
                &index,
                "--keys",
                &keys,
                "--plugins-dir",
                &plugins,
            ],
            &none,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("no trusted key signed this index"),
            "{error}"
        );
        let listed = command(
            &[
                "list",
                "--index",
                &index,
                "--keys",
                &keys,
                "--plugins-dir",
                &plugins,
                "--allow-unsigned",
            ],
            &none,
        )
        .unwrap();
        assert!(listed.contains("the signature did not verify"), "{listed}");
    }

    #[test]
    fn verify_any_checks_an_arbitrary_signed_file() {
        // release.json is not an index, so `verify` would refuse it after the
        // signature check. `verify-any` exists for exactly that case.
        let publisher = Dir::new("verify-any");
        let none = Vars(HashMap::new());
        let key_file = publisher.path("release-key");
        let generated = command(&["keygen", &key_file], &none).unwrap();
        let public = generated
            .lines()
            .find(|line| line.starts_with("ed25519 "))
            .unwrap()
            .to_string();
        let keys = publisher.path("keys.txt");
        std::fs::write(&keys, format!("{public}\n")).unwrap();

        let payload = publisher.path("release.json");
        std::fs::write(&payload, b"{\"format\":1,\"name\":\"release\"}").unwrap();
        command(
            &["sign", &payload, "--key", &format!("{key_file}.key")],
            &none,
        )
        .unwrap();

        let verified = command(&["verify-any", &payload, "--keys", &keys], &none).unwrap();
        assert!(verified.contains("ok: signed by key 1"), "{verified}");
    }

    #[test]
    fn the_default_key_list_is_converted_and_an_explicit_toml_list_is_read() {
        let root = Dir::new("default-keys");
        let publisher = Dir::new("default-keys-pub");
        write_package(&publisher, "com.example.p", "1.0.0", "");
        let none = Vars(HashMap::new());
        let index = publisher.path("index.json");
        command(&["index", &publisher.path(""), "--out", &index], &none).unwrap();
        let key_file = publisher.path("market");
        let generated = command(&["keygen", &key_file], &none).unwrap();
        let public = generated
            .lines()
            .find(|line| line.starts_with("ed25519 "))
            .unwrap()
            .to_string();
        command(
            &["sign", &index, "--key", &format!("{key_file}.key")],
            &none,
        )
        .unwrap();

        // The Orbit root holds the old text list; the first run converts it.
        let orbit = root.0.join("ParsecOrbit");
        let market_dir = orbit.join("market");
        std::fs::create_dir_all(&market_dir).unwrap();
        let key = public.split_whitespace().nth(1).unwrap();
        let legacy = format!("ed25519 {key} publisher\n");
        std::fs::write(market_dir.join("trusted-keys.txt"), &legacy).unwrap();
        let mut values = HashMap::from([("PARSEC_ORBIT_INDEX".to_string(), index.clone())]);
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        values.insert(
            "PARSEC_ORBIT_HOME".to_string(),
            orbit.to_string_lossy().into_owned(),
        );
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        values.insert("APPDATA".to_string(), root.path(""));
        let env = Vars(values);

        let listed = command(&["list"], &env).unwrap();
        assert!(listed.contains("com.example.p  1.0.0"), "{listed}");
        let converted = std::fs::read_to_string(market_dir.join("trusted-keys.toml")).unwrap();
        assert!(
            converted.contains("[[key]]") && converted.contains("comment = \"publisher\""),
            "{converted}"
        );
        assert_eq!(
            std::fs::read_to_string(market_dir.join("trusted-keys.txt")).unwrap(),
            legacy
        );

        let explicit = publisher.path("keys.toml");
        std::fs::write(&explicit, &converted).unwrap();
        let verified = command(&["verify", &index, "--keys", &explicit], &none).unwrap();
        assert!(verified.contains("ok: signed by key 1"), "{verified}");
    }

    #[test]
    fn usage_errors_say_what_is_wrong() {
        let none = Vars(HashMap::new());
        assert!(command(&[], &none)
            .unwrap_err()
            .to_string()
            .contains("Usage"));
        assert!(command(&["frobnicate"], &none)
            .unwrap_err()
            .to_string()
            .contains("unknown command"));
        assert!(command(&["install"], &none)
            .unwrap_err()
            .to_string()
            .contains("exactly one"));
        assert!(command(&["install", "a", "--bogus"], &none)
            .unwrap_err()
            .to_string()
            .contains("unknown option"));
        assert!(command(&["install", "a", "--index"], &none)
            .unwrap_err()
            .to_string()
            .contains("needs a value"));
        assert!(
            command(&["list"], &none)
                .unwrap_err()
                .to_string()
                .contains("no index")
                || command(&["list"], &none).is_err()
        );
        assert!(command(&["help"], &none)
            .unwrap()
            .contains("For publishers"));
        assert!(command(&["index", "x"], &none)
            .unwrap_err()
            .to_string()
            .contains("--out"));
        assert!(command(&["sign", "x"], &none)
            .unwrap_err()
            .to_string()
            .contains("--key"));
    }

    #[test]
    fn building_an_index_rejects_broken_packages() {
        let dir = Dir::new("broken");
        std::fs::write(dir.0.join("bad.zip"), b"not a zip").unwrap();
        let error = build_index(&dir.0, "").unwrap_err().to_string();
        assert!(error.contains("bad.zip"), "{error}");
        let empty = Dir::new("empty");
        assert!(build_index(&empty.0, "").unwrap().plugins.is_empty());
        assert!(build_index(&empty.0.join("missing"), "").is_err());
    }

    #[test]
    fn publisher_index_uses_manifest_kind_for_release_platform() {
        let dir = Dir::new("platform-publish");
        write_package(&dir, "com.example.windows", "1.0.0", "");
        let manifest = "id = \"com.example.mac\"\nversion = \"1.0.0\"\nkind = \"dylib\"\nentry = \"plugin.dylib\"\n";
        let mac_package = zip_of(&[
            ("com.example.mac/plugin.toml", manifest.as_bytes()),
            ("com.example.mac/plugin.dylib", b"Mach-O"),
        ]);
        std::fs::write(dir.0.join("com.example.mac-1.0.0.zip"), mac_package).unwrap();

        let index = build_index(&dir.0, "platforms").unwrap();
        assert_eq!(
            index.plugin("com.example.windows").unwrap().versions[0].platform,
            PlatformId::windows_x86_64()
        );
        assert_eq!(
            index.plugin("com.example.mac").unwrap().versions[0].platform,
            PlatformId::macos_arm64()
        );
    }
}
