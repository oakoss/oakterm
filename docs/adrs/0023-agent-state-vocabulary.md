---
adr: '0023'
title: Agent State Vocabulary
status: accepted
date: 2026-08-16
tags: [core, agents]
---

# 0023. Agent State Vocabulary

## Context

Three docs carry three different agent-state models. [Sidebar](../ideas/04-sidebar.md) badges four states (working / needs-input / done / error). [Agent Control API](../ideas/32-agent-control-api.md) sketches `self set-status working|needs-input|done|error`. The [herdr review](../reviews/2026-05-06-164003-herdr-architecture-review.md) recommended a five-state vocabulary — `working / blocked / done (finished, unseen) / idle (finished, seen) / unknown` — whose done→idle transition is what makes "jump to the next pane needing attention" terminate: without it, every finished pane stays "done" forever and the `Cmd+Shift+U` cycle from [Notifications](../ideas/34-notifications.md) never drains. That recommendation has been parked in TREK-184 since May.

The [Agent Tooling Landscape Audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md) added corroboration: Shep ships provider-aware status indicators, Zeron surfaces per-session state, and Orca tracks agent liveness per terminal — three shipping products displaying agent state while oakterm's docs still disagree on the vocabulary.

The herdr five cannot be adopted verbatim: it has no error state, and error is the state [Notifications](../ideas/34-notifications.md) ranks _first_ in its attention cycle (errors → needs-input → warnings → recent done). Reconciling error against the attention lifecycle is the core of this decision. A grilling session (2026-08-16) also surfaced two adjacent questions settled here: who owns "seen" when multiple clients view one daemon, and whether the vocabulary is extensible.

## Options

### Option A: Keep the four-state model

`working / needs-input / done / error` as idea 04 sketches.

**Pros:**

- Already in two idea docs; no migration.
- Error is first-class.

**Cons:**

- No seen/unseen distinction, so the attention cycle cannot terminate — the herdr review's central finding stands unaddressed.
- Conflates lifecycle (is it running?) with outcome (did it succeed?): "done" and "error" are peers in the enum but answer different questions.

### Option B: Herdr's five states verbatim

`working / blocked / done / idle / unknown`, folding error into blocked (anything needing attention is blocked).

**Pros:**

- Proven in a shipping agent multiplexer; the attention cycle terminates.

**Cons:**

- Deletes the error/needs-input distinction the notification priorities depend on: "the agent crashed" and "the agent has a question" demand different responses and different badge colors.
- An acknowledged error has nowhere to live — it becomes plain idle, indistinguishable from a success.

### Option C: Five lifecycle states plus an outcome field

Lifecycle: `working / blocked / done / idle / unknown`, where done→idle on acknowledgment. Completion (`done`, and `idle` after acknowledgment) carries an **outcome**: `success | error | cancelled`. "The agent crashed and nobody has looked" is `done + error`; acknowledged, it becomes `idle + error` — still visibly a failure, no longer demanding attention.

**Pros:**

- The attention cycle terminates _and_ error keeps its first-place priority: the cycle orders `done+error` → `blocked` → `done+success`.
- Lifecycle and outcome answer their own questions independently; no combinatorial enum.
- Maps cleanly onto both existing four-state docs (needs-input → blocked; error → done+error) and herdr's five.

**Cons:**

- Two fields instead of one everywhere state crosses a boundary (wire, Lua, sidebar data model).

## Decision

**Option C.** Five lifecycle states — `working / blocked / done / idle / unknown` — with an outcome field (`success | error | cancelled`) attached at completion. For agent entries the attention cycle drains in priority order `done+error`, then `blocked`, then `done+success`; acknowledgment moves done→idle without erasing the outcome.

**This vocabulary models agent panes only, and slots into — not replaces — the notifications ordering.** [Notifications](../ideas/34-notifications.md)' full cycle stays errors → needs-input → warnings → recent done, with the agent states mapping onto the errors, needs-input, and done tiers; the warnings tier (⚠, set by service-monitor and memory alerts) is non-agent badge territory that keeps its slot and its existing semantics, owned by the notifications doc. Command lifecycle (long-running command completion, [Shell Integration](../ideas/18-shell-integration.md)) is likewise a separate axis owned by shell integration ([ADR-0008](0008-shell-integration-timing.md), [ADR-0015](0015-command-blocks.md)): it feeds the attention cycle's done tier for plain shells without entering the agent state enum. The recency qualifier also survives: `done+success` ages out of the cycle after the notifications doc's recency window even if never acknowledged, while `done+error` and `blocked` never age out — ignored successes drain, ignored failures don't. `done+cancelled` is user-initiated and demands no attention; it drains like `done+success`.

**Acknowledgment ("seen") is daemon-global.** Viewing the pane from any client — desktop, a second window, a Phase 4 phone client — clears it everywhere. This matches the semantics of the word (the _user_ looked at it, on whatever glass) and costs one bit in the daemon rather than another per-client state set — the protocol already tracks per-client viewport pins ([ADR-0012](0012-copy-mode-scrollback-access.md)); seen-ness doesn't need to join them. Revisit only if real multi-user daemon sharing (not multi-device single-user) ever exists.

**The vocabulary is a closed enum with a free-form detail string.** The five states and three outcomes are the complete machine vocabulary — the attention cycle, badge semantics, and wire encoding are total functions over them, forever. Agents and plugins add color through a free-form detail/badge string (`oakterm ctl self set-badge`, sketched in [Agent Control API](../ideas/32-agent-control-api.md)) that renders next to the state but never affects cycling, filtering, or notification routing. `set-badge` joins [ADR-0021](0021-agent-control-api.md)'s always-allowed `self` action class when Spec-0012 formalizes the surface.

## Consequences

- [Sidebar](../ideas/04-sidebar.md), [Agent Control API](../ideas/32-agent-control-api.md) (`set-status` takes a lifecycle state; outcome is set by completion reporting), and [Notifications](../ideas/34-notifications.md) (agent entries restated over state+outcome; the warnings tier and non-agent badges unchanged) absorb the vocabulary — the TREK-183 edit batch, unblocked together with [ADR-0024](0024-agent-state-sources.md).
- The agent-session spec and Spec-0012 encode the enums on the wire. Closed governs the current vocabulary, not future additive bumps: per [ADR-0020](0020-daemon-upgrade-version-skew.md)'s version-skew rules, a client receiving an unrecognized state or outcome value renders `unknown`/no outcome rather than failing deserialization.
- TREK-184 sheds its state-vocabulary item; six unrelated parked decisions remain there.
- `unknown` is a real state, not an error: it is what heuristic-only detection reports before any signal arrives ([ADR-0024](0024-agent-state-sources.md) owns who reports what).

## References

- [Herdr Architecture Review](../reviews/2026-05-06-164003-herdr-architecture-review.md) — origin of the five-state model and the done→idle insight
- [Agent Tooling Landscape Audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md) — 2026 corroboration across Shep/Zeron/Orca
- [Sidebar](../ideas/04-sidebar.md), [Notifications](../ideas/34-notifications.md), [Agent Control API](../ideas/32-agent-control-api.md) — the docs this vocabulary reconciles
- [ADR-0024 Agent State Sources](0024-agent-state-sources.md) — which sources may set these states
