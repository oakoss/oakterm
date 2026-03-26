# OakTerm

GPU-accelerated, extensible terminal emulator with a plugin-driven process dashboard and context-aware shell.

**Status: Idea Phase** — collecting and refining design docs before implementation.

## Project Structure

```text
docs/ideas/     # Exploration — brainstorming, research, design sketches
docs/reviews/   # Audits — point-in-time reviews that surface work (YYYY-MM-DD-title.md)
docs/adrs/      # Decisions — resolve open questions from ideas (NNNN-title.md)
docs/specs/     # Contracts — formal definitions that code must satisfy (NNNN-title.md)
.trekker/       # Task tracking — implementation work that references specs
```

## Pipeline

```text
docs/ideas/ → docs/adrs/ → docs/specs/ → implementation
explore    decide       formalize     build (trekker)
               ↑
        docs/reviews/
        (audits that surface corrections and ADR candidates)
```

- **Ideas** explore possibilities. Status: `draft → reviewing`.
- **Reviews** audit ideas and surface corrections, contradictions, and missing specs.
- **ADRs** resolve questions that ideas or reviews surfaced. An accepted ADR moves the idea doc status to `decided`.
- **Specs** formalize decided designs into implementation contracts.
- **Implementation** builds what specs define. Trekker tracks implementation work.

## Conventions

### Idea Docs (`docs/ideas/`)

- Follow structure in [docs/ideas/30-conventions.md](docs/ideas/30-conventions.md) — YAML frontmatter, sections for Problem, Design, Configuration, Plugin API, What This Is Not
- **Frontmatter status**: `draft → reviewing → decided → implementing → reference`
- **Frontmatter category**: core, plugin, community-plugin, cross-cutting, research
- An accepted ADR moves the idea doc status from `reviewing` to `decided`

### ADRs (`docs/adrs/`)

- Format: `NNNN-short-title.md`, numbered sequentially, never renumber
- **Status**: `proposed → accepted → [superseded | deprecated]`
- One ADR per decision. Link to the idea docs that surfaced the question.
- See [docs/adrs/README.md](docs/adrs/README.md) for template

### Specs (`docs/specs/`)

- Format: `NNNN-short-title.md`, numbered sequentially
- **Status**: `draft → review → accepted → implementing → complete`
- One spec per bounded concern (API surface, wire protocol, data format)
- Trekker tasks reference specs. Implementation builds what specs define.
- See [docs/specs/README.md](docs/specs/README.md) for template

### Reviews (`docs/reviews/`)

- Format: `YYYY-MM-DD-HHMMSS-short-title.md`, timestamped for ordering
- Point-in-time snapshots — findings may become stale
- Surface corrections (fix directly), contradictions (write ADRs), and missing specs
- See [docs/reviews/README.md](docs/reviews/README.md) for template

### General

- **Config naming**: kebab-case (flat), snake_case (Lua), 1:1 mapping
- **Plugin naming**: lowercase kebab-case registry name, title case display name
- **Theme naming**: lowercase kebab-case file name, title case display name
- **Cross-references**: relative paths — `See [Memory Management](15-memory-management.md)`
- **Keybinds**: borrow from OS/VS Code/tmux/vim/browser conventions, never invent new muscle memory
- **Markdown**: fenced code blocks always have a language tag

## Tooling

Managed by [mise](https://mise.jdx.dev/). Run `mise install` to get all tools.

| Tool                  | Purpose                                            | Command                                     |
| --------------------- | -------------------------------------------------- | ------------------------------------------- |
| **prettier**          | Format non-Rust files (markdown, TOML, JSON, YAML) | `mise run fmt` / `mise run fmt:check`       |
| **markdownlint-cli2** | Markdown linting                                   | `mise run lint:md` / `mise run lint:md:fix` |
| **lefthook**          | Git hooks (pre-commit, commit-msg)                 | `lefthook install` (once after clone)       |
| **cocogitto**         | Conventional commit validation                     | `cog verify`                                |
| **cargo fmt**         | Rust formatting (when code exists)                 | `cargo fmt`                                 |
| **cargo clippy**      | Rust linting (when code exists)                    | `cargo clippy`                              |

## Architecture Context

- **Roadmap phases**: 0 (renderer) → 1 (multiplexer) → 2 (plugins) → 3 (shell intelligence) → 4 (networking) → 5 (polish)
- **Tech**: Rust, wgpu, Wasmtime (plugins), Lua (config), platform-native text shaping
- **Core principles**: performance non-negotiable, extensible by design, secure by default, abstracted at every seam, accessible from day one, everything is a pane, the plugin is the product, debugging is built in

## Task Tracking

**Trekker** tracks implementation work. Design-phase work is tracked by the docs themselves (review findings, ADR index, spec index).

### Organization

- **Epics** = committed work streams scoped by user-visible outcome (5-15 tasks, 2-6 weeks). Must answer "what is different when this is done?"
- **Tags** = cross-cutting labels for type (`feature`, `chore`, `spike`) + area (`renderer`, `multiplexer`, `plugins`, `config`, `a11y`, `security`, `docs`, `ideas`)
- **Priority** goes on tasks, not epics — distinguishes must-ship from nice-to-have within an epic
- **Standalone tasks** (priority 3) for shaped but uncommitted features — promote to epic when committed
- **Icebox** = priority 4-5 tasks with no epic; archive if nobody mentions them in 3 months
- Don't create epics for uncommitted work
- Don't use catch-all epics like "Features" or "Improvements"

## Task Workflow

Two workflow branches. Pick the one that matches the task.

---

### Design Workflow (ideas, ADRs, specs)

Use for: idea docs, ADRs, specs, research, brainstorming, conventions, roadmap updates.

#### D1. Pick Work

Check review docs for open findings, ADR index for unresolved decisions, or start new exploration.

#### D2. Research

- **Ideas**: Read related idea docs in `docs/ideas/` and their cross-references. Read [docs/ideas/30-conventions.md](docs/ideas/30-conventions.md). Web search for prior art.
- **ADRs**: Read the idea docs that surfaced the question. Read any existing ADRs in the same area. Research how competitors solved it.
- **Specs**: Read the accepted ADR(s) that led here. Read the idea docs for design context. Read existing specs for API consistency.

#### D3. Plan

For substantial design work, align on scope and approach first. Use `/grill-me` to stress-test the design. Skip for small edits, typo fixes, or brainstorm additions.

#### D4. Write

**Idea docs:**

- Follow [docs/ideas/30-conventions.md](docs/ideas/30-conventions.md). YAML frontmatter required.
- Clear problem statement, concrete design, explicit scope boundaries ("What This Is Not").
- Cross-reference related docs with relative paths.

**ADRs:**

- Follow [docs/adrs/README.md](docs/adrs/README.md) template. One decision per ADR.
- List options considered with pros/cons. State the decision and rationale.
- Link to the idea docs that surfaced the question.
- Update the idea doc's frontmatter status to `decided` when the ADR is accepted.

**Specs:**

- Follow [docs/specs/README.md](docs/specs/README.md) template. One bounded concern per spec.
- Formal definitions: function signatures, wire formats, type definitions, error types.
- State behavior for normal and error cases. Call out edge cases.
- Include constraints (performance budgets, memory limits, security).
- Link to the ADR(s) that led here.

#### D5. Checks

- `mise run fmt` — format all files.
- `mise run lint` — lint markdown.
- `/de-slopify` on all prose.
- Verify frontmatter is complete and status is correct.
- Check all cross-references resolve to real docs.
- For specs: verify all types are defined, no hand-waving ("TBD", "details later").

#### D6. Review

- `/pr-review-toolkit:review-pr` — fix findings.
- Re-run polish if anything changed.

#### D7. Update Indexes

- Update README tables if a new doc was added.
- Update ADR/spec/review README index.
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

- Read the **spec** this task references — the contract you're implementing.
- Read the ADR(s) behind the spec for decision rationale.
- Read the idea doc(s) for broader design context.
- Read existing source code in the area you're changing.
- Check [docs/ideas/12-performance.md](docs/ideas/12-performance.md) if the work touches a hot path.
- Check [docs/ideas/21-security.md](docs/ideas/21-security.md) if the work touches input handling, plugins, or IPC.

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
- **Trekker is for implementation** — `trekker ready` before starting implementation work, summary comment before completing. Design work is tracked by docs.
- **Read before writing** — understand existing docs/code before modifying.
- **Conventions are law** — follow [docs/ideas/30-conventions.md](docs/ideas/30-conventions.md) for idea docs, README templates for ADRs and specs.
- **No empty docs** — every idea doc needs a problem statement, every ADR needs options + rationale, every spec needs formal definitions.
- **Scope is explicit** — include "What This Is Not" to prevent feature creep.
- **Implementation references specs** — trekker tasks link to specs, not idea docs. No spec = not ready to implement.
- **Decisions go in ADRs** — don't resolve contradictions inline in idea docs. Write an ADR, then update the idea doc to reflect the decision.

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

Scopes: `ideas`, `review`, `adr`, `spec`, `docs`, `readme`, `config`, `trekker`
