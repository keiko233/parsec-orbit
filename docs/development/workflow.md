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

## Worktrees and resource reuse

Feature/refactoring work runs in isolated git worktrees by default. Working in the
current checkout is an option when the user chooses it after a cost assessment.
Before implementation:

- Consider the task's scope, expected duration, concurrent work, and existing
  uncommitted changes in the current checkout.
- Weigh the isolation benefits against dependency installation, an independent
  Cargo `target/` (about 19 GB in this checkout), disk usage, and any preparation a
  fresh worktree needs. Reuse a suitable existing worktree when available.
- Summarize the relevant costs and benefits, state a recommendation, then ask the
  user whether to work in a worktree or the current checkout. Wait for their choice
  before implementation; if they have already specified a choice for the task,
  follow it without asking again.
- Keep isolated worktrees as the recommended default for feature/refactoring work.
  Small, isolated edits may be cheaper in the current checkout; broad changes or
  concurrent work strengthen the case for a worktree. The assessment adds a
  user-selectable alternative, not a replacement for the isolation and resource
  reuse policy.

When the user chooses a worktree, choose a path outside the repo tree using the
filesystem policy below; respect any location the developer has explicitly
selected. Worktrees share the main `.git`. Each worktree owns its
**branch-dependent** outputs; its Cargo `target/` may start as an independent
copy-on-write (CoW) clone of an existing target.

### Filesystem and location policy

Inspect the filesystem containing the project and the volume containing the source
target before creating a worktree. Check the proposed destination's existing parent
as well: a matching filesystem type on two different volumes does not establish
clone support. Prefer an existing parent outside the repo tree on the same
clone-capable volume. On Windows, if the project is on ReFS, create the worktree on
that same ReFS volume rather than defaulting to a worktree directory on NTFS. If
the developer explicitly chooses a different location, explain any loss of cloning
and use an independent target there.

Use existing filesystem capabilities only. Do not reformat or repartition disks,
create or mount VHD/VHDX images, add filesystems or mount points, create snapshot
datasets/subvolumes, move existing checkouts or caches, or change global
environment variables or tool configuration to enable reuse. Creating a new
worktree and its own `target/` under an existing parent is the intended operation;
reorganizing the developer's existing directories is not.

Before preparing the target, tell the user which filesystem and source/destination
volume were detected, which native reuse method will be used, and whether that
method was previously used or verified for this project and these paths. Prefer an
existing applicable method rather than introducing a different one. A previous
successful clone or probe may establish support while the relevant volumes, OS and
copy tool remain applicable; recheck when those conditions change. This is a status
explanation, not a separate approval step after the worktree choice is already
authorized. If native cloning is unsupported or cannot be verified with existing
tools, briefly explain that target reuse will be skipped and build independently.
Do not pursue VHDs, filesystem changes or other storage workarounds.

| Existing environment                     | Target reuse in a new worktree                                 | Conditions                                                                                                  |
| ---------------------------------------- | -------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| Windows ReFS                             | CoW (block) clone into the worktree's own `target/`            | Source and destination on the same ReFS volume; verify that the OS and copy operation support block cloning |
| Windows NTFS                             | Independent target, built as needed                            | No native ReFS-style block cloning; do not substitute hardlinks or provision another volume                 |
| Unsupported or cross-volume destinations | Independent target, built as needed                            | No verified native CoW clone for the selected paths                                                         |

Parsec Orbit is Windows-only, so the macOS and Linux clone filesystems do not apply
here.

Use a small disposable file under the selected existing parents to verify clone
support when it is not already established. A fast copy or a successful command
alone does not prove shared blocks. A clone-only API/operation must succeed, or
filesystem extent information must confirm sharing; modifying the destination must
leave the source unchanged. Do not install system tools or change system settings
just to run the probe. If cloning cannot be established, report the limitation and
build an independent target instead of silently copying a large target tree.

The native operation is
[ReFS block cloning](https://learn.microsoft.com/en-us/windows-server/storage/refs/block-cloning)
(`FSCTL_DUPLICATE_EXTENTS_TO_FILE`).
[Robocopy](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/robocopy/)
can fall back to an ordinary copy, so its successful exit status alone is
insufficient to establish CoW reuse. Do not use a full-copy fallback as the default
for target seeding.

### Reuse policy

| Path (repo-relative)                                       | Approx size | Policy                                                                  | Reason                                                                                                                                     |
| ---------------------------------------------------------- | ----------- | ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `target/`                                                  | ~19 GB      | **Independent CoW clone when supported; otherwise build independently** | Clones share unchanged data blocks while keeping files and Cargo locks independent; never share a writable target directory                 |
| `node_modules/`                                            | ~79 MB      | **Independent `pnpm install`**                                          | The pnpm global store already hardlink-dedupes; sharing risks concurrent lock conflicts                                                     |
| `dist/`                                                    | build output | **Independent - never symlink**                                        | `scripts/package.ps1` wipes and refills it; one worktree's package run would overwrite another's staging                                   |
| `javascript-sdk/dist/`, `javascript-sdk/dist-test/`, `examples/*-ts/dist/` | build output | **Independent - never symlink**                          | Branch-dependent TypeScript build output                                                                                                   |

No path in this repository is a symlink candidate: there are no downloaded
sidecars or static resources to share, and every other gitignored path is
branch-dependent build output. The Cargo `target/` is the one artifact worth
seeding.

This repository has no gitignored build prerequisites of the kind a Tauri build
needs: there is no `build.rs` anywhere, and nothing validates a generated `dist/`
at compile time. A fresh worktree needs `target/` (seeded or built) and a
`pnpm install`.

### Reuse Cargo target through CoW

- Seed a new, absent `target/` from a suitable existing target on the same
  clone-capable volume. Do not overwrite an existing worktree's target. A clone
  creates independent files; it must not hardlink destination files back to the
  source, use a symlink/junction to the source target, or point multiple worktrees
  at one `CARGO_TARGET_DIR`. Hardlinks between files entirely inside the new target
  may be preserved.
- Clone only while the source target is not being written by Cargo, rustc,
  rust-analyzer, or other build tools. A recursive file clone is not an atomic
  directory snapshot. Do not stop the developer's running processes to obtain a
  seed; if the source cannot be kept idle, skip seeding and build independently.
- Prefer a source built with the same resolved Rust toolchain version, target
  triple, lockfile, profile, features, and rustflags. Reuse an already suitable
  target; do not clean or rebuild the main checkout just to prepare a seed.
  Preserve timestamps, permissions and link semantics, and ensure copied links do
  not redirect writes to the source target.
- Treat the clone as a build cache, not a completed build for the new worktree.
  Cargo must validate it through the relevant build/check/test command before use.
  Absolute paths, build-script outputs and source timestamps can cause rebuilding
  even at the same commit. Do not rewrite Cargo fingerprints or cached paths to
  force reuse.
- Cloning saves initial data duplication, not every future build's space: rewritten
  or newly generated artifacts need storage. Directory size sums can double-count
  hardlinks and CoW data; assess savings through filesystem allocation information
  or measured volume-space changes.
- Keep the `node_modules/` installation and the `dist/` staging independent. Do not
  extend target cloning into a shared output directory.

For changes to worktree preparation tooling, verify clone support and write
isolation with disposable files, then verify Cargo behavior after seeding:
build/check/test as appropriate, rebuild after a source change, build two worktrees
concurrently, and remove one without modifying the seed or the other worktree. Test
the unsupported-filesystem path as well: it must leave a usable independent
worktree without provisioning storage or performing an unannounced full target
copy.

### Create a worktree

Commands shown for Windows / PowerShell. Choose `<worktree-path>` according to the
filesystem policy above and `<type>/<name>` according to the task's branch
convention. After creating the worktree, seed its `target/` according to the CoW
policy when supported, before launching build tools; otherwise leave it to Cargo
to create independently.

```powershell
$main = git rev-parse --show-toplevel                 # capture main checkout root
git worktree add <worktree-path> -b <type>/<name>
cd <worktree-path>

# Seed target/ through verified CoW cloning when supported.
# Otherwise let Cargo create it; never link it back to the main target.

pnpm install
```

The main checkout lives on `F:` (ReFS). The Codex-managed worktree root under
`%USERPROFILE%\.codex\worktrees\` is on `C:` (NTFS), so a worktree created there
cannot reuse the target through a block clone. When target reuse matters, create
the worktree under an existing parent on `F:` with `git worktree add` instead of
relying on the default location, or accept an independent target and say so.

### Remove a worktree

`git worktree remove` on Windows can fail with `Filename too long` because
per-worktree `node_modules/` and `target/` hold paths over MAX_PATH. Force-delete
with the extended-length prefix, then reconcile git:

```powershell
Remove-Item -LiteralPath "\\?\<absolute-worktree-path>" -Recurse -Force
git worktree prune
git worktree list
```

Removal reclaims only the worktree's own files - it never touches the main
checkout or another worktree's target. For CoW target clones, shared data blocks
remain allocated while another file still references them; deleting a clone
releases only allocations no longer referenced.

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

- How to build the project - that belongs in documentation, not in a commit
  message.
- Obvious statements of usage.
- "Build succeeded" or "all tests green" - the commit's existence already implies
  it passed.

Mention these only when they are genuinely non-obvious:

- New test commands or tools that do not yet exist in the project.
- Non-standard configuration required to reproduce the result.
- Unusual constraints that affect how the data should be interpreted.
