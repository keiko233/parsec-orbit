# 0012 - macOS launch is a normal app launch of `Parsec Orbit.app`; the installer signs

Status: Superseded as the default product route by [ADR 0014](0014-macos-patched-bundle.md). This record preserves the earlier host-copy decision.

## Context
The Windows installer drops `bcrypt.dll` next to the client, and starting Parsec as usual loads Orbit. The maintainer wants the same seamlessness on macOS: no command line and no wrapper script to run. On macOS the launched thing must be the host (a signed copy), not the vendor app, because the vendor app cannot be injected or modified (invariant 1).

## Decision
1. The installer (`scripts/install.sh`) creates `~/Applications/Parsec Orbit.app` whose executable is the stub copy, installs the loader dylib and the selected plugins, and signs everything ad-hoc on the user's machine. No Developer ID, no administrator rights.
2. Opening `Parsec Orbit.app` by any normal means (Finder, Spotlight, Dock, login item) starts the host with the loader injected. How the environment variable is delivered (`LSEnvironment` or a tiny launcher that `exec`s the host) is decided by measurement in M1.
3. The vendor app starts a plain client; Orbit does not intercept it.
4. The installer verifies the vendor stub is unchanged, and release archives contain no vendor binary. `uninstall.sh` removes the app and keeps user data unless `--purge`.
5. Claiming the `parsec://` URL scheme or setting a login item is not done by default.

## Consequences
* "Seamless" means seamless for `Parsec Orbit.app`; a user who wants Orbit launches that app (and may put it in the Dock instead of the vendor's).
* Windows packaging scripts have shell counterparts (`install.sh`, `uninstall.sh`, `package.sh`).
* The signing environment is the user's machine, so the installer needs `codesign` and `lipo`.
* Downloaded release archives are quarantined; the installer is the supported path, manual copying is not.
