# Task Workflow

Two workflow branches. Pick the one that matches the task.

## Design Workflow (ideas, ADRs, specs)

Use for: idea docs, ADRs, specs, research, brainstorming, conventions, roadmap updates.

**D1. Pick Work** — check review docs, ADR index, or start new exploration.

**D2. Research** — read related docs and cross-references. Web search for prior art.

**D3. Plan** — for substantial work, align on scope. Use `/grill-me` to stress-test. Skip for small edits.

**D4. Write** — follow conventions for the doc type (see `.claude/rules/docs.md`).

**D5. Checks** — `mise run fmt`, `mise run lint`, `/de-slopify` on prose.

**D6. Review** — `/pr-review-toolkit:review-pr`, fix findings.

**D7. Update Indexes** — update README tables, cross-references.

**D8. Commit Gate** — one logical change per commit. Wait for explicit "commit".

## Implementation Workflow (code)

Use for: new features, bug fixes, refactors, tests, performance work, CI/CD.

**I1. Review** — `trekker ready`, pick task, set `in_progress`.

**I2. Research** — read the spec, ADRs, idea docs, existing code. Check performance/security docs if relevant.

**I3. Plan** — for non-trivial work, align on approach. Use `/tracer-bullets`, `/grill-me`, or `/improve-codebase-architecture`.

**I4. TDD** — `/tdd` for core functions. Red-green-refactor with adversarial tests and clippy inline. The bigger the task, the more TDD matters — decompose into vertical slices (e.g., "binds socket" → "accepts connection" → "completes handshake"), not horizontal layers.

**I5. Implement** — code until tests pass and clippy is clean.

**I6. Update Tracking** — trekker comment + status. Update docs if implementation diverged from spec.

**I7. Checks** — `mise run check` then `mise run test`.

**I8. Polish** — `/de-slopify` on code. `/performance-optimizer` if touching hot paths.

**I9. Review** — `/pr-review-toolkit:review-pr`, fix findings.

**I10. Re-check** — re-run I7 if anything changed during polish or review.

**I11. Commit Gate** — one logical change per commit. Wait for explicit "commit".
