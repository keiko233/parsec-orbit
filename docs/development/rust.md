# Rust code style

This guide records the existing Rust style and lint setup. It does not introduce a
new lint profile. Follow [Architecture](../design/architecture.md) for the crate
graph and the loader's size and concurrency policy, and
[Testing and review](testing.md) for tests.

## Formatting and imports

There is no `rustfmt.toml`; the workspace uses default Rustfmt and CI checks it.

| Setting             | Current convention                                                          |
| ------------------- | --------------------------------------------------------------------------- |
| Line width          | Rustfmt default (100 columns)                                               |
| Indentation         | Four spaces, no hard tabs                                                   |
| Edition             | 2021 (`[workspace.package] edition`)                                        |
| Toolchain           | Stable (`dtolnay/rust-toolchain@stable` with the `rustfmt` component in CI) |
| Target              | `x86_64-pc-windows-msvc`, static CRT via `.cargo/config.toml`               |
| Crate directory     | Equal to the package name                                                   |

Let Rustfmt format imports, multiline expressions, attributes, and where clauses.
Use `snake_case` for modules/functions/fields, `PascalCase` for types and variants,
and `SCREAMING_SNAKE_CASE` for constants. Group imports as the surrounding module
does: `std` first, then workspace crates, then `windows-sys`. Match the surrounding
module's API and visibility conventions; avoid unrelated formatting or module
reorganizations.

## Types, errors, and ownership

- There is no `anyhow` or `thiserror`; each layer keeps its own error type and the
  consuming crate decides the convention. `parsec-orbit-core` returns
  `Result<T, String>` (the text goes into the log as-is); the loader's Win32 layer
  returns `Result<T, u32>` (the Win32 error code); `parsec-orbit-market` has an
  `Error` enum with a manual `Display` and `From<std::io::Error>`;
  `parsec-orbit-plugin-sdk` has `Error { code, message }` with a `Result` alias.
- Propagate recoverable errors with `Result` and `?`. Follow the consuming crate's
  existing error type instead of adding a workspace-wide error convention.
- `no_std` is a hard boundary. `parsec-orbit-plugin-abi` and
  `parsec-orbit-plugin-sdk` are `#![no_std]` with `#[cfg(test)] extern crate std`.
  Do not add a `std` feature: a `std` plugin links `std` itself and uses the
  same SDK, and the `std`-only `MockHost` is the dev-dependency
  `parsec-orbit-plugin-sdk-testing` ([ADR 0002](../design/decisions/0002-sdk-is-no-std-by-default.md)).
- Keep public traits and methods narrow (`Platform`, `Logger`, `console::Console`).
  Preserve ownership and borrowing instead of introducing locks or clones solely
  to bypass a design problem.
- A panic represents an invariant violation. `panic = "abort"` is set in every
  profile because a `no_std` plugin cannot unwind; do not write `catch_unwind` in
  production code.
- `unsafe` is confined to the FFI boundaries (the loader, the ABI and the
  ABI-facing plugin code). Keep the loading logic in `parsec-orbit-core` free of
  it, and document the invariant next to each `unsafe` block.
- Use `#[cfg(...)]` for platform and test boundaries. Keep test-only APIs behind
  `#[cfg(test)]` or the `testutil` feature, out of the production interface.
- Comments and doc comments explain invariants, lifecycle assumptions, and the
  ABI. Put `///` on public items, use intra-doc links, and point to the matching
  `docs/spec/` document.

## Checks

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked
```

CI runs both. Clippy is not part of the CI gate; run it on touched code as a review
aid:

```powershell
cargo clippy --workspace --all-targets
```

To apply formatting, run `cargo fmt --all`. Fix issues in touched code. If an
allowance is needed, keep it at the narrowest relevant scope and explain the
specific reason. Prefer measuring before adding a dependency that grows the loader
or a plugin (see the [size policy](../design/architecture.md#size-policy)).
