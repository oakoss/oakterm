# OakTerm

GPU-accelerated, extensible terminal emulator with a plugin-driven process dashboard and context-aware shell.

**Status: Phase 1** — Phase 0 terminal foundation (renderer, VT parser, screen buffer, daemon/client split) complete; the multiplexer is the next roadmap focus.

## Pipeline

```text
docs/ideas/ → docs/adrs/ → docs/specs/ → implementation
explore    decide       formalize     build (trekker)
               ↑
        docs/reviews/
        (audits that surface corrections and ADR candidates)
```

- **Ideas** explore possibilities. **Reviews** audit them and surface corrections.
- **ADRs** resolve questions. An accepted ADR moves the idea doc to `decided`.
- **Specs** formalize decisions into implementation contracts.
- **Implementation** builds what specs define. Trekker tracks work.

For doc conventions, see `.claude/rules/docs.md`. For workflows, see `.claude/rules/workflow.md`. For Rust patterns, see `.claude/rules/rust.md`.

## Tooling

Managed by [mise](https://mise.jdx.dev/). Run `mise install` to get all tools.

| Command          | Purpose                        |
| ---------------- | ------------------------------ |
| `mise run check` | All checks (fmt + lint + Rust) |
| `mise run test`  | All tests                      |
| `mise run bench` | All benchmarks                 |
| `mise run fmt`   | Format non-Rust files          |
| `mise run lint`  | Lint markdown                  |
| `cargo clippy`   | Rust linting                   |

## Architecture

- **Phases**: 0 (renderer) → 1 (multiplexer) → 2 (plugins) → 3 (shell intelligence) → 4 (networking) → 5 (polish)
- **Tech**: Rust, wgpu, Wasmtime (plugins), Lua (config), platform-native text shaping
- **Principles**: performance non-negotiable, extensible by design, secure by default, abstracted at every seam, accessible from day one

## Task Tracking

Trekker tracks implementation work. Design work is tracked by docs themselves.

- **Epics** = committed work streams (5-15 tasks, 2-6 weeks)
- **Tags** = type (`feature`, `chore`, `spike`) + area (`renderer`, `core`, `docs`)
- **Priority** on tasks, not epics
- **Standalone tasks** (priority 3) for shaped but uncommitted features — promote to epic when committed
- **Icebox** = priority 4-5 with no epic; archive if nobody mentions in 3 months
- Don't create epics for uncommitted work or catch-all epics like "Features"

## Rules

- **Never commit proactively** — wait for the user's go-ahead
- **Never push** unless explicitly asked
- **Trekker is for implementation** — `trekker ready` before starting, summary comment before completing
- **Read before writing** — understand existing docs/code before modifying
- **Conventions are law** — follow [docs/ideas/30-conventions.md](docs/ideas/30-conventions.md) for idea docs, README templates for ADRs/specs
- **No empty docs** — every idea doc needs a problem statement, every ADR needs options + rationale, every spec needs formal definitions
- **Scope is explicit** — include "What This Is Not" to prevent feature creep
- **Implementation references specs** — no spec = not ready to implement
- **Decisions go in ADRs** — don't resolve contradictions inline
- **One logical change per commit** — split independent concerns

## Commit Style

Conventional commits: `type(scope): short description`

Types: `docs`, `chore`, `feat`, `fix`, `refactor`, `test`, `perf`, `ci`

Scopes: `ideas`, `review`, `adr`, `spec`, `docs`, `readme`, `config`, `trekker`, `core`, `setup`, `renderer`, `ci`, `repo`

Task references go in the commit footer as a bare `TREK-XX`, not in the subject line:

```text
feat(core): implement dark/light mode detection

Detect OS appearance via winit ThemeChanged and expose to Lua config.

TREK-50
```

# Comment policy

Comments are useful when they add value. Keep them clean and minimal.

A good comment:

- Is accurate (matches the code; remove if stale)
- Earns its place (explains WHY or non-obvious context, not WHAT)
- Is concise (one or two lines unless documenting a complex invariant)

Avoid:

- Restating what the code does
- Section markers like `// ===== HELPERS =====`
- Hedge words, apologies, "obviously", "basically", "just"
- "Note:" / "Important:" prefixes when surrounding text already conveys importance
- TODOs without ticket references
- Cross-references that belong in the PR description ("added for X", "used by Y")
- Multi-line comments on trivial code
- AI-flavored phrasings ("Here we...", "Let's...", "This...")

When in doubt: keep the comment, but make it tighter.

# Fix-vs-defer policy

When addressing review findings (from the review-cycle skill, PR comments, or any other reviewer):

Default to fixing inline. Defer to a follow-up only if:

- The fix is substantially more work than writing the follow-up itself
- The fix requires architectural changes spanning files outside this PR scope
- The fix requires a new dependency or schema migration not in this PR
- The fix would invalidate unrelated tests

If you can describe the fix in one sentence, just do the fix.

When deferring, briefly state which criterion above applies.
