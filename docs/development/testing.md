# Testing and review

## Design tests around boundaries

- Test the pure loading logic in `parsec-orbit-core` directly with plain values;
  it is written against traits, so it never maps a DLL.
- Put infrastructure behind the small traits the crates already define
  (`Platform`, `Logger`, `console::Console`) and inject a fake in tests.
- Reuse the existing doubles instead of building a Parsec client: `loader-host`
  stands in for `parsecd.exe`; the e2e sandbox redirects `%APPDATA%` and `%TEMP%`
  and installs a throwaway `bcrypt.dll`; the native patcher runs against an
  in-memory fake PE image; the WebView bridge runs against a fake DevTools
  endpoint; `parsec-orbit-plugin-sdk-std` ships `MockHost`.
- Never touch an installed Parsec client or a real user configuration. Use
  temporary or injected paths.

## Where tests live

| Kind                     | Location                                         | Command                                        |
| ------------------------ | ------------------------------------------------ | ---------------------------------------------- |
| Unit                     | inline `#[cfg(test)] mod tests` in the same file | `cargo test --workspace --locked`              |
| Integration              | `crates/<crate>/tests/`                          | same                                           |
| End-to-end               | `tests/parsec-orbit-e2e/`                        | same                                           |
| Fixtures                 | `tests/parsec-orbit-fixture-*`                   | same                                           |
| JavaScript SDK and sample | `javascript-sdk/test/`, `examples/*/test/`       | `pnpm -r build && pnpm -r test` (`node --test`) |

The end-to-end suite builds the loader, the plugins and the fixtures into
`target/e2e` and runs them inside a real process. Its WebView script behaviour
test needs Node; CI sets `PARSEC_ORBIT_REQUIRE_NODE=1` so a missing Node fails
instead of silently skipping.

## Run the relevant checks

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked
pnpm install --frozen-lockfile; pnpm -r build; pnpm -r test
```

After changing a published crate, also package it:

```powershell
cargo package -p parsec-orbit-plugin-abi -p parsec-orbit-plugin-sdk -p parsec-orbit-plugin-sdk-std --locked
```

Choose the tests that exercise the changed behavior. A formatting pass is not a
review.

## Review before submitting

- Each changed line supports the request; assumptions and success criteria are
  clear.
- A behavior change updates the matching `docs/spec/` in the same commit.
- The loader and `parsec-orbit-core` stay light: no new dependency that grows the
  loader or a plugin without a measured reason.
- No new process-global mutable state; dependencies are constructed explicitly.
- Tests use plain values, injection, or the existing doubles and verify meaningful
  behavior.
- Relevant checks pass, and any limitation that could not be run is recorded.
- Language style follows [Rust code style](rust.md).
- The commit is atomic and only related files are staged.
