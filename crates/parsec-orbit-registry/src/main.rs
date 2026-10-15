//! `parsec-orbit-market`: see `parsec-orbit-market help`.

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = std::io::stdout().lock();
    match parsec_orbit_registry::cli::run(
        &args,
        &parsec_orbit_registry::cli::System,
        &parsec_orbit_registry::source::Real,
        &mut stdout,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = stdout.flush();
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
