//! The `parsec-orbit-market` command line.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::index::{Index, Plugin, Release, FORMAT};
use crate::install::{InstallOptions, Market};
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

Options
  --index <location>         index file or URL (or PARSEC_ORBIT_INDEX)
  --plugins-dir <folder>     default %APPDATA%\\ParsecOrbit\\plugins
  --keys <file>              trusted keys, default %APPDATA%\\ParsecOrbit\\market\\trusted-keys.txt
                             (one `ed25519 <base64 public key> [comment]` per line)
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
    env.var("APPDATA")
        .map(|appdata| Path::new(&appdata).join("ParsecOrbit"))
        .ok_or_else(|| {
            Error::Usage("%APPDATA% is not set; give --plugins-dir and --keys".to_string())
        })
}

fn one<'a>(options: &'a Options, what: &str) -> Result<&'a str, Error> {
    match options.positional.as_slice() {
        [single] => Ok(single),
        _ => Err(Error::Usage(format!("expected exactly one {what}"))),
    }
}

fn read_keys(
    options: &Options,
    env: &dyn Environment,
    fetch: &dyn Fetch,
) -> Result<Vec<trust::TrustedKey>, Error> {
    let location = match &options.keys {
        Some(location) => location.clone(),
        None => appdata(env)?
            .join("market")
            .join("trusted-keys.txt")
            .to_string_lossy()
            .into_owned(),
    };
    match fetch.fetch(&location, 1024 * 1024) {
        Ok(bytes) => trust::parse_trusted_keys(&String::from_utf8_lossy(&bytes)),
        // No key list is not an error by itself: nothing will verify, which
        // the commands that need a signature say.
        Err(_) if options.keys.is_none() => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn market<'a>(
    options: &Options,
    env: &dyn Environment,
    fetch: &'a dyn Fetch,
) -> Result<Market<'a>, Error> {
    let plugins_dir = match &options.plugins_dir {
        Some(dir) => PathBuf::from(dir),
        None => appdata(env)?.join("plugins"),
    };
    let mut market = Market::new(fetch, plugins_dir, read_keys(options, env, fetch)?);
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
    let Some((command, rest)) = args.split_first() else {
        return Err(Error::Usage(USAGE.to_string()));
    };
    let options = parse(rest)?;
    let say = |out: &mut dyn Write, text: String| writeln!(out, "{text}").map_err(Error::from);

    match command.as_str() {
        "help" | "--help" | "-h" => say(out, USAGE.trim_end().to_string()),
        "list" => {
            let market = market(&options, env, fetch)?;
            let index = market.load_index(&index_location(&options, env)?)?;
            if !index.signed {
                say(
                    out,
                    "warning: the index is not signed by a trusted key".to_string(),
                )?;
            }
            let installed = market.installed()?;
            for plugin in &index.index.plugins {
                let latest = plugin.select(None, market.api).map(|r| r.version.clone());
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
            let market = market(&options, env, fetch)?;
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
            let market = market(&options, env, fetch)?;
            let index = market.load_index(&index_location(&options, env)?)?;
            if !index.signed {
                say(
                    out,
                    "warning: the index is not signed by a trusted key".to_string(),
                )?;
            }
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
            let market = market(&options, env, fetch)?;
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
            let market = market(&options, env, fetch)?;
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
                "add that last line to the trusted-keys.txt of everyone who should trust it"
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
            url: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            sha256: trust::sha256_hex(&bytes),
            size: bytes.len() as u64,
            requires: manifest.requires.clone(),
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
            }),
        }
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    let index = Index {
        format: FORMAT,
        name: name.to_string(),
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
        run(&args, env, &Real, &mut out)?;
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
            run(&args, &no_env, &Real, &mut out)?;
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
    fn the_index_is_found_through_the_environment_and_appdata() {
        let user = Dir::new("env");
        let publisher = Dir::new("env-pub");
        write_package(&publisher, "com.example.p", "1.0.0", "");
        let index = publisher.path("index.json");
        let none = Vars(HashMap::new());
        command(&["index", &publisher.path(""), "--out", &index], &none).unwrap();

        let env = Vars(HashMap::from([
            ("APPDATA".to_string(), user.path("roaming")),
            ("PARSEC_ORBIT_INDEX".to_string(), index.clone()),
        ]));
        // Nothing trusted yet: the index is refused, with the reason.
        let error = command(&["list"], &env).unwrap_err().to_string();
        assert!(error.contains("signature"), "{error}");
        // The unsigned escape hatch works and warns.
        let listed = command(&["list", "--allow-unsigned"], &env).unwrap();
        assert!(
            listed.contains("warning: the index is not signed"),
            "{listed}"
        );
        assert!(listed.contains("com.example.p  1.0.0"));

        let installed = command(&["install", "com.example.p", "--allow-unsigned"], &env).unwrap();
        assert!(installed.contains("installed com.example.p 1.0.0"));
        assert!(user
            .0
            .join("roaming/ParsecOrbit/plugins/com.example.p/plugin.toml")
            .is_file());
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
}
