# 0024 - Integrate the UI companion with the crate role layout

## Context

The directory restructure (ADR 0018, crate roles) and independent UI renderer
(ADR 0018, UI companion) landed in parallel. The renderer moves GUI dependencies
out of the UI service, but shares serializable schema and protocol types with it.
The original dependency law allowed service providers to link only the SDK.

## Decision

Keep the service in `libs/parsec-orbit-lib-ui`, the shared protocol in
`crates/parsec-orbit-ui-protocol`, and the executable in
`tools/parsec-orbit-ui-companion`. Permit the UI service to link the shared
protocol as a narrow exception to the SDK-only rule; enforce it in the existing
architecture test. Keep GUI dependencies solely in the executable.

Windows packages produced by the Rust release tool include the helper beside
the UI service DLL and refuse packaging when the helper has not been built.
The legacy public packaging policy and macOS artifact allowlist stay deferred
to the release cutover.

## Consequences

The merged tree preserves both process isolation and crate roles without
reintroducing `sdk-std` or GUI dependencies into the client process. Development
installation and the Rust package command use the matching helper executable.
