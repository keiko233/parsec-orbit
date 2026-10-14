# M1c local ad-hoc signing checkpoint

Date: 2026-10-15. Scope: ordinary-user disposable official Parsec bundle;
the live `/Applications/Parsec.app` and real credentials were not modified.

## Selected route

The maintainer selected ad-hoc signing for both development and end-user
local installation. Developer ID/notarization is not a required delivery
route. End-user installation scripts are explicitly deferred. The existing
transactional Rust installer signs the Orbit loader first and the modified
outer bundle last, retaining Hardened Runtime and only
`com.apple.security.cs.disable-library-validation`; it does not re-sign the
vendor client dylib, remove quarantine, invoke vendor preinstall or change
system protection settings.

`assess <app> [--json]` is read-only. It reports strict signature verification,
raw signing details and entitlements, the separate Gatekeeper exit/output,
and the exact quarantine attribute bytes or absence. A Gatekeeper rejection
is evidence in the report, not a command failure that would hide the other
results. Querying signing identities is not part of `assess`; one explicitly
authorized controller query found zero valid identities, which is irrelevant
to the selected ad-hoc route.

Apple distinguishes Developer ID/notarization from a valid local signature:
[Developer ID](https://developer.apple.com/developer-id/) and
[notarization requirements](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).
An ad-hoc local installation is not a notarized download. Apple's
[app-opening guidance](https://support.apple.com/en-ie/102445) describes
user-controlled decisions for blocked apps; this implementation does not
automate those decisions or remove the warning.

## Measured

The formal installer applied `/private/tmp/orbit-m1c-i30pesbu/Parsec.app`,
preserving the selected copy's path and `tv.parsec.www` identifier. The
original official stub hash was `785f985d...be76d`. Both universal slices'
recorded file-backed sections survived patch/signing. Strict signature
verification passed; `codesign --display --verbose=4` reported
`flags=0x10002(adhoc,runtime)`, `Signature=adhoc`, no TeamIdentifier.
The entitlements report contained the selected disable-LV exception.
`spctl --assess --type execute --verbose=4` returned **3, rejected**.
This source copy had **no quarantine attribute**; it is not a clean downloaded
payload measurement. Evidence: `/tmp/orbit-m1c-assessment.json`.

Direct launch, on installer-preseeded fresh private state, reached a Parsec
window in **one attempt** (PID 20959, start identity `1791991864/706802`).
Config storage, Boa evaluation and native fixture registration passed;
101a-to-104a self-update caused a second loader/plugin initialization.
Launch Services `open -n` then found exactly one process at the explicit
copy path (PID 21739, start identity `1791991917/192424`) and displayed its
Parsec window. Controllers stopped only identity-verified run-owned PIDs.
Real Parsec/shared root lstat records and original/staged bundle payload
hashes were unchanged; this is not a complete syscall isolation audit.

Evidence under the scratch root:

- `acceptance-307969c4/acceptance-result.json`: direct first launch and re-exec.
- `acceptance-3a3b6b43/acceptance-result.json`: explicit Launch Services copy.
- `acceptance-88e28dc4/acceptance-result.json`: signed-out UI inventory run.

The scratch assessment regression sets a **synthetic** quarantine attribute
on an owned toy bundle and checks that assessment preserves both the whole
metadata tree and attribute bytes. It does not download an app or establish
the user experience of a real quarantined installation. Uninstall of the
official copy passed exact pristine metadata/signature checks with no
warnings and retained Orbit data. A further apply/uninstall used the final
strict metadata comparator (`/tmp/orbit-m1c-strict-uninstall.json`).

## Remaining acceptance

Existing Finder/Dock/Spotlight entries at the developer's actual installation,
default URI behavior, TCC/camera/microphone and credential effects remain
unmeasured. An isolated test user/machine is required for that acceptance;
copy path/id preservation and explicit `open -n` do not close it. The chosen
ad-hoc policy removes the external signing-credential dependency, not those
behavioral tests. No release-ready or all-M1-gates claim is made.

The selected launch/restore tool is documented in the
[installer checkpoint](installer-checkpoint.md). End-user scripts and
packaging remain deferred rather than being created implicitly.
