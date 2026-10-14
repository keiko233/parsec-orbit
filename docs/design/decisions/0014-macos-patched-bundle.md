# 0014 - macOS loads Orbit through a patched official app bundle

Status: Accepted direction; implementation and distribution gates remain open.
Supersedes ADR 0006 and ADR 0012 as the default product route. Partially supersedes ADR 0011's exclusion of signing/distribution research and its no-entitlement assumption. ADR 0007's isolated state remains required, with the additional shared-path coverage below.

## Context

The required experience is opening the installed `Parsec.app` through its existing Finder/Dock entry, with Orbit loaded automatically and no extra launcher. The earlier host-copy design preserves the vendor bundle but does not meet that requirement. The maintainer permits app-file modification; SIP and other global protections must stay enabled.

The [extension-project investigation](../macos/extension-projects.md) shows Spicetify's reusable model is version-aware patch/apply/backup/restore. Its Spotify resource-JS injection does not provide Parsec's native ABI. The [native dependency PoC](../macos/native-dependency-poc.md) instead patched a disposable Parsec stub with `LC_LOAD_DYLIB`, kept Hardened Runtime, added `disable-library-validation`, and completed Finder launch, client self-update/re-exec, real core/SDK plugin Init and interface registration. The login window displayed a visible marker. No DYLD injection variable or extra launcher was used.

The PoC used a separate path/id and ad-hoc signing. `codesign --verify --strict` passed; `spctl --assess` rejected the app. Original-path identity, existing Dock entries, credentials/TCC, full app updates and release distribution remain unverified.

## Decision

1. Implement a version-aware **in-place patched bundle** route as the target. Keep the installed path, executable name and bundle id; patch only a supported arm64 bootstrap's load-command area to add the sibling Orbit loader dependency. Do not replace vendor instruction bytes, its client dylib or the stub's client-hash validation. Reject unknown hashes, invalid layout, insufficient padding and repeated/conflicting patches.
2. Back up the full pristine bundle and metadata before mutation. Stage changes, sign owned dylibs and the outer bundle, verify them, and install transactionally with rollback. Do not silently recapture a patched bundle as the pristine backup or restore an old backup over a new vendor version.
3. Preserve Hardened Runtime, with `com.apple.security.cs.disable-library-validation` as the measured loading exception. It weakens library validation for this application process and changes its signing identity. Do not add allow-dyld-env, JIT, debug or capture exceptions without a separate demonstrated need and decision. No global DYLD environment or system-protection changes.
4. Use Orbit-owned isolated state, including the discovered `/Users/Shared/.parsec-persistent` accesses. Never migrate/read real credentials. Validate roots and home lookup before client execution; preseed fresh state from verified official skel. Partial fopen/stat mapping is an experiment, not a completed isolation backend.
5. Keep constructor work minimal. The PoC uses a strong dependency; weak loading may allow missing files but does not recover from signature corruption or constructor abort. Decide the production strong/weak policy with fault tests; do not promise universal fail-open behavior.
6. Use ad-hoc signatures for Orbit-owned libraries and the modified outer app, including end-user local installation (maintainer decision, 2026-10-15). Retain Hardened Runtime with only disable-library-validation. Developer ID/notarization is not the selected route. Assess and report Gatekeeper/quarantine separately; never silently remove quarantine or disable protections. End-user installation scripts are deferred by the maintainer.
7. Keep Loader-as-Host as an optional development/fallback route, not the default user experience. Release archives contain Orbit-owned code only; vendor files are copied/backed up on the user's machine.
8. Distinguish client-dylib updates from whole-app/stub updates. Detect vendor replacement, validate the new version and require explicit re-apply; unknown versions fail closed at apply. Uninstall restores the corresponding pristine bundle and keeps Orbit data unless the user explicitly requests purge.

## Consequences

- Original vendor seal/notarization/Team identity cannot be retained for the modified outer app. Keychain/TCC, permission prompts, URL handling and updater assumptions must be tested.
- Patching `/Applications` can require administrator or App Management authorization; do not promise a privilege-free install. Request only the concrete necessary authorization after staging/validation, never an unbounded privileged service.
- The independent app is no longer the default launch requirement. Separate sign-in remains a consequence of isolated state.
- arm64/client-only scope remains. Hosting, x86_64 and companion-process plugin execution are not added.
- The PoC is evidence for an entry path, not completion of M0/M1 or verification of production plugin hooks. Implementation gates and per-phase commits are in [macos-phases.md](../macos-phases.md).
