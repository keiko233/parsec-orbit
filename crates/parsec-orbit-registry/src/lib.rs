//! Finding, checking and installing Parsec Orbit plugins.
//!
//! * [`index`] - the list of plugins and releases, with sha256 digests,
//! * [`trust`] - digests and ed25519 signatures: what makes a download
//!   trustworthy,
//! * [`package`] - reading a plugin zip without trusting it,
//! * [`install`] - installing, updating and removing below a plugins
//!   directory,
//! * [`source`] - where bytes come from (files, https),
//! * [`cli`] - the command line.
//!
//! The specification is `docs/spec/marketplace.md`.

pub mod cache;
pub mod cli;
pub mod index;
pub mod install;
pub mod package;
pub mod source;
pub mod trust;

use std::fmt;

/// ABI version of the loader this tool installs for.
pub const LOADER_API: u32 = 1;

/// Everything that can go wrong, with the text shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Wrong arguments.
    Usage(String),
    /// The index is unreadable or inconsistent.
    Index(String),
    /// A plugin or version that is not there.
    NotFound(String),
    /// A release that needs a newer loader.
    Incompatible(String),
    /// A package whose digest differs from the index.
    Hash(String),
    /// A missing, malformed or unverifiable signature or key.
    Signature(String),
    /// A package that is not what the index promised, or not safe to unpack.
    Package(String),
    /// Something that would be overwritten.
    Conflict(String),
    /// Files.
    Io(String),
    /// The network.
    Network(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Error::Usage(text)
        | Error::Index(text)
        | Error::NotFound(text)
        | Error::Incompatible(text)
        | Error::Hash(text)
        | Error::Signature(text)
        | Error::Package(text)
        | Error::Conflict(text)
        | Error::Io(text)
        | Error::Network(text)) = self;
        f.write_str(text)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error.to_string())
    }
}
