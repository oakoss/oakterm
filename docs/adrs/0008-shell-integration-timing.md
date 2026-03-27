---
adr: '0008'
title: Shell Integration Timing
status: accepted
date: 2026-03-26
tags: [core]
---

# 0008. Shell Integration Timing

## Context

Shell integration (OSC 133 semantic prompt marking, OSC 7 current working directory) is currently placed in Phase 3 (shell intelligence) in [33-roadmap.md](../ideas/33-roadmap.md). However, multiple features across earlier phases depend on it:

- Alternate screen capture ([ADR 0006](0006-scroll-buffer-architecture.md)) benefits from prompt boundaries
- Scroll-to-prompt navigation (Phase 1)
- Per-command output selection (Phase 1)
- Process completion notifications (Phase 1)
- Context engine command boundaries (Phase 2)

The review audit flagged this as a phasing concern: Phase 3 is late for data that Phase 1 features need.

Additionally, [ADR 0006](0006-scroll-buffer-architecture.md) established alternate screen scrollback capture as a Phase 0 feature. Shell integration marks enrich this captured data with semantic boundaries.

## Options

### Option A: Full shell integration in Phase 0

Parse OSC 133/7 marks, store them in the screen buffer, and build user-facing features (scroll-to-prompt, per-command selection, notifications) all in Phase 0.

**Pros:**

- Complete experience from day one.

**Cons:**

- Phase 0 is already large (renderer, AccessKit, scroll buffer, daemon/client split, Lua config). Adding UI features increases scope further.
- Shell integration UI features depend on the multiplexer (Phase 1) for multi-pane navigation.

### Option B: Parsing in Phase 0, features in Phase 1

Phase 0: VT parser captures OSC 133 marks (A=prompt start, B=input start, C=output start, D=output end + exit status) and OSC 7 (current working directory). Marks stored in the screen buffer alongside text. No user-facing features.

Phase 1: Scroll-to-prompt, per-command output selection, process completion notifications, command duration display.

**Pros:**

- Phase 0 scope stays focused on the rendering foundation.
- The data layer is ready when Phase 1 needs it — no retroactive parser changes.
- Shell integration parsing is a small addition to the VT parser (a few escape sequence handlers).

**Cons:**

- Phase 0 users don't benefit from shell integration visually.

### Option C: Defer everything to Phase 3 (current plan)

Keep shell integration entirely in Phase 3 as originally planned.

**Pros:**

- Smallest Phase 0 and Phase 1 scope.

**Cons:**

- Phase 1 features (scroll-to-prompt, notifications) can't be built until Phase 3.
- The VT parser must be retrofitted to capture marks that it previously discarded.
- Contradicts the principle of building the data layer early.

## Decision

**Option B — parsing in Phase 0, features in Phase 1.**

The VT parser captures OSC 133 and OSC 7 marks from day one. The marks are stored in the screen buffer as metadata alongside the text they annotate. No user-facing shell integration features in Phase 0 — those come in Phase 1 when the multiplexer provides the navigation context.

### Phase 0 Scope (Data Layer)

- Parse OSC 133 marks: `A` (prompt start), `B` (input start), `C` (output start), `D;exit_status` (output end)
- Parse OSC 7: `file://{hostname}/{path}` (current working directory)
- Store marks as metadata in the screen buffer rows
- No UI — marks exist in the data but are not surfaced to the user

### Phase 1 Scope (Features)

- Scroll-to-prompt navigation (Ctrl+Shift+Up/Down or similar)
- Per-command output selection/copy
- Process completion notifications (command finished with exit status)
- Command duration display
- CWD-aware tab/pane titles (from OSC 7)

### Graceful Degradation

When the shell does not emit OSC 133 marks (bash without integration, SSH to a remote without shell integration, non-standard shells):

- Shell integration features are unavailable. No fake marks, no heuristic prompt detection.
- The terminal works as a normal terminal — shell integration features are additive, not required.
- `oakterm doctor` reports whether the current shell has integration installed.
- `oakterm shell-integration install` helps users set it up.

## Consequences

- Update [33-roadmap.md](../ideas/33-roadmap.md) to move OSC 133/7 parsing from Phase 3 to Phase 0, and shell integration UI features to Phase 1.
- Update [18-shell-integration.md](../ideas/18-shell-integration.md) to reflect the two-phase split.
- The VT parser specification (Phase 0 spec) must include OSC 133 and OSC 7 handling.
- Screen buffer row metadata must have fields for semantic marks.
- Phase 1 multiplexer features can assume marks are present in the buffer when available.

## References

- [18-shell-integration.md](../ideas/18-shell-integration.md)
- [33-roadmap.md](../ideas/33-roadmap.md)
- [ADR 0006: Scroll Buffer Architecture](0006-scroll-buffer-architecture.md)
- [OSC 133 specification (iTerm2)](https://iterm2.com/documentation-escape-codes.html)
