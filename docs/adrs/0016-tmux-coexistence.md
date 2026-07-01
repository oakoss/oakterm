---
adr: '0016'
title: tmux Coexistence Stance
status: accepted
date: 2026-06-22
tags: [core, multiplexer]
---

# 0016. tmux Coexistence Stance

## Context

OakTerm's Phase 1 multiplexer aims to replace tmux — tabs, splits, workspaces, session persistence ([03-multiplexer.md](../ideas/03-multiplexer.md)). The project principle states "Replaces tmux, not complements it."

The [2026 competitive review](../reviews/2026-06-22-144218-ultimate-terminal-alignment-audit.md) flagged a risk: "replaces, not complements" is one inch from kitty's mistake. kitty's error was not building a multiplexer — it was hostility to the tmux _workflow_, which produced "won't fix" responses to tmux-interaction bugs. Users will still run tmux inside OakTerm: attaching to a session on a remote box over SSH, years of muscle memory, or shared sessions. The question: what is our stance when tmux runs inside OakTerm?

This is distinct from "do we build tmux control-mode integration" — that would be _complementing_ tmux (rendering its sessions as native panes, iTerm2-style), which the principle explicitly rejects.

## Options

### Option A: Replace-only (kitty-adjacent)

Optimize solely for the native multiplexer. tmux runs as any other program but gets no special accommodation; known frictions (kitty keyboard protocol passthrough, scrollback/mouse interplay) are low priority.

Pros: simplest; all effort goes to the native multiplexer.
Cons: repeats kitty's reputational mistake; tmux-over-SSH and muscle-memory users hit papercuts we treat as out of scope; "replaces" reads as "is hostile to."

### Option B: Graceful coexistence, no integration

The native multiplexer is the recommended path ("you won't need tmux"), but tmux-inside-OakTerm must work _correctly_: kitty keyboard protocol passthrough behaves, mouse/scrollback interplay is correct, and the terminal's own scrollback/search never leaks tmux's off-screen history into the native scrollbar or search (the exact bug Ghostty hit and addressed in 1.3). We do not build tmux control-mode integration.

Pros: "you won't need tmux, but if you use it, it works" — avoids kitty's mistake without the cost of integration; the correctness bar (don't leak alt-screen history) already follows from [ADR-0006](0006-scroll-buffer-architecture.md); honors "replaces, not complements" (no control-mode).
Cons: a standing compatibility commitment to test against tmux; some effort not spent on native features.

### Option C: Full complement (control-mode integration)

Implement tmux control mode (iTerm2-style) so tmux sessions render as native OakTerm tabs/panes.

Pros: best tmux interop; tmux sessions feel native.
Cons: directly contradicts "replaces, not complements"; large, ongoing maintenance surface tied to tmux internals; competes with our own multiplexer.

## Decision

**Option B — graceful coexistence without integration.**

The stance is "you won't need tmux, but if you run it, it works correctly." We commit to the correctness bar — keyboard protocol passthrough, mouse/scrollback behavior, and (in the default configuration) never leaking tmux's alternate-screen history into native scrollback/search (consistent with [ADR-0006](0006-scroll-buffer-architecture.md)) — and explicitly do not build tmux control-mode integration, which would be complementing rather than replacing. This avoids kitty's reputational failure mode while keeping the principle intact.

## Consequences

- Testing: add a tmux-inside-OakTerm compatibility check (keyboard protocol passthrough, alt-screen scrollback isolation, mouse).
- Alt-screen scrollback isolation follows from [ADR-0006](0006-scroll-buffer-architecture.md) under the default `save_alternate_scrollback = false` (tmux is an alt-screen app); this ADR makes "don't leak tmux history into native search/scrollbar" an explicit, tested guarantee **in the default configuration**. If a user opts into `save_alternate_scrollback = true` they have chosen to capture alt-screen content (tmux included); whether to add a tmux-specific capture exclusion is an open question.
- No tmux control-mode work is scoped, now or later, unless this ADR is superseded.
- [03-multiplexer.md](../ideas/03-multiplexer.md) and the README now frame the multiplexer as "you won't need tmux/Zellij/screen — but they coexist cleanly if you do," replacing the earlier "replaces tmux, not complements it" wording.

## References

- [2026 Ultimate Terminal Alignment Audit](../reviews/2026-06-22-144218-ultimate-terminal-alignment-audit.md)
- [03-multiplexer.md](../ideas/03-multiplexer.md)
- [ADR-0006: Scroll Buffer Architecture](0006-scroll-buffer-architecture.md)
