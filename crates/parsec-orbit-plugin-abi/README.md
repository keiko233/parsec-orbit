# parsec-orbit-plugin-abi

The C ABI between the [Parsec Orbit](https://github.com/keiko233/parsec-orbit)
loader and its plugins: the plugin descriptor, the host table, the interface
header and kind-handler tables, error codes and export names. `no_std`, no
dependencies, no allocation.

Most plugins should depend on
[`parsec-orbit-plugin-sdk`](https://crates.io/crates/parsec-orbit-plugin-sdk),
which re-exports this crate and adds the safe API. Depend on this crate
directly to implement a loader, a tool or an SDK for another language against
the same definitions.

* Layouts are frozen by tests; the C equivalent is documented in
  [`docs/spec/plugin-abi.md`](https://github.com/keiko233/parsec-orbit/blob/main/docs/spec/plugin-abi.md)
  and a test fails when the two drift.
* Structs only grow at the tail. `ParsecOrbitHost` revision 2 (per-plugin
  tables, `log_ex`, the interface registry) is appended after the revision 1
  fields, so plugins built against either revision keep working.

Windows only in practice (the loader is a `bcrypt.dll` proxy), but the crate
itself builds anywhere.

## License

MIT
