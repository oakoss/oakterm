# Phantom Terminal

GPU-accelerated, extensible terminal emulator with a plugin-driven process dashboard and context-aware shell.

**Status: Idea Phase** — collecting and refining design docs before implementation.

## Project Structure

```text
ideas/          # Design documents (NN-topic.md, numbered for reading order)
docs/           # Implementation docs (empty until code begins)
.trekker/       # Task tracking
```

## Conventions

- **Idea docs**: Follow structure in [ideas/30-conventions.md](ideas/30-conventions.md) — YAML frontmatter (title, status, category, description, tags), sections for Problem, Design, Configuration, Plugin API, What This Is Not
- **Frontmatter status**: draft → reviewing → decided → implementing → reference
- **Frontmatter category**: core, plugin, community-plugin, cross-cutting, research
- **Config naming**: kebab-case (flat), snake_case (Lua), 1:1 mapping
- **Plugin naming**: lowercase kebab-case registry name, title case display name
- **Theme naming**: lowercase kebab-case file name, title case display name
- **Cross-references**: relative paths — `See [Memory Management](15-memory-management.md)`
- **Keybinds**: borrow from OS/VS Code/tmux/vim/browser conventions, never invent new muscle memory
- **Markdown**: fenced code blocks always have a language tag

## Architecture Context

- **Roadmap phases**: 0 (renderer) → 1 (multiplexer) → 2 (plugins) → 3 (shell intelligence) → 4 (networking) → 5 (polish)
- **Tech**: Rust, wgpu, Wasmtime (plugins), Lua (config), platform-native text shaping
- **Core principles**: performance non-negotiable, extensible by design, secure by default, abstracted at every seam, accessible from day one, everything is a pane, the plugin is the product, debugging is built in

## Task Tracking

Use **Trekker** for all task tracking. `trekker ready` before starting work, summary comment before completing.

## Task Workflow

Two workflow branches. Pick the one that matches the task. Both start and end the same way — trekker to begin, commit gate to finish.

---

### Design Workflow (ideas, research, docs)

Use for: new idea docs, doc revisions, research, brainstorming, conventions, roadmap updates.

#### D1. Review

`trekker ready` — pick a task, check context/deps/blockers. Set to `in_progress`.

#### D2. Research

- Read related idea docs in `ideas/` and their cross-references.
- Read [ideas/30-conventions.md](ideas/30-conventions.md) for structure, naming, and frontmatter requirements.
- For research tasks: read [ideas/10-pain-points.md](ideas/10-pain-points.md), [ideas/11-inspiration.md](ideas/11-inspiration.md), [ideas/16-wishlist-features.md](ideas/16-wishlist-features.md) for prior art.
- Web search for prior art, competing implementations, community discussions as needed.

#### D3. Plan

For substantial design work (new idea docs, major revisions), align on scope and approach first. Use `/grill-me` to stress-test the design. Skip for small edits, typo fixes, or brainstorm additions.

#### D4. Write

- Follow the doc structure in [ideas/30-conventions.md](ideas/30-conventions.md).
- YAML frontmatter: title, status, category, description, tags — all required.
- Clear problem statement ("Why does this exist?").
- Concrete design with examples, ASCII mockups, config samples.
- Explicit scope boundaries ("What This Is Not").
- Cross-reference related docs with relative paths.
- For brainstorm additions: add to [ideas/31-brainstorm.md](ideas/31-brainstorm.md) under the right section.

#### D5. Polish

- `/de-slopify` on all prose.
- Verify frontmatter is complete and status is correct.
- Check all cross-references resolve to real docs.
- Ensure fenced code blocks have language tags.
- Check for broken markdown (unclosed links, bad tables).

#### D6. Review

- `/pr-review-toolkit:review-pr` — fix findings.
- Re-run polish if anything changed.

#### D7. Update Tracking

- Trekker comment summarizing what was done.
- Update README idea docs table if a new doc was added.
- Update cross-references in other docs if the new work affects them.
- Delete any plan files.

#### D8. Commit Gate

- One logical change per commit. Split independent doc changes.
- Present summary and **wait for explicit "commit"**.

---

### Implementation Workflow (code)

Use for: new features, bug fixes, refactors, tests, performance work, CI/CD.

#### I1. Review

`trekker ready` — pick a task, check context/deps/blockers. Set to `in_progress`.

#### I2. Research

- Read the relevant design doc(s) in `ideas/` — the spec for what you're building.
- Read [ideas/01-architecture.md](ideas/01-architecture.md) for layer boundaries and abstraction seams.
- Read existing source code in the area you're changing.
- Check [ideas/12-performance.md](ideas/12-performance.md) if the work touches a hot path.
- Check [ideas/21-security.md](ideas/21-security.md) if the work touches input handling, plugins, or IPC.

#### I3. Plan

For non-trivial work, align on approach first. Use `/tracer-bullets` for multi-layer features, `/grill-me` to stress-test the design, or `/improve-codebase-architecture` for structural decisions. Skip for small fixes.

#### I4. TDD

`/tdd` for core functions and business logic. Write failing tests first, then implement. Skip for config changes, docs-only, or pure refactors with existing test coverage.

#### I5. Implement

Write code until tests pass. Follow the architecture layer boundaries. Keep abstractions at seams (traits/interfaces, not concrete types).

#### I6. Update Tracking

- Trekker comment + status update.
- Update design docs if the implementation diverged from the spec.
- Delete plan files.

#### I7. Checks

Run in order (as applicable to what exists in the build system):

1. `cargo fmt` (format)
2. `cargo clippy` (lint)
3. `cargo check` (typecheck)
4. `cargo test` (unit + integration tests)
5. E2E tests (when they exist)

#### I8. Polish

- `/de-slopify` on code.
- `/performance-optimizer` if touching hot paths.

#### I9. Review

- `/pr-review-toolkit:review-pr` — fix findings.

#### I10. Re-check

Re-run step I7 if anything changed during polish or review.

#### I11. Commit Gate

- One logical change per commit. Split independent concerns.
- Present summary and **wait for explicit "commit"**.

## Rules

- **Never commit proactively** — always wait for the user's go-ahead.
- **Never push** unless explicitly asked.
- **Trekker is the task system** — `trekker ready` before starting, summary comment before completing.
- **Read before writing** — understand existing docs/code before modifying.
- **Conventions are law** — follow [ideas/30-conventions.md](ideas/30-conventions.md) for all design docs.
- **No empty docs** — every idea doc must have a clear problem statement and concrete design.
- **Scope is explicit** — include "What This Is Not" to prevent feature creep.

## Commit Style

Conventional commits. Allowed types:

- `docs` — idea docs, README, design writing
- `chore` — tooling, config, trekker setup
- `feat` — new functionality (when code exists)
- `fix` — bug fixes (when code exists)
- `refactor` — restructuring without behavior change
- `test` — test additions/changes
- `perf` — performance improvements
- `ci` — CI/CD changes

Format: `type(scope): short description`

Scopes (current phase): `ideas`, `docs`, `readme`, `config`, `trekker`
