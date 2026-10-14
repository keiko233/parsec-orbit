# Development standards

These are the current development rules for human contributors and coding agents.
Read them before changing behavior. They are the single source of the detailed
rules; [`AGENTS.md`](../../AGENTS.md) adds agent behavior guidelines, routes to
these guides, and restates only the most frequently violated rules.

## Choose a guide

| When you are working on                                              | Read                                            |
| -------------------------------------------------------------------- | ----------------------------------------------- |
| Repository scripts, worktrees, or commits                            | [Development workflow](workflow.md)             |
| Tests, mocks, verification, or final review                          | [Testing and review](testing.md)                |
| Rust style and current formatter/lint behavior                       | [Rust code style](rust.md)                      |
| Components, crate graph, size and concurrency policy                 | [Architecture](../design/architecture.md)       |
| ABI, manifest, loading rules, host services, or SDK APIs             | [Specifications](../spec/)                      |
| How to write a Rust or TypeScript plugin                             | [Guides](../guides/)                            |
| Why a design decision was made                                       | [Decision records](../design/decisions/)        |

Start with the [root README](../../README.md) for building and installing the
loader, and the [documentation index](../README.md) for the map of `docs/`.

## Rules to keep in mind

- Keep the loader tiny and boring; plugins do not pay for what they do not use.
- Construct dependencies explicitly and keep mutable state with its owner; do not
  add process-global mutable state.
- A change in behavior changes the matching spec under `docs/spec/` in the same
  commit.
- Write the test or reproduce the failure first, then make it pass.
- Keep changes focused, verify the intended behavior, and submit atomic commits.
- Prefer isolated worktrees for feature work; ask before choosing the current
  checkout, and reuse the Cargo `target/` where the filesystem supports a block
  clone.

## Maintaining these rules

Update the relevant guide when changing a shared rule, and update `AGENTS.md` in
the same change only if it restates that rule.
`CLAUDE.md` imports `AGENTS.md`, so the same instructions apply there. Neither
human nor agent documentation should introduce a separate architectural exception.

The documents under `docs/spec/`, `docs/design/` and `docs/design/decisions/` are
the project's normative and historical record. Use these guides and `AGENTS.md`
for current rules.
