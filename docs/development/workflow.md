# Development workflow

## Scope and verification

State assumptions and discuss ambiguous requirements before implementation. Keep
changes focused on the requested behavior, match existing style, and remove only
dead code created by your change. Prefer the smallest complete solution.

Define observable success criteria before editing. For bugs, reproduce the failure
and verify the fix. For refactors, verify behavior before and after. For multi-step
work, pair each step with a check. Explain material tradeoffs and blockers rather
than preserving an undocumented compatibility layer.

Do not merge a pull request or push to `main` while a CI check is red, even when
only one job fails. When `main` turns red, fix it before landing further changes.

## Repository scripts

Automation lives in `scripts/` as PowerShell (`kebab-case.ps1`) with a usage header,
`[CmdletBinding()] param(...)` first, `$ErrorActionPreference = 'Stop'`, and
`-LiteralPath` for every file operation. Python helpers and the injected
`webview-bridge-spy.js` follow the same "explain why and when to rerun" doc-comment
style. CI calls the same scripts a developer runs (`scripts/verify.ps1`,
`scripts/package.ps1`); keep execution logic in one place and keep the pnpm
commands as thin delegates.

## GitHub workflows

- Use `[Category] Action Object` for workflow display names, with the categories
  `CI`, `Release`, `Maintenance`, and `Reusable`.
- Name the actual operation and output. Keep workflow file paths and CI job names
  stable: check callers, badges, documentation, and required checks before renaming
  them.
- Separate adjacent workflow steps with one blank line.

## Worktrees

Feature and refactoring work runs in an isolated git worktree by default; working
in the current checkout is an option when the user chooses it after a cost
assessment. Before implementation, weigh the isolation benefit against dependency
installation, an independent Cargo `target/`, disk use, and any uncommitted changes
already in the checkout. Summarize the costs, state a recommendation, then ask the
user and wait for the choice unless they have already chosen. Reuse a suitable
existing worktree when one is available.

- Keep the worktree outside the repository tree and give it its own writable
  `target/`; never share a `CARGO_TARGET_DIR` or link a writable build directory
  back to the main checkout.
- Do not change the developer's system configuration or existing directory layout
  to prepare a worktree.
- On Windows, `git worktree remove` can fail with `Filename too long` for a
  worktree with `node_modules`/`target`; force-delete with the `\\?\`
  extended-length prefix, then `git worktree prune`.

## Git commits

### Stage only related files

Before committing, run `git status` to review the changes, stage only the files
related to this change with explicit paths (`git add <specific-path>`), then verify
with `git diff --cached --stat`.

Never use blanket staging such as `git add .`, `-A`, `--all`, or `*`. If something
was staged by mistake, unstage it with `git reset HEAD <path>`.

### One commit does one thing

Every commit must be atomic, complete, and buildable.

When the user requests phased delivery, commit each completed, verified phase as
work progresses. Split phases by complete behavior, not by arbitrary file batches;
each commit must build without depending on later phases. Fold corrections into
the unpushed commit they complete, following the rules below.

- One indivisible task is one commit.
- Multiple independent tasks are split into multiple commits.
- Do not commit code you know is broken.
- Do not make fix-up (patch-style) commits on a development branch.

If a commit on a development branch is flawed and has not been pushed, fix it with
`git reset --soft HEAD~1` and recommit. If it has already been pushed, any rewrite,
amend, or force push requires explicit consent first.

Self-check before committing: does this change complete or correct the previous
commit? If yes, fold it into the previous commit with `git reset --soft HEAD~1` and
recommit instead of creating a new one. Even when two commits are each individually
clean, a later commit that completes an earlier one is still a fix-up commit.

Exploratory work may live on `temp/`, `wip/`, or `scratch/` branches. Do not merge
those directly; create a clean branch and reorganize the work into atomic commits.

### Commit message content

The subject states what changed; the body explains why when the problem or the fix
is not obvious.

Subject rules:

- Use the imperative mood, stay within 72 characters, and do not end with a period.
- Describe the behavior or capability.

Body rules:

- A non-trivial change must have a body; the body may be omitted only when the
  subject is fully self-explanatory.
- Explain the root cause and the rationale for the fix: why this is a bug and why
  this change is needed.
- Do not enumerate changes file by file, and do not restate implementation steps
  that the diff already shows.
- Describe only the final state relative to the parent commit, not differences
  between intermediate versions of the same patch (e.g. "v2 fixes X").

### Trust the reader

Assume the reader is a competent developer familiar with the project; do not
explain what they already know:

- How to build the project — that belongs in documentation, not in a commit
  message.
- Obvious statements of usage.
- "Build succeeded" or "all tests green" — the commit's existence already implies
  it passed.

Mention these only when they are genuinely non-obvious:

- New test commands or tools that do not yet exist in the project.
- Non-standard configuration required to reproduce the result.
- Unusual constraints that affect how the data should be interpreted.
