//! Market - official Parsec Orbit plugin that browses, installs, updates and
//! removes plugins from the signed registries, inside the client.
//!
//! The pieces: [`controller`] holds the state of the Market section and turns
//! UI actions into commands; `docs/design/restructure.md` section 4 is the design
//! and `docs/spec/registry.md` the section keys.

pub mod controller;
