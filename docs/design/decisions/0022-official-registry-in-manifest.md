# 0022 - The official registry is `manifest/` on `main`

## Context
The official plugin index and the loader and installer descriptions were assembled by release scripts from the build outputs, and published only as release assets. Nothing that listed a plugin was reviewed, and a release could not be traced back to a change. Third-party packages need a listing too, but they must not be able to publish under the official id prefix. See [restructure.md](../restructure.md#43-the-official-registry-manifest-on-main).

## Decision
The catalogue lives in `manifest/` on `main` as small TOML sources: `plugins/<id>/entry.toml` (identity and presentation), `plugins/<id>/releases.toml` (one `[[release]]` per package with its URL and sha256) and `host/releases.toml` (loader and installer). Packages stay on release assets or HTTPS URLs and are never committed. `parsec-orbit-release registry build --sign` generates `index.json` and `host.json` and their signatures, and those are the only files clients read. Ids starting with `com.parsec-orbit.` are reserved for entries with a `[build]` table. The release job appends `[[release]]` blocks as text; a person edits only `yanked`.

## Consequences
* Every change to the official registry is a reviewed commit, and git history is its audit log.
* The signature, not the hosting, makes the registry trustworthy: a package is trusted by the sha256 and size in a signed index.
* The sources are TOML with no writer dependency: the tool appends text and reads with serde, so hand-written content is never rewritten.
* Registry changes need the release job (or a maintainer) to run `registry build`; a source edit alone changes nothing a client sees until the signed files are regenerated.
* Third-party packages are listed by pull request and carry no official status; the `official` field is derived from the id.
