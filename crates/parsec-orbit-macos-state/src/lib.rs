//! Host-state primitives shared by the macOS loader and the installer.
//!
//! The crate exists so that one implementation of the verified seed
//! transaction serves both sides: the loader seeds fresh state at launch,
//! and the installer preseed the same state before the first vendor start.
//! It is a plain rlib on purpose: the loader's own library publishes a
//! `cdylib` as well, and a multi-crate-type dependency would make the
//! workspace build the loader twice with the same output file names.
//!
//! POSIX hosts compile the module; Windows builds it empty, like the loader.

#![cfg(unix)]

mod seed;

pub use seed::{seed, SeedReport};
