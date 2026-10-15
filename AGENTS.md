# AGENTS.md

Parsec Orbit is an unofficial, local-only plugin loader for the Parsec client: a
`bcrypt.dll` proxy (Windows only), the plugin ABI, the Rust and TypeScript SDKs,
the official plugins and the tools around them. Keep the loader tiny and boring;
plugins do not pay for what they do not use.

## Mandatory Reading: Development Standards

Before starting any repository work, agents MUST read
[docs/development/README.md](docs/development/README.md) and the standards guides
it routes to: [workflow](docs/development/workflow.md),
[testing and review](docs/development/testing.md),
[Rust code style](docs/development/rust.md), and the existing
[architecture](docs/design/architecture.md), [specifications](docs/spec/) and
[guides](docs/guides/).

Agents MUST follow these development standards together with the instructions
below. Reading this file alone is insufficient. The guides are mandatory project
requirements, not optional background or suggestions. If a guide is unavailable or
the current requirements conflict, report the issue and resolve it before
proceeding with affected work.

Behavioral guidelines reduce common LLM coding mistakes. Merge with
project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks,
use judgment.

## 0. Synchronization Policy

`CLAUDE.md` imports this file, so both tools follow the same instructions. The
guides under `docs/development/` are the single source of the detailed rules; this
file routes to them and restates only the rules that are violated most often.

- When a shared rule changes, update its guide in the same change, and update this
  file only if it restates that rule.
- Do not copy guide sections into this file; link to them.
- Do not create a Claude-only or agent-only exception unless the tool truly
  requires it.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:

- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

For small, obvious tasks, do this briefly. For architecture, migration, or
cross-module work, be explicit.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes,
simplify.

This rule does not override the architecture rules. Do not use process-global
mutable state merely because it is fewer lines.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:

- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:

- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:

- "Add validation" -> "Write tests for invalid inputs, then make them pass"
- "Fix the bug" -> "Write a test that reproduces it, then make it pass"
- "Refactor X" -> "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:

```text
1. [Step] -> verify: [check]
2. [Step] -> verify: [check]
3. [Step] -> verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work")
require constant clarification.

## 5. Architecture

The crate graph, the loader's size and concurrency policy, and the failure model
live in [Architecture](docs/design/architecture.md); read it before changing the
loader, `parsec-orbit-core` or the plugin boundary. The decisions are recorded in
[docs/design/decisions/](docs/design/decisions/).

Non-negotiable rules, restated because they are the most common violations:

- The loader is a thin `bcrypt.dll` proxy. `DllMain` does the absolute minimum and
  never runs plugin work inside it; everything else runs on a worker thread.
- `parsec-orbit-core` stays Win32-free and light ([ADR 0004](docs/design/decisions/0004-loader-stays-light.md)):
  no async runtime, no `serde`, no `tracing` unless it is measured to fit. Its
  decisions are written against small traits (`Platform`, `Logger`,
  `console::Console`) so they are unit-tested without mapping a DLL.
- Plugins do not pay for what they do not use. The SDK is one `no_std` package
  without features, used by `std` and `no_std` plugins alike; never add a `std`
  feature to it (Cargo unifies features and one `std` plugin would break every
  `no_std` plugin in the workspace). The `std`-only test double is the
  dev-dependency `parsec-orbit-plugin-sdk-testing`
  ([ADR 0002](docs/design/decisions/0002-sdk-is-no-std-by-default.md),
  [restructure design](docs/design/restructure.md#67-the-sdk-as-one-package-adr-0002-re-evaluated)).
- Construct dependencies explicitly and pass them in; do not add process-global
  mutable state. The loader owns one worker thread and one log file, and a plugin
  that needs more spawns its own thread and returns from `init`.
- Watch the size budget ([size policy](docs/design/architecture.md#size-policy),
  [size report](docs/design/size-report.md)); a dependency that grows the loader
  or a plugin needs a measured reason.

## 6. Testing

Read [Testing and review](docs/development/testing.md) for test design and the
checks to run. Test the loading logic with plain values and the existing doubles
(`loader-host`, the e2e sandbox, the fake PE image, `MockHost`); never touch an
installed Parsec client or a real user configuration.

## 7. Code Style

Read [Rust code style](docs/development/rust.md) for formatting, lint, types and
errors. The crate directory equals the package name. The SDK guides under
[docs/guides/](docs/guides/) are written together with the SDKs they describe.

## 8. Repository Scripts and Tools

Product automation (install, upgrade, packaging, registry generation and
signing) is Rust, in `tools/parsec-orbit-installer` and
`tools/parsec-orbit-release`; logic and security-sensitive code never lives in
shell, PowerShell or Python. `scripts/` holds thin PowerShell entry points
(`kebab-case.ps1`, usage header, `[CmdletBinding()] param(...)` first,
`$ErrorActionPreference = 'Stop'`, `-LiteralPath` for file operations) that
build and delegate to those tools; CI calls the same scripts a developer runs
(`scripts/verify.ps1`, `scripts/package.ps1`) and the pnpm commands stay thin
delegates. The bootstrap scripts shipped to users (`get-orbit.ps1`,
`get-orbit.sh`) only download, check `SHA256SUMS.txt` and run `orbit`.
One-off research scripts live in `research/` and are not product automation.
Until the tools land, the existing scripts stay; do not add logic to them. See
[Repository scripts](docs/development/workflow.md#repository-scripts) and
[File formats](docs/development/formats.md).

## 9. Documentation

- Configuration and metadata files follow [File formats](docs/development/formats.md):
  TOML for what people edit, JSON for what programs exchange, one source per fact.
- `docs/spec/` is normative: a change in behaviour changes the spec in the same
  commit.
- `docs/design/` explains why; `docs/guides/` are task-oriented how-tos written
  with the SDKs they describe.
- `docs/design/decisions/` holds decision records: one file per decision,
  numbered, never rewritten after acceptance (supersede with a new record), in
  the order Context, Decision, Consequences.

## 10. Worktrees

Follow [Worktrees and resource reuse](docs/development/workflow.md#worktrees-and-resource-reuse).
Feature and refactoring work runs in an isolated worktree by default; assess the
cost, recommend a location, and ask the user unless they have already chosen.
Reuse the Cargo `target/` through a block clone when the worktree is on the same
clone-capable volume as the main checkout, and never share a writable target
directory.

## 11. Git Commits

Follow [Git commits](docs/development/workflow.md#git-commits).

- Stage explicit paths only; never `git add .`, `-A`, `--all`, or `*`.
- One commit does one thing and builds. No fix-up commits: fold a correction into
  the unpushed commit it completes. Rewriting pushed history requires explicit
  consent.
- The subject is imperative, at most 72 characters, without a trailing period; a
  non-trivial change explains why in the body.

## 12. Final Review

Before finishing, walk through
[Review before submitting](docs/development/testing.md#review-before-submitting).
In particular: every changed line traces to the request, success criteria were
verified, and no process-global mutable state or new loader dependency entered
without a measured reason.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer
rewrites due to overcomplication, clarifying questions come before implementation
rather than after mistakes, and the loader and the plugins stay small.
