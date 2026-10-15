# 0020 - The market is a plugin over the registry library

## Context
The first market was a stand-alone command-line tool. Users browse and install from inside the client, and the installer, the release tool and the client need the same index, signature and package checks. Two implementations would drift, and the checks are the part that must not.

The client also has a UI service (`parsec-orbit.ui`) that shows declarative sections, and a Windows loader that maps plugins into a running client, so a loaded DLL cannot be replaced in place.

## Decision
The checks live once, in `parsec-orbit-registry`, which is a shared library. The in-client market is a plugin, `com.parsec-orbit.market`, built on the SDK and the registry, and it registers one section of the existing UI ABI with no new item type. The section's keys are in [registry.md](../../spec/registry.md#in-client-market-plugin).

Inside the plugin, a pure controller turns actions into commands and a single worker thread runs them. Installs go to `plugins/`. Updates are staged in `pending/` and promoted by the loader at the next start ([ADR 0023](0023-staged-update-promotion.md)). Removal deletes the installed folder and any staged copy of it.

The registry sources are listed in `market/sources.toml`, a person-written TOML file read by the registry library.

## Consequences
* One implementation of verification serves the installer, the release tool and the client. A fix to the checks reaches all three.
* The plugin links the registry's HTTP and archive dependencies. They are paid only by the market plugin, and the loader stays light ([ADR 0004](0004-loader-stays-light.md)).
* The section can only show what the UI ABI offers. The dropdown options are fixed at registration, so a change of the list re-registers the section and resets the user's place until a dynamic-options item exists ([restructure.md section 4.4](../restructure.md#44-ui-interface-reuse-of-parsec-orbitui)).
* An update takes effect at the next start of Parsec, not at once.
* The market is not a reserved `parsec-orbit.market` interface: no other plugin needs one, and `market/` stays the install record.
