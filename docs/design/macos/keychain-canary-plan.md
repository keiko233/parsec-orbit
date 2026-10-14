# Keychain boundary measurement

Date: 2026-10-13. **Executed in two separate, narrowly scoped transactions
(the App Sandbox client under the original authorization; a custom-SBPL client
under the later "continue implementing" instruction); every owned dummy item
was deleted and exact post-delete queries confirmed absence.** This is an owned
canary, not a Parsec launch, signing-identity change, credential migration or
product isolation backend.

The file/socket App Sandbox canary does not establish a Keychain RPC boundary.
`scripts/macos-keychain-canary.py prepare` builds an owned controller and an
owned unique-id client in private scratch; `--client` selects the App Sandbox
entitlement client or (`--client sbpl`) a client whose constructor installs the
measured custom SBPL deny profile with the sentinel gate before probing. Both
clients retain Hardened Runtime and disable library validation; no vendor code
is used. The controller/client hashes, exact two UUID-bound services, account and
fake-payload scope are saved in `keychain-plan.json` for review. Preparing
only compiles/signs files and performs no Security API execution.

Five mocked transaction tests cover exact-scope cleanup, a timed-out add,
cleanup failure reporting, a successful delete response without actual absence,
and changed-payload rejection. They run in macOS
CI and make no Security calls; they verify controller error handling, not
Keychain enforcement.

After explicit authorization, `run PLAN` uses the legacy login Keychain route
(`kSecUseDataProtectionKeychain=false`) and tests these two generic-password
items:

| Own dummy item | Access configuration | Purpose |
| --- | --- | --- |
| `…controller-only` | Default access trusts only the newly built controller | Separate ordinary item ACL denial from a sandbox-wide boundary |
| `…explicit-client` | Access explicitly trusts that controller and the newly built sandbox client | Determine whether permitted Keychain RPC can return this own fake payload despite filesystem sandboxing |

Queries specify the exact service, account and class; no real Parsec item is
named, enumerated or requested. The fake payload is generated from the fixture
UUID. Results record OSStatus and whether it matched, never real credentials.
This tests a deliberately granted synthetic item, not the unknown ACLs of any
existing account item. It cannot establish real-user or data-protection
Keychain behavior merely from the legacy result.

The programs disable interaction and request `kSecUseAuthenticationUIFail` for
queries/deletion; failure to disable legacy interaction aborts before probes.
A locked, unavailable or deprecated API path remains a reported limitation;
the fixture does not unlock a Keychain or ask for a password. Cleanup covers
every attempted creation, including a timed-out add, and deletes only the same
two own item keys. Cleanup failures are reported with exact identifiers for
manual recovery, rather than concealed. The OS may create the client's new,
uniquely named app container; existing containers are not read or removed.

Measured plan:
`/private/tmp/orbit-kc-tapo91ze/keychain-plan.json`.
The runner verifies the owned scratch directory and payload hashes before
execution. Creating/querying/deleting these dummy items changes the existing
login Keychain, so authorization is required before invoking:

```sh
python3 scripts/macos-keychain-canary.py run /absolute/path/to/keychain-plan.json
```

Apple's [SecAccessCreate documentation](https://developer.apple.com/documentation/security/secaccesscreate(_:_:_:))
describes the default creator trust list; its
[ACL documentation](https://developer.apple.com/documentation/security/access-control-lists)
explains that item access configuration is separate from filesystem access.
These sources justify measuring the distinction; no result is assumed in advance.

## Measured result

The authorized run used the unchanged, hash-validated controller and App
Sandbox client. Both creations returned success; the controller read and
matched both fake payloads before the client probe.

| Own dummy item | Sandbox client OSStatus | Fake payload matched | Delete OSStatus | Post-delete controller query |
| --- | --- | --- | --- | --- |
| Controller-only | `-25293` | No | `0` | `-25300`, absent |
| Explicit-client | `-25293` | No | `0` | `-25300`, absent |

The local Security SDK identifies `-25293` as `errSecAuthFailed` and `-25300`
as `errSecItemNotFound`. The client failed authorization/authentication even
with explicit item trust. This is an observed failure to read these two legacy
items, not proof of a sandbox-wide Keychain boundary: the experiment does not
separate sandbox enforcement from signing/ACL identity or other authorization
conditions, and does not exercise data-protection Keychain or vendor behavior.
No authentication UI or unlock fallback was used.

Artifacts:
`/private/tmp/orbit-kc-tapo91ze/keychain-result.json` and
`/private/tmp/orbit-kc-tapo91ze/keychain-cleanup-verification.json`.
The latter records only exact queries of the two owned keys after deletion.
The runner now also performs this absence check after every attempted item
cleanup and reports failure if a successful delete response is not followed
by the expected item-not-found result. This change was verified with mocks;
the real transaction was not repeated.

### Custom-SBPL client measurement (2026-10-13 continuation)

A second transaction measured the re-signed custom-profile client that the
mechanism assessment selected. `prepare --client sbpl` compiled a disposable
client whose constructor installs the shipped rule set — the four real
protected roots with `file-read*`/`file-write*` subpath denies, the
`network-outbound` unix-socket deny, and a literal sentinel read deny — then
requires the sentinel open to fail before probing. The client carried only
Hardened Runtime and disable-library-validation; no App Sandbox entitlement was
present. Both creations and both controller payload checks succeeded, the
sentinel gate reported `verified=1 errno=1` (EPERM), and both client probes
returned `-25293` with no payload match. Both deletes returned `0`, and the
exact post-delete controller queries returned `-25300` (absent).

| Own dummy item | Custom-SBPL client OSStatus | Fake payload matched | Delete OSStatus | Post-delete controller query |
| --- | --- | --- | --- | --- |
| Controller-only | `-25293` | No | `0` | `-25300`, absent |
| Explicit-client | `-25293` | No | `0` | `-25300`, absent |

Artifacts: `/private/tmp/orbit-kc-e404qxk1/keychain-plan.json` and
`/private/tmp/orbit-kc-e404qxk1/keychain-result.json` (the result embeds the
exact client profile text and the gate line). The result is identical to the
App Sandbox client's, so this still separates neither sandbox enforcement from
signing/ACL identity nor legacy from data-protection Keychain behavior; the
same fake-item-only scope, no-enumeration, no-UI constraints held. This was a
fresh narrow action taken under the "continue implementing" instruction, not a
reuse of the spent App Sandbox authorization.
