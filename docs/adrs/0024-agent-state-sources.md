---
adr: '0024'
title: Agent State Sources and Precedence
status: accepted
date: 2026-08-16
tags: [core, agents, security]
---

# 0024. Agent State Sources and Precedence

## Context

[ADR-0023](0023-agent-state-vocabulary.md) fixes _what_ the agent states are. This ADR fixes _who may say so_. The docs currently disagree: [Agent Management](../ideas/07-agent-management.md) detects state from process output using configurable per-provider patterns (heuristics-primary), while [Agent Protocol](../ideas/39-agent-protocol.md) treats ACP `session/update` events as authoritative for ACP-aware panes. Neither says what happens when sources conflict, and the [herdr review](../reviews/2026-05-06-164003-herdr-architecture-review.md) ranked its hybrid three-tier model first among its borrow-worthy patterns, calling it the load-bearing architectural decision — parked in TREK-184 since May.

There are four candidate sources with different trust and availability properties:

1. **Process detection** — the daemon owns the PTY and child process; it knows liveness, exit, and process identity with certainty, always.
2. **Protocol events** — ACP `session/update` ([ADR-0022](0022-agent-integration-protocol.md)) and agent hook scripts for agent state; OSC 133 marks ([ADR-0008](0008-shell-integration-timing.md)) for command lifecycle. Semantically rich, present only when the agent or shell emits them, and each speaks only to its own claim.
3. **Self-report** — `oakterm ctl self set-status` ([ADR-0021](0021-agent-control-api.md)). Explicit, but only as trustworthy as the cooperative agent making the claim.
4. **Screen heuristics** — regex over pane output per idea 07's `agent_providers` patterns. Universally available, least reliable.

A related sourcing question rides here because it shares the same shape — what may feed an agent-facing surface: the [landscape audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md) found Orca's usage meters reading Claude Code's OAuth token from the macOS Keychain and refreshing it against Anthropic's endpoints, while its _fallback_ parses `/usage` from a hidden PTY. oakterm needs the rule written down before any usage surface exists.

## Options

### Option A: Protocol-authoritative

ACP/hook events are the single source of truth; panes without them show `unknown`.

**Pros:**

- Highest-fidelity signal when present; no heuristic false positives.

**Cons:**

- Every plain-CLI agent — the majority today, and the default Claude path per ADR-0022 — shows `unknown` forever. The sidebar's core promise dies for exactly the panes users run most.

### Option B: Heuristics-primary

Pattern-match output for all panes, protocol events merely another input (idea 07 as written).

**Pros:**

- Every pane gets a state from day one.

**Cons:**

- Heuristics can contradict better sources — a pattern matching "Done!" in scrollback while ACP streams `tool_call` events means flickering, wrong badges. Pattern maintenance becomes load-bearing for correctness instead of best-effort.

### Option C: Hybrid three-tier with fixed precedence

Process detection owns identity and liveness unconditionally; explicit reports (protocol events and self-report) own semantic state when present; heuristics fill in only where no agent-level explicit source exists.

**Pros:**

- Every pane gets the best state its signals can support; better signals always win.
- Heuristic failure degrades to `unknown`, never overrides truth.

**Cons:**

- Three code paths and a precedence rule to specify and test.

## Decision

**Option C**, with the precedence rules made explicit:

1. **Liveness beats semantics.** Process detection owns pane identity, liveness, and terminal transitions. When the child exits, the pane is `done` with an outcome derived from how it exited — exit 0 → `success`; non-zero exit or unexpected signal termination (SIGSEGV, SIGABRT, an OOM kill) → `error`; `cancelled` only when the user, or oakterm acting on the user's behalf, initiated the stop — with the exact mapping formalized in the agent-session spec — regardless of what any protocol event or self-report last claimed. A vanished process can never be `working`.
2. **Explicit beats inferred — per claim, not per pane.** Each source ranks only within the claim it can actually make. OSC 133 marks are authoritative for _command lifecycle_ — a command started, it finished, its exit status — but claim nothing about agent state inside a running command. ACP `session/update`, agent hooks, and `ctl self set-status` are authoritative for _agent semantic state_ (`working`/`blocked`, and `done`+outcome for in-pane task completion); the most recent such report wins, and heuristics never override an agent-level report for the lifetime of the session.
3. **Heuristics are the floor for agent semantic state.** Provider patterns run whenever no agent-level explicit source (ACP, hooks, self-report) has spoken — shell-level OSC marks alone do not suppress them, or the default interactive Claude PTY could never show `blocked`. When patterns don't match, the pane reports `unknown` rather than guessing. Per-provider patterns stay user-configurable config (idea 07), best-effort by contract.
4. **Subagent completion does not complete the parent.** A subagent stop event (e.g. Claude Code subagent hooks) maps back to `working` on the parent pane, never to `done`/`idle` — the expensively-learned herdr detail, recorded here so it is never rediscovered.
5. **Release is for claimed panes, not agent-owned panes.** When an agent was claimed on top of a surviving shell (hook-attached), a crash or explicit `release` returns the pane to plain-shell state — clearing agent state rather than freezing a stale badge (lifecycle to be specced in Spec-0012). When the agent _is_ the pane's child process, rule 1 governs instead: the exit-derived `done`+outcome persists until acknowledged, so a crashed agent stays first in the attention cycle rather than vanishing from it.

**Usage and cost surfaces obey the same source discipline.** Any agent usage, cost, or rate-limit display sources its data from (a) PTY-owned output the daemon already possesses — statusline content, `/usage` output, OSC-carried metadata — or (b) agent self-report via `oakterm ctl` ([ADR-0021](0021-agent-control-api.md)). oakterm never reads, refreshes, or transmits provider credentials (OAuth tokens, API keys, keychain entries) to fetch account or usage data — the [landscape audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md)'s Orca finding is the documented anti-pattern, its PTY fallback the pattern. This extends the auth posture ([idea 39](../ideas/39-agent-protocol.md)) from inference credentials to _all_ provider credentials: a terminal's privileged position is owning the PTY, and that is the only privilege it uses.

## Consequences

- The idea 07 (heuristics-primary) vs idea 39 (protocol-authoritative) ambiguity is resolved; both docs absorb the tiered model in the TREK-183 edit batch alongside [ADR-0023](0023-agent-state-vocabulary.md)'s vocabulary.
- The agent-session spec and Spec-0012 encode the precedence rules and the `release` lifecycle; conformance tests exercise the conflict cases (live process + stale hook state; exited process + optimistic self-report; subagent stop; OSC 133 command-finished arriving on a pane whose agent is `blocked` — agent state must not flip).
- [31-brainstorm](../ideas/31-brainstorm.md)'s "Agent Cost/Usage Visibility" sketch can be promoted into [Sidebar](../ideas/04-sidebar.md) with its data sources now constrained; `ccusage`-style token displays are in scope only insofar as their data arrives through the two permitted channels.
- TREK-184 sheds its state-source item; six unrelated parked decisions remain there.
- An agent-level source that reports once and then goes silent freezes semantic state until process exit — accepted deliberately to prevent heuristic flicker. If real usage shows silent agents stranding stale states, a staleness window on rule 2 is the revisit knob.
- [Security](../ideas/21-security.md) gains the credential-reading prohibition as a stated principle when it next absorbs updates.

## References

- [Herdr Architecture Review](../reviews/2026-05-06-164003-herdr-architecture-review.md) — the three-tier model and the subagent-stop detail
- [Agent Tooling Landscape Audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md) — the Orca usage-meter finding behind the credential rule
- [ADR-0023 Agent State Vocabulary](0023-agent-state-vocabulary.md) — the states these sources set
- [ADR-0022 Agent Integration Protocol](0022-agent-integration-protocol.md) — the structured channel supplying tier-2 events
- [ADR-0021 Agent Control API](0021-agent-control-api.md) — self-report surface and the permission substrate
- [ADR-0008 Shell Integration Timing](0008-shell-integration-timing.md) — OSC 133 marks as a tier-2 source
- [Agent Management](../ideas/07-agent-management.md), [Agent Protocol](../ideas/39-agent-protocol.md) — the docs whose conflict this resolves
