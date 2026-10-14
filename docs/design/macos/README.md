# macOS support research

Status: **M0 and M1a/M1b implemented; M1c local ad-hoc/original-path activation measured; M2 independent local scenarios implemented (2026-10-15). Session, distribution and internals-dependent gates remain open.** This directory records evidence, not normative requirements. Current specifications: [macos-host.md](../../spec/macos-host.md), [macos-platform.md](../../spec/macos-platform.md). Current decision: [ADR 0014](../decisions/0014-macos-patched-bundle.md). Implementation gates: [macos-phases.md](../macos-phases.md).

## Current finding

The untouched vendor bundle/signature has no established in-process Orbit entry. With local modification allowed, a dependency load command added to a supported arm64 bootstrap, re-signed with Hardened Runtime plus disable-library-validation, loaded Orbit on ordinary Finder/Launch Services launch in a copy. The official client updated/re-execed, the real core/SDK hello activated and a visible label appeared.

The later [installer checkpoint](installer-checkpoint.md) records original `/Applications` path activation, administrator restore regression and persistent installation. Existing Dock entries, credentials/TCC and published distribution remain unverified. Strict codesign verification passed, while spctl rejected the ad-hoc copy. Re-signing replaces the original vendor identity/seal; no system-wide protections were changed. Spicetify contributes the version-aware patch/apply/restore model, not a Parsec resource-JS entry.

## Reading order

1. [bootstrap-checkpoint.md](bootstrap-checkpoint.md): guarded activation, acceptance measurements, audit capture limits and remaining work.
2. [installer-checkpoint.md](installer-checkpoint.md): the M1b transaction, on-disk layout, stub whitelist, signing order, scratch evidence and the explicit unverified list.
3. [native-dependency-poc.md](native-dependency-poc.md): measurements, signing/fault matrices, isolation discovery, limitations and reproduction contract.
4. [extension-projects.md](extension-projects.md): pinned Spicetify/Vencord/SpotX source investigation and route comparison.
5. [launch-integration.md](launch-integration.md): earlier launch investigation under an untouched-bundle constraint. Its negative verdict is scoped to that constraint.
6. Historical implementation evidence below, then the current spec/ADR/phase plan.

## Historical evidence

| Document | Scope |
| --- | --- |
| [windows-surface.md](windows-surface.md) | per-crate dependency inventory: portable, small shim, Windows-only |
| [loading-constraints.md](loading-constraints.md) | early toy signing/dyld experiments and evaluated options |
| [client-internals.md](client-internals.md) | static bootstrap, state/settings and client UI mapping |
| [loader-as-host.md](loader-as-host.md) | two-gate correction and wx_main host contract |
| [smoke-test.md](smoke-test.md) | earlier bare-copy injected-host run, update and idle evidence |

Earlier research pages retain their dated conclusions. They do not override ADR 0014 or convert plain-host executable-memory observations into HR-host guarantees. Loader-as-Host remains a development/fallback option, not the current normal-official-app target.

## Terms and important boundaries

- Bootstrap: small `parsecd` launcher; client dylib: versioned implementation with `wx_main`.
- Host: process running both, now targeted at the patched installed app. Historical runs used separate copies.
- Orbit loader: native dependency providing core/Platform/Logger. M0 implemented the product Platform/Logger and non-Parsec core/SDK tests; guarded production bootstrap is implemented with the acceptance limits above.
- Independent state: Orbit-owned `.parsec` plus a shared-persistent replacement. No credential migration is planned; separate sign-in remains required.

The historical experiments below used copies and scratch state. Later original-path transactions were separately authorized and are recorded in the installer checkpoint. The newer PoC discovered and initially read real shared `.parsec-persistent/devid.bin`; writes were denied, then later builds redirected observed APIs and denied original-path reads. Do not describe that first run or the partial mapper as complete read isolation.

The login window had a native view and no WKWebView in its measured view tree. WebView code in the client does not prove an available page/DOM for every account. Hooks/UI/client-session support remain per-build runtime gates.

## Corrections and unresolved gates

- The earlier core test failure was alignment, not header size: the header is two u32 (8 bytes, 4-byte aligned); the registry expects pointer alignment. M0 fixed the fixture.
- Copy window/bundle/self-exec and native SDK activation passed in the new PoC; original-path UX, full isolation, signing/distribution and actual connected sessions remain open.
- Empty-state first launch exited 2; preseeded first launch passed. The retained appdata parse state is a static candidate explanation, not a traced runtime root cause.
- Weak dependency loading recovered a missing/unparseable image in toy tests, but not invalid signature or constructor abort.
- Client-dylib update is distinct from full app/stub replacement and re-apply. Restore was exercised only in scratch.

## Archive policy

Older raw logs/disassembly live in the local `/tmp/parsec-orbit-research/` archive. New fixtures/logs live in `/tmp/parsec-orbit-poc-20261013/`, with an external source/log ZIP. These are author-local paths, not prerequisites for reading the durable findings here. Vendor binaries are not committed or redistributed. Reproduction must use the user's own pristine app, fresh private roots and the scoped safety/identity checks in the PoC document.
