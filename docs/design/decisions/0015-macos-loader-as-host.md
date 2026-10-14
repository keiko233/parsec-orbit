# 0015 - macOS runs the client from a host we control

Status: Superseded as the default product route by [ADR 0014](0014-macos-patched-bundle.md). This record preserves the earlier host-copy decision.

## Context
Windows loads `bcrypt.dll` from the client directory; macOS has no such slot. The Parsec macOS client is hardened-runtime, library-validated and team-pinned: `DYLD_INSERT_LIBRARIES` into the installed app is stripped, a planted client dylib fails both the stub's SHA-256 check and dyld's team check, and the stub has no `LC_RPATH` (all measured, see [loading-constraints](../macos/loading-constraints.md)). Re-signing the installed app would break its notarized seal and must be redone after every update. The alternative evaluated next, a host we control, was confirmed end to end by the [smoke test](../macos/smoke-test.md): a re-signed ad-hoc copy of the 266 KB bootstrap stub, started with our dylib injected, loaded the real signed client dylib, was accepted with `LOADER_V14`, self-updated inside an isolated directory, and left the installed app and the live client untouched.

## Decision
1. The **host** is a copy of the vendor bootstrap stub, made from the user's installed Parsec app on the user's machine and signed ad-hoc by the installer. The vendor bootstrap runs unchanged. A from-scratch host (reimplementing the bootstrap) is not built.
2. The **loader dylib** is injected with `DYLD_INSERT_LIBRARIES` into that host only. It activates only when the process's main executable is the sibling host executable of the loader dylib; in every other process it does nothing and its interposed functions forward unchanged.
3. The installed vendor app is never modified, re-signed or injected (invariant 1 of [macos-host.md](../../spec/macos-host.md#2-invariants)).

## Alternatives rejected
* Re-sign the installed app with the two dyld entitlements: breaks the seal, redone per update, drops profile-gated entitlements.
* Plant a client dylib in the state directory: blocked by two independent gates.
* Out-of-process companion: cannot host in-process plugins.
* Own host (`wx_main` called from our code): possible (the contract is mapped, the helper library is MIT), but it reimplements crash counters, `appdata.json` and restart semantics that we only partly mapped. It stays allowed as an implementation of the same host contract ([macos-host.md](../../spec/macos-host.md#5-host-contract-what-the-bootstrap-does)).

## Consequences
* The user runs the client from `Parsec Orbit.app`; the vendor app keeps starting a plain client.
* The vendor app must be installed (seed source). A newer vendor stub is not picked up until the installer runs again.
* The macOS loader is a new crate with a different library name (`libparsec_orbit_loader.dylib`); the Windows `bcrypt` crate is unchanged.
* Compatibility depends on three vendor surfaces we cannot control: the bootstrap contract, the `LOADER_V` token and the client dylib build ([ADR 0013](0013-macos-client-compatibility.md)).
