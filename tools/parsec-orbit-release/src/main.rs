//! `parsec-orbit-release`: packages the official modules, and checks, builds
//! and signs the registry in `manifest/`. The design is in
//! `docs/design/restructure.md` (sections 4.3 and 5).

mod args;
#[cfg(test)]
mod testing;
mod version;

use std::io::Write;
use std::process::ExitCode;

use parsec_orbit_registry::cli;
use parsec_orbit_registry::source::Real;
use parsec_orbit_registry::Error;

const USAGE: &str = "\
parsec-orbit-release - package, check and publish the Parsec Orbit registry

Usage: parsec-orbit-release <command> [options]

  version [--workspace DIR]           print [workspace.package] version (default: .)

Signing (the parsec-orbit-market implementation)
  keygen <name>                       write <name>.key (the secret), print the public key
  sign <index.json> --key <name.key>  write <index.json>.sig
  verify <index.json> [--keys FILE]   check <index.json>.sig against the trusted keys
  verify-any <file> [--keys FILE]     check <file>.sig without reading it as an index

Exit status: 0 success, 1 error (a check that finds problems is an error), 2 usage.
";

fn run(args: &[String], out: &mut dyn Write) -> Result<(), Error> {
    let Some((command, rest)) = args.split_first() else {
        return Err(args::usage("a command is required"));
    };
    match command.as_str() {
        "help" | "--help" | "-h" => Ok(write!(out, "{USAGE}")?),
        "version" => version::run(rest, out),
        "keygen" | "sign" | "verify" | "verify-any" => signing(command, rest, out),
        other => Err(args::usage(format!("unknown command {other:?}"))),
    }
}

/// The signing commands are the registry library's own: this only forwards
/// the arguments, so both tools sign and verify the same way.
fn signing(command: &str, rest: &[String], out: &mut dyn Write) -> Result<(), Error> {
    let mut forwarded = vec![command.to_string()];
    forwarded.extend_from_slice(rest);
    cli::run(&forwarded, &cli::System, &Real, out).map_err(|error| match error {
        Error::Usage(_) => args::usage(format!("invalid arguments for {command}")),
        other => other,
    })
}

/// 0 for success, 2 for a usage error, 1 for anything else.
fn status(result: &Result<(), Error>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(Error::Usage(_)) => 2,
        Err(_) => 1,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = std::io::stdout().lock();
    let result = run(&args, &mut stdout);
    let _ = stdout.flush();
    if let Err(error) = &result {
        eprintln!("error: {error}");
        if matches!(error, Error::Usage(_)) {
            eprintln!("\n{USAGE}");
        }
    }
    ExitCode::from(status(&result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    fn output(args: &[&str]) -> Result<String, Error> {
        let mut out = Vec::new();
        run(&words(args), &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn help_prints_the_usage_on_success() {
        assert_eq!(status(&Ok(())), 0);
        assert!(output(&["help"])
            .unwrap()
            .contains("Usage: parsec-orbit-release"));
    }

    #[test]
    fn missing_or_unknown_commands_are_usage_errors() {
        assert_eq!(status(&run(&[], &mut Vec::new())), 2);
        assert_eq!(status(&run(&words(&["bogus"]), &mut Vec::new())), 2);
    }

    #[test]
    fn a_failed_command_is_status_one() {
        let result = run(
            &words(&["version", "--workspace", "/no/such/folder"]),
            &mut Vec::new(),
        );
        assert_eq!(status(&result), 1);
    }

    #[test]
    fn signing_is_delegated_and_usage_is_reworded() {
        // Without a file the registry library answers with its own usage text,
        // which must not reach the user of this tool.
        let error = output(&["verify"]).unwrap_err();
        assert!(matches!(error, Error::Usage(ref text) if text == "invalid arguments for verify"));
        assert_eq!(status(&Err(error)), 2);
    }
}
