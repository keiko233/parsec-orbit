# File formats

The rule for every configuration, manifest and metadata file we define. The
design and the full inventory are in
[restructure.md](../design/restructure.md#7-formats-and-single-sources).

| Use | Format |
| --- | --- |
| Written or reviewed by a person: manifests, source metadata, user settings, key lists, policy | **TOML** |
| Exchanged between programs or generated: indexes, install records, caches, stored settings | **JSON** (JSON Lines for append-only records) |
| Required by an external tool or specification (`Cargo.toml`, `package.json`, `tsconfig.json`, GitHub workflows, pnpm files, `SHA256SUMS.txt`) | whatever it requires; not changed |

A file is never both. A generated file is JSON and is not edited. An edited
file is TOML; a program only appends a block to it or converts it once from an
old format and never re-serialises it. No private line formats: a new key list
or server list is TOML.

JSON documents we define:

* an integer `format` (schema version) at the top level, never `schema_version`;
* `snake_case` keys, `kebab-case` enum values;
* timestamps RFC 3339 UTC strings (TOML files use native TOML dates);
  sizes integer bytes; hashes lowercase hex sha256; signatures and keys base64;
* URLs absolute `https`, or relative to the document with no `..`;
* readers ignore unknown keys and refuse an unknown `format`.

One source per fact: the version comes from `Cargo.toml` / `package.json`, a
module's identity from its `plugin.toml`, paths from `parsec-orbit-layout`, JSON
schemas from the Rust types in `parsec-orbit-registry` (specs quote examples that
a test parses). Do not copy a fact into a second file; generate it.
