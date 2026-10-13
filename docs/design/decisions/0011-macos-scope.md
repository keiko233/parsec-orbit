# 0011 - macOS scope: arm64, client mode

Status: Partially superseded by [ADR 0014](0014-macos-patched-bundle.md) for signing/distribution research and entitlements. arm64/client-only scope remains.

## Context
Hosting needs the profile-gated `persistent-content-capture` entitlement, which an ad-hoc signature cannot carry; whether the client works as a host under the ad-hoc host was never tested. The x86_64 slice could not be run (no Rosetta in the research environment). The maintainer defined the first target.

## Decision
Supported: macOS on `aarch64` (Apple silicon), client mode. Out of scope, not "later in this phase": hosting (sharing the Mac's screen, audio, input), `x86_64`, Developer ID signing and notarization, an out-of-process companion. The installer thins the stub to its `arm64` slice and refuses other machines. The out-of-scope items may be taken up by a new decision record.

## Consequences
* The host has no entitlements; no spec text promises behaviour that needs them.
* Microphone and camera behaviour under the ad-hoc host is recorded as unverified and not a supported feature.
* `x86_64` support will need the dyld/signing measurements repeated on an Intel run.
