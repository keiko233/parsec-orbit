//! The `parsec-orbit-installer-macos` command line.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::apply::{self, ApplyOptions};
use crate::digest;
use crate::platform;
use crate::state;
use crate::uninstall::{self, UninstallOptions};
use crate::whitelist::Whitelist;
use crate::STUB_RELATIVE;

pub const USAGE: &str = "\
parsec-orbit-installer-macos - transactional apply and restore for the patched Parsec.app

Usage: parsec-orbit-installer-macos <command> [options]

  inspect <app>                    classify the selected bundle; change nothing
  assess <app>                     report signature, Gatekeeper and quarantine
  apply <app> --loader <dylib>     back up, patch, sign, verify and install
  uninstall <app>                  restore the matching pristine bundle
  status <app>                     print the recorded installation state

Options
  --loader <dylib>      Orbit-owned loader dylib (required for apply)
  --orbit-root <folder> state root, default ~/Library/Application Support/ParsecOrbit
  --sudo-user <uid>     explicit sudo mode: bind data/record/tools to caller UID
  --purge-data          with uninstall: remove home/plugins/data/market/log too
  --json                machine-readable output
  --help                this text

Use scripts/install-macos.sh for protected installation. Explicit sudo mode
retains root only for selected bundle renames and lock creation. It refuses
a running app, an unsupported stub and any bundle whose tree no longer matches its recorded applied snapshot.
";

#[derive(Debug, Default)]
struct Options {
    positional: Vec<String>,
    loader: Option<PathBuf>,
    orbit_root: Option<PathBuf>,
    purge_data: bool,
    json: bool,
    sudo_user: Option<u32>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "--loader" => options.loader = Some(value("--loader")?),
            "--sudo-user" => {
                let uid = args.next().ok_or("--sudo-user needs a UID")?;
                options.sudo_user =
                    Some(uid.parse().map_err(|_| "--sudo-user needs a numeric UID")?);
            }
            "--orbit-root" => options.orbit_root = Some(value("--orbit-root")?),
            "--purge-data" => options.purge_data = true,
            "--json" => options.json = true,
            "--help" | "-h" => options.positional.push("help".into()),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => options.positional.push(argument.clone()),
        }
    }
    Ok(options)
}

/// `~/Library/Application Support/ParsecOrbit`, from `$HOME`.
fn default_orbit_root() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set; pass --orbit-root".to_string())?;
    Ok(home.join("Library/Application Support/ParsecOrbit"))
}

fn app_argument(options: &Options) -> Result<&Path, String> {
    match options.positional.as_slice() {
        [command, app] if command != "help" => Ok(Path::new(app)),
        [command, ..] if command != "help" => {
            Err(format!("{command} takes exactly one <app> path"))
        }
        _ => Err(USAGE.to_string()),
    }
}

fn orbit_root(options: &Options) -> Result<PathBuf, String> {
    match &options.orbit_root {
        Some(root) => Ok(root.clone()),
        None => default_orbit_root(),
    }
}

/// Runs one command. `out` receives the human-readable or JSON report.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let options = parse(args)?;
    let command = options
        .positional
        .first()
        .cloned()
        .unwrap_or_else(|| "help".into());
    if command == "help" || options.positional.is_empty() {
        out.write_all(USAGE.as_bytes())
            .map_err(|error| error.to_string())?;
        return Ok(());
    }

    if let Some(uid) = options.sudo_user {
        if !matches!(command.as_str(), "apply" | "uninstall") {
            return Err("--sudo-user is accepted only for apply/uninstall".into());
        }
        // Resolve paths before dropping privilege. The wrapper always supplies
        // the invoking user's explicit state root rather than root's HOME.
        let selected_root = options
            .orbit_root
            .as_ref()
            .ok_or("--sudo-user requires an explicit --orbit-root")?;
        crate::privilege::enter(uid, app_argument(&options)?, selected_root)?;
    } else if cfg!(target_os = "macos")
        && platform::effective_uid() == 0
        && matches!(command.as_str(), "apply" | "uninstall")
    {
        return Err("root apply/uninstall needs --sudo-user <caller UID> and --orbit-root; use scripts/install-macos.sh".into());
    }

    match command.as_str() {
        "assess" => {
            let report = crate::assessment::assess(app_argument(&options)?)?;
            if options.json {
                write_json(out, &report)
            } else {
                writeln!(
                    out,
                    "app: {}\nstrict signature: {}\nGatekeeper: {}\nquarantine: {}\n{}",
                    report.app.display(),
                    report.strict_signature.success,
                    report.gatekeeper.success,
                    report.quarantine.is_some(),
                    report.gatekeeper.stderr.trim()
                )
                .map_err(|error| error.to_string())
            }
        }
        "inspect" => {
            let app = app_argument(&options)?;
            inspect(app, &options, out)
        }
        "status" => {
            let app = app_argument(&options)?;
            status(app, &options, out)
        }
        "apply" => {
            let app = app_argument(&options)?.to_path_buf();
            let loader = options
                .loader
                .clone()
                .ok_or_else(|| "apply needs --loader <dylib>".to_string())?;
            let apply_options = ApplyOptions::new(orbit_root(&options)?);
            match apply::apply(&app, &loader, &apply_options) {
                Ok(report) => {
                    if options.json {
                        write_json(out, &report)?;
                    } else {
                        writeln!(
                            out,
                            "{}: {} -> {}",
                            report.outcome_text(),
                            report.app.display(),
                            report.install_dir.display()
                        )
                        .map_err(|error| error.to_string())?;
                        if let Some(stub) = &report.applied_stub_sha256 {
                            writeln!(out, "applied stub: {stub}")
                                .map_err(|error| error.to_string())?;
                        }
                        writeln!(
                            out,
                            "preseed: {} ({})",
                            report.preseed_text(),
                            report.preseed_detail
                        )
                        .map_err(|error| error.to_string())?;
                        for warning in &report.warnings {
                            writeln!(out, "warning: {warning}")
                                .map_err(|error| error.to_string())?;
                        }
                    }
                    Ok(())
                }
                Err(failure) => Err(failure.to_string()),
            }
        }
        "uninstall" => {
            let app = app_argument(&options)?.to_path_buf();
            let mut restore_options = UninstallOptions::new(orbit_root(&options)?);
            restore_options.purge_data = options.purge_data;
            match uninstall::uninstall(&app, &restore_options) {
                Ok(report) => {
                    if options.json {
                        write_json(out, &report)?;
                    } else {
                        writeln!(
                            out,
                            "uninstalled: {} restored from {}",
                            report.app.display(),
                            report.install_dir.display()
                        )
                        .map_err(|error| error.to_string())?;
                        writeln!(out, "pristine stub: {}", report.restored_stub_sha256)
                            .map_err(|error| error.to_string())?;
                        writeln!(out, "patched bundle kept at: {}", report.removed.display())
                            .map_err(|error| error.to_string())?;
                        if report.purged {
                            writeln!(out, "purged {} paths", report.purged_paths.len())
                                .map_err(|error| error.to_string())?;
                        }
                        for warning in &report.warnings {
                            writeln!(out, "warning: {warning}")
                                .map_err(|error| error.to_string())?;
                        }
                    }
                    Ok(())
                }
                Err(failure) => Err(failure.to_string()),
            }
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn inspect(app: &Path, options: &Options, out: &mut dyn Write) -> Result<(), String> {
    let app = crate::platform::canonical_absolute(app)?;
    let stub = app.join(STUB_RELATIVE);
    let stub_sha256 = digest::sha256_file(&stub).ok();
    let bundle_id = platform::bundle_identifier(&app).ok();
    let root = orbit_root(options)?;
    let root = platform::canonical_absolute(&root)?;
    let manifest = state::find_manifest(&root, &app)?;
    let whitelist = Whitelist::supported();
    let classification = state::classify(&app, manifest.as_ref(), &whitelist)?;
    let parent_writable = app
        .parent()
        .map(platform::directory_writable)
        .transpose()?
        .unwrap_or(false);
    let running = platform::running_executables(&stub).unwrap_or_default();

    if options.json {
        write_json(
            out,
            &serde_json::json!({
                "app": app,
                "bundle_id": bundle_id,
                "stub_sha256": stub_sha256,
                "classification": classification,
                "supported": stub_sha256.as_ref().is_some_and(|sha| whitelist.permits(sha)),
                "recording": manifest.as_ref().map(|manifest| serde_json::json!({
                    "installation_id": manifest.installation_id,
                    "state": manifest.state,
                    "applied_stub_sha256": manifest.applied_stub_sha256,
                })),
                "orbit_root": root,
                "parent_writable": parent_writable,
                "running_pids": running,
            }),
        )?;
        return Ok(());
    }

    writeln!(out, "app: {}", app.display()).map_err(|error| error.to_string())?;
    if let Some(bundle_id) = &bundle_id {
        writeln!(out, "bundle id: {bundle_id}").map_err(|error| error.to_string())?;
    }
    if let Some(sha256) = &stub_sha256 {
        let supported = if whitelist.permits(sha256) {
            "supported"
        } else {
            "not supported"
        };
        writeln!(out, "stub: {sha256} ({supported})").map_err(|error| error.to_string())?;
    }
    writeln!(out, "classification: {:?}", classification.class)
        .map_err(|error| error.to_string())?;
    for difference in &classification.differences {
        writeln!(out, "  {difference}").map_err(|error| error.to_string())?;
    }
    writeln!(out, "orbit root: {}", root.display()).map_err(|error| error.to_string())?;
    writeln!(
        out,
        "parent writable: {parent_writable}{}",
        if parent_writable {
            ""
        } else {
            " (apply needs permission on the selected bundle)"
        }
    )
    .map_err(|error| error.to_string())?;
    if !running.is_empty() {
        writeln!(out, "running: pid {running:?}").map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn status(app: &Path, options: &Options, out: &mut dyn Write) -> Result<(), String> {
    let app = crate::platform::canonical_absolute(app)?;
    let root = platform::canonical_absolute(&orbit_root(options)?)?;
    let manifest = state::find_manifest(&root, &app)?;
    let classification = state::classify(&app, manifest.as_ref(), &Whitelist::supported())?;

    match &manifest {
        None => writeln!(out, "no recorded installation for {}", app.display()),
        Some(manifest) => writeln!(
            out,
            "{}: {:?} ({:?}), applied stub {}",
            manifest.installation_id,
            manifest.state,
            classification.class,
            manifest.applied_stub_sha256.as_deref().unwrap_or("(none)")
        ),
    }
    .map_err(|error| error.to_string())?;
    for difference in &classification.differences {
        writeln!(out, "  {difference}").map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_json<T: serde::Serialize>(out: &mut dyn Write, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("cannot serialize the report: {error}"))?;
    writeln!(out, "{text}").map_err(|error| error.to_string())
}

#[cfg(test)]
mod sudo_tests {
    use super::*;

    #[test]
    fn sudo_identity_is_refused_for_read_only_commands_before_path_access() {
        let mut output = Vec::new();
        let args = [
            "status",
            "/path/that/must/not/be/accessed",
            "--sudo-user",
            "501",
        ]
        .map(str::to_string);
        assert!(run(&args, &mut output)
            .unwrap_err()
            .contains("only for apply/uninstall"));
        assert!(output.is_empty());
    }

    #[test]
    fn sudo_identity_requires_an_explicit_user_state_root() {
        let args = [
            "apply",
            "/path/that/must/not/be/accessed",
            "--sudo-user",
            "501",
        ]
        .map(str::to_string);
        assert!(run(&args, &mut Vec::new())
            .unwrap_err()
            .contains("explicit --orbit-root"));
    }

    #[test]
    fn sudo_mode_cannot_select_root_as_the_client_user() {
        let args = [
            "apply",
            "/path/that/must/not/be/accessed",
            "--sudo-user",
            "0",
            "--orbit-root",
            "/path/that/must/not/be/created",
        ]
        .map(str::to_string);
        assert!(run(&args, &mut Vec::new()).is_err());
    }
}
