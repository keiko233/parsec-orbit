# 0023 - Staged updates are promoted by the loader

## Context
Windows will not replace a mapped DLL or rename its folder, so a program running inside the client (the market plugin) cannot update a plugin that is already loaded. Requiring the user to close Parsec and run a command defeats an in-client market.

## Decision
An update of a loaded module is written to `<Orbit root>/pending/<id>/`. The loaders call `parsec_orbit_core::staging::promote` before the scan: the staged folder takes the place of the installed one (old one set aside as `<id>.orbit-old`, deleted afterwards, restored if the swap fails). It is `std::fs` only, adds no dependency and is the only loader-side change of the restructure. The paths come from `parsec-orbit-layout` (`managed_plugins_dir`, `pending_dir`). See [loading.md](../../spec/loading.md#0-staged-updates).

## Consequences
* New plugins still install immediately; updates of loaded ones take effect at the next start without a helper program.
* The loader size grows by a few hundred bytes; it is measured and recorded in the size report on Windows (hand-off item).
* A failed promotion never prevents the installed version from loading.
