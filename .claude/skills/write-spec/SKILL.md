---
name: write-spec
description: |
  Write a formal specification from accepted ADRs and idea docs. Interviews about contract boundaries, sketches bounded concerns, and produces a spec with no hand-waving.

  Use when: transitioning from accepted ADRs to implementation specs, formalizing API surfaces, wire protocols, or data formats. Must have at least one accepted ADR as input.
---

# Write Spec

Turn accepted ADRs into formal implementation contracts. A spec defines the exact interface, behavior, and constraints that code must satisfy.

Skip for: decisions that don't need formal contracts (tooling config, doc conventions), or when the ADR itself is specific enough to implement from directly.

## Process

### 1. Gather Input

Read the accepted ADR(s) that led to this spec. Read the idea docs they reference. Identify what the spec must formalize — the ADR decided _what_, the spec defines _how exactly_.

### 2. Interview About the Contract Boundary

Walk through each branch of the contract, resolving ambiguity:

- **Types**: What are the exact data structures? Field names, types, optionality, invariants.
- **Functions/Messages**: What are the signatures? Parameters, return types, error types.
- **Error Cases**: What can go wrong? How is each error reported? What does the caller do?
- **Edge Cases**: Empty input, maximum sizes, concurrent access, version mismatches.
- **Constraints**: Performance budgets, memory limits, security requirements.
- **Ordering**: Are operations sequential, concurrent, or unordered? What happens during races?

For each question, provide a recommended answer. If the answer requires exploration (reading existing code, checking a dependency's API), do the exploration.

### 3. Sketch Bounded Concerns

Identify if this spec should be one document or split into multiple specs. One spec per bounded concern:

- One API surface (plugin API, config API, agent control API)
- One wire protocol (daemon↔client IPC, remote access WebSocket)
- One data format (session serialization, scroll buffer archive format)

Look for opportunities to extract deep modules — interfaces that encapsulate complexity behind a simple, testable boundary that rarely changes.

### 4. Write the Spec

Use the project's spec template (`docs/specs/0000-template.md`). Every section must be concrete:

**Overview**: One paragraph — what this spec defines and why.

**Contract**: The formal definition. No hand-waving. Every type is defined. Every function has a signature. Every message has a format. If referencing an IDL file (`.proto`, `.wit`), the IDL is the source of truth and the spec explains behavior and constraints the IDL can't express.

**Behavior**: Expected behavior for normal and error cases. Edge cases called out explicitly. State transitions documented if applicable.

**Constraints**: Performance budgets, memory limits, security requirements, backward compatibility guarantees.

**References**: Link to the ADR(s) that led here and the idea doc(s) for broader context.

### 5. Rigor Check

Before presenting the spec:

- [ ] Every type is fully defined (no `TBD`, `details later`, `to be determined`)
- [ ] Every function/message has input types, output types, and error types
- [ ] Every error case has a defined behavior (what happens, what the caller sees)
- [ ] Edge cases are called out (empty, maximum, concurrent, disconnected)
- [ ] Constraints are measurable (not "fast" but "< X ms" or "competitive with Y")
- [ ] All ADR decisions are reflected in the spec
- [ ] Cross-references link to real docs
- [ ] If an IDL file is referenced, its path is specified

### 6. Checks

- `mise run fmt` — format
- `mise run lint` — lint
- `/de-slopify` on all prose
- Verify frontmatter is complete (spec number, title, status: draft, date, ADR references, tags)
- Check all cross-references resolve

### 7. Update Index

- Add the new spec to the index table in `docs/specs/README.md`.
- Update cross-references in related ADR and idea docs if needed.

### 8. Commit Gate

Present the spec summary and wait for explicit "commit" before committing.

## Common Mistakes

| Mistake                                   | Fix                                                                  |
| ----------------------------------------- | -------------------------------------------------------------------- |
| Spec restates the ADR's rationale         | Spec defines the contract; ADR explains why. Don't duplicate.        |
| "TBD" or "details later" in any section   | Resolve it now or split into a separate spec with a dependency note. |
| Types defined only by example             | Write the full type definition. Examples supplement, not replace.    |
| Error cases listed without behavior       | For each error: what does the system do? What does the caller see?   |
| Performance constraints as absolutes      | Use "competitive with X" or "measured by benchmark Y" per ADR-0002.  |
| Spec too broad (covers multiple concerns) | Split into focused specs. One API surface, one protocol, one format. |

## Output

A spec file at `docs/specs/NNNN-short-title.md` following the project template. If the spec references an IDL, the IDL file path is specified (the file itself may not exist yet — the spec is the contract, the IDL is generated during implementation).
