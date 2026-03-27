---
name: spec-to-tasks
description: |
  Break an accepted spec into implementable Trekker tasks with dependency ordering, HITL/AFK classification, and spec coverage verification.

  Use when: an accepted spec is ready for implementation, you need to decompose work into tasks, or you want to verify every contract item has a task. Requires an accepted spec as input.
---

# Spec to Tasks

Decompose an accepted spec into independently implementable Trekker tasks. Each task is traceable to specific contract items in the spec. No contract item is left without a task.

Skip for: trivial specs that map to a single task, or when the spec is still in draft/review.

## Process

### 1. Read the Spec

Read the spec thoroughly. Identify every contract item:

- Each type definition
- Each function/message signature
- Each behavior (normal and error cases)
- Each constraint (performance, memory, security)
- Each edge case

Make a checklist of contract items. Every item must have at least one task responsible for implementing it.

### 2. Identify Foundation Tasks

Some work must exist before any vertical slice can be built. These are horizontal foundation tasks:

- Data structures and core types
- Trait definitions and abstractions
- Build system setup (new crates, dependencies, feature flags)
- Test infrastructure (test utilities, fixtures, mock implementations)

Foundation tasks are typically small, focused, and have no dependencies on each other. They block vertical slices.

### 3. Draft Vertical Slices

Break the remaining work into thin vertical slices. Each slice:

- Delivers a narrow but complete path through the contract
- Is independently testable and verifiable
- Can be merged without breaking existing functionality
- References specific contract items from the spec

Prefer many thin slices over few thick ones. A completed slice should be demoable or verifiable on its own.

For terminal emulator work, typical slices:

- **Protocol**: Define one message type end-to-end (struct, serialize, send, receive, deserialize, handle, test)
- **Renderer**: One rendering feature end-to-end (data, shader, pipeline, compositing, test)
- **Config**: One config surface end-to-end (type, parse, validate, apply, hot-reload, test)
- **Plugin API**: One plugin capability end-to-end (WIT definition, host binding, guest binding, example plugin, test)

### 4. Classify Tasks

Tag each task with an autonomy level:

| Level         | Label          | Criteria                                                                                        | Example                                                           |
| ------------- | -------------- | ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| Autonomous    | **AFK**        | Clear spec, single concern, existing patterns to follow, test-first viable                      | Implement `PingPdu` message type with roundtrip test              |
| Review needed | **Checkpoint** | Multi-file, touches API surface, security-adjacent, performance-critical                        | Implement screen buffer dirty-region tracking                     |
| Collaborative | **HITL**       | Ambiguous spec interpretation, cross-cutting concern, new public API, architectural uncertainty | Design the glyph atlas sharing strategy between daemon and client |

### 5. Set Dependencies

Order tasks so blockers come first:

- Foundation tasks have no dependencies (or depend only on other foundations)
- Vertical slices depend on their foundation tasks
- Later slices may depend on earlier slices if they extend the same surface
- HITL tasks should be scheduled early — they block on human input

Use Trekker's dependency system (`trekker dep add`) to encode the graph.

### 6. Verify Spec Coverage

Walk the contract item checklist from Step 1. For each item, identify which task(s) implement it. Flag any items without a task.

Present a coverage table:

```text
| Contract Item | Task(s) | Status |
| --- | --- | --- |
| PingPdu message type | T-001 | Covered |
| Screen buffer dirty flags | T-003 | Covered |
| Device-loss recovery | (none) | MISSING |
```

Every contract item must be covered. If an item is intentionally deferred, note it with a reason.

### 7. Create an Epic

If the spec decomposes into 5+ tasks, create a Trekker epic scoped by user-visible outcome (per CLAUDE.md: "What is different when this is done?"). The epic references the spec. Tasks belong to the epic.

### 8. Quiz the User

Present the full breakdown:

- Epic name and scope
- Task list with: title, AFK/Checkpoint/HITL classification, dependencies, contract items covered
- Coverage table showing any gaps
- Suggested implementation order (foundation → first slice → remaining slices)

Ask:

- Is the granularity right? (Too coarse? Too fine?)
- Are the dependencies correct?
- Are the HITL/AFK classifications right?
- Any tasks to merge or split?

Iterate until approved.

### 9. Create Tasks in Trekker

For each approved task, create a Trekker task with:

- **Title**: Clear, action-oriented (e.g., "Implement PingPdu message with roundtrip test")
- **Description**: What to build, referencing specific spec sections
- **Tags**: `feature`/`chore`/`spike` + area tag + AFK/Checkpoint/HITL
- **Priority**: 1 (must-ship) / 2 (important) / 3 (nice-to-have) within the epic
- **Dependencies**: Trekker dep links to blocking tasks

Set dependencies after all tasks are created (need real task IDs).

### 10. Update Spec Status

Update the spec's frontmatter status from `accepted` to `implementing`. Update the index table in `docs/specs/README.md` if it tracks status. If any doc files were modified during this process, run `mise run fmt` and `mise run lint` before committing.

### 11. Commit Gate

Present the task breakdown summary and wait for explicit "commit" before committing.

## Common Mistakes

| Mistake                                                                        | Fix                                                                                         |
| ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------- |
| All tasks are horizontal layers ("implement all types", "implement all tests") | Split into vertical slices that cut through layers                                          |
| No foundation tasks — first slice is huge                                      | Extract shared types and infrastructure into small foundation tasks                         |
| Every task is HITL                                                             | Most implementation from a clear spec is AFK. Reserve HITL for genuine ambiguity.           |
| Tasks don't reference spec sections                                            | Every task description must cite the contract items it implements                           |
| Coverage gaps not flagged                                                      | Run the coverage check. Missing items are either deferred (with reason) or added as tasks.  |
| Epic is too vague ("Implement the renderer")                                   | Epic must answer "what is different when this is done?" with a specific, verifiable outcome |

## Output

- A Trekker epic (if 5+ tasks)
- Trekker tasks with descriptions, tags, priorities, and dependencies
- A spec coverage table showing all contract items are accounted for
