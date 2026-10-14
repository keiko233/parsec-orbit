# 0013 - Client builds are detected, logged and checked against a verified list

## Context
The client self-updates inside the isolated state (150-101a became 150-104a in the smoke test), so the build running can change between two runs. `LOADER_V14` is accepted today; what happens with other tokens is unknown. Some plugins (native bitrate patch, hooks) depend on the layout of one build.

## Decision
At every start the loader identifies the client build (the loaded `parsecd-*.dylib` image name and its SHA-256, plus the `LOADER_V<n>` token), logs it, and compares it with a table compiled into the loader: `verified`, `incompatible`, or absent (`not verified`). An unverified build warns once and Orbit continues; an incompatible build disables the plugins that depend on client internals. The result is offered to plugins as the `parsec-orbit.client` service, and plugins that patch the client must refuse anything not `verified`. A build is added to the table only after the phase verification matrix has been run against it.

## Consequences
* Orbit keeps working across a client update for everything that does not touch client internals.
* Each release of Orbit documents the builds it verified.
* The vendor stub age is tracked through `host.json`; refreshing it is a re-run of the installer.
* Unknown-token behaviour (E5) and a possible rejection by a future bootstrap remain risks of M1.
