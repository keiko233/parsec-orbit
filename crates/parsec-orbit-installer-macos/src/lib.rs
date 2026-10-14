//! Transactional installer for the patched-bundle layout.
//!
//! The installer owns the mutation side of `docs/spec/macos-host.md`: it
//! inspects a selected `Parsec.app`, verifies the supported stub before any
//! change, keeps a full pristine backup beside an append-only journal, patches
//! only supported Mach-O header space, signs inside-out, swaps atomically and
//! can restore the recorded pristine bundle. The macOS tool invocations
//! (`codesign`, `ditto`, `libproc`) live in [`platform`]; the Mach-O parser,
//! digests, journal and state classification are portable and unit-tested
//! everywhere.
//!
//! M1b is scratch-first: nothing here touches `/Applications` unless the
//! caller passes that path explicitly, and the binary never escalates.

pub mod apply;
pub mod assessment;
pub mod cli;
pub mod digest;
pub mod journal;
pub mod lock;
pub mod macho;
pub mod platform;
pub mod state;
pub mod uninstall;
pub mod whitelist;

/// The dependency the installer adds to the vendor stub.
pub const LOADER_DEPENDENCY: &str = "@executable_path/../Frameworks/libparsec_orbit_loader.dylib";
/// Loader location inside the patched bundle.
pub const LOADER_RELATIVE: &str = "Contents/Frameworks/libparsec_orbit_loader.dylib";
/// Installation record consumed by the loader constructor.
pub const RECORD_RELATIVE: &str = "Contents/Resources/orbit-install.json";
/// The vendor bootstrap inside the bundle.
pub const STUB_RELATIVE: &str = "Contents/MacOS/parsecd";
/// Verified seed source inside the bundle.
pub const SKEL_RELATIVE: &str = "Contents/Resources/skel";
/// Entitlement file written next to the staged bundle.
pub const ENTITLEMENTS_FILE: &str = "disable-library-validation.entitlements";

pub use apply::{
    apply, apply_with, ApplyFailure, ApplyOptions, ApplyOutcome, ApplyReport, Faults,
    PreseedOutcome, Step,
};
pub use journal::{InstallDir, InstallState, Manifest};
pub use state::{AppClass, Classification};
pub use uninstall::{
    uninstall, uninstall_with, UninstallFailure, UninstallFaults, UninstallOptions,
    UninstallReport, UninstallStep,
};
pub use whitelist::Whitelist;
