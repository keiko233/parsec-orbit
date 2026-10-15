# Market

Official [Parsec Orbit](../../README.md) plugin. It browses, installs, updates
and removes plugins from the signed registries, inside the client. The section
is `Market` in the Orbit window (`com.parsec-orbit.market`); it needs the
companion window service (`parsec-orbit.ui`).

## Use

1. List the registries in `<Orbit root>/market/sources.toml` (see
   [registry.md](../../docs/spec/registry.md#sourcestoml)). The keys of the
   publishers you trust go in `<Orbit root>/market/trusted-keys.toml`.
2. Open the Market section and press **Refresh**. Only an index signed by a
   trusted key is listed.
3. Pick a plugin. **Install** adds it (and the libraries it requires) to the
   plugins folder. **Update** stages a newer release; **Remove** deletes a
   plugin the market installed.

Installs and removals take effect at the next start of Parsec. An update is
written to `<Orbit root>/pending/`, which the loader swaps in before it loads
anything, so it is safe while Parsec runs.

## Files

* `plugin.toml` - the manifest the loader reads.
* `src/controller.rs` - the state of the section and what each action asks for.
* `src/worker.rs` - the registry client, run on one thread.
* `src/lib.rs` - the plugin entry point.

The design is [restructure.md section 4](../../docs/design/restructure.md#4-market-as-a-plugin),
the decision [ADR 0020](../../docs/design/decisions/0020-market-is-a-plugin-over-registry.md).
