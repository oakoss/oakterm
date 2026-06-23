---
title: Ultimate Terminal Alignment Audit
date: 2026-06-22T14:42:18
scope: 2026 terminal-emulator landscape (Ghostty, kitty, WezTerm, Alacritty, Warp, iTerm2) cross-checked against OakTerm's vision and roadmap
---

# Ultimate Terminal Alignment Audit

## Scope

A competitive-landscape pass over the 2026 terminal field — performance, native-vs-cross-platform tradeoffs, extensibility, the Ghostty memory-leak episode, the kitty/Ghostty rivalry, and Warp's AI-native model — checked against OakTerm's principles, roadmap, and memory-management design. Goal: find where the project's design is validated by the field, where it goes beyond it, and where the field surfaces an unresolved question.

Not a code review. Sources are external (vendor docs, release notes, GitHub issues, post-mortems) plus OakTerm's own idea docs, ADRs, and roadmap.

## Findings

### Validated Decisions

- **Native core, per-platform shells, not Electron.** The field's clearest lesson: cross-platform terminals that wrap a Linux renderer feel non-native on macOS; Ghostty's native-AppKit/Metal approach is why it leads on macOS latency/throughput. OakTerm's daemon/client split + `TextShaper` trait + AppKit/GTK/WinUI matches this.
- **Memory as a first-class, bounded subsystem.** Ghostty's leak (users hit 37 GB over 10 days; 71 GB on a 16 GB machine) came from a ~3-year-old scrollback-page bug that _agentic CLI output_ (Claude Code) finally triggered at scale; fixed in Ghostty 1.3 (March 2026). OakTerm already enumerates this exact case and answers it with a tiered ring+disk buffer ([ADR-0006](../adrs/0006-scroll-buffer-architecture.md)), per-pane budgets, and a "Claude Code simulation" CI soak test. The project is ahead of the field here, with receipts.
- **AI timing split.** Agent _plumbing_ ([Agent Control API](../ideas/32-agent-control-api.md), [ACP](../ideas/39-agent-protocol.md), agent management) in Phase 3; NL/AI _text features_ in Phase 5. Warp's churn shows chat UX is fashion while provider-neutral plumbing is durable — the split is correct, hold it.
- **Open, zero-telemetry, BYOK/local AI.** Warp's capabilities are real but its closed-source/account/telemetry model is disqualifying for a daily-driver tool; OakTerm's stance is a genuine differentiator, not just a principle.

### Differentiators not forced by the field

Design strengths the competitive set does _not_ push you toward:

- **Accessibility from day one (AccessKit, [ADR-0001](../adrs/0001-accessibility-in-phase-zero.md)).** No GPU-accelerated terminal ships cross-platform screen-reader support (Windows Terminal has UIA on Windows, but nothing portable). Building it into the renderer in Phase 0 (vs bolting on later) makes it structurally harder for competitors to catch up.
- **Process-dashboard / "everything is a pane" thesis.** Reframes the terminal as an orchestration surface for agents/services/watchers rather than "fast terminal + AI feature." No competitor is building this; the others are all iterating on the same pane/tab model.

### Contradictions / ADR candidates

Two unresolved questions the landscape surfaces. Each needs an ADR; do not resolve inline.

1. **Command "blocks" UX vs. semantic-zone substrate.** Warp's foldable/re-runnable command+output blocks are genuinely good UX. OakTerm has the _substrate_ (OSC 133/7 semantic zones, scroll-to-prompt) but no committed decision on block-style rendering/folding/re-run. The plumbing makes this nearly free to add now and expensive to retrofit if not reserved for. Decide: deliberate omission (per the "don't become a chat app/IDE" line) or planned feature? **ADR candidate.**
2. **"Replaces tmux, not complements it" — coexistence stance.** Right ambition, but one inch from kitty's mistake. kitty's error wasn't building a multiplexer; it was the maintainer's _hostility_ to the tmux workflow. "Replaces, not complements" must mean "you won't need tmux," not "tmux is unwelcome" — users will still attach to plain tmux over SSH or rely on years of muscle memory. Needs an explicit stance on graceful coexistence. **ADR candidate.**

## Action Items

- **ADRs:** (1) blocks/semantic-zone decision; (2) tmux coexistence stance. Both are shaped enough to open as `proposed`.
- **Process (no doc):** hold the Phase 0→1 line. The biggest risk for an effort this ambitious (39 idea docs, 6 phases, WASM plugin platform, ACP, web client) is not bad architecture — the "plugin is the product" design mitigates that — it's never finishing the multiplexer because the backlog pulls focus. Everything in Phases 3–5 is a distraction until Phase 1 replaces tmux for daily use.
- **Positioning (later):** the memory story (Ghostty 71 GB → bounded ring buffer + CI soak test) and day-one accessibility are both shippable headline claims with evidence.
