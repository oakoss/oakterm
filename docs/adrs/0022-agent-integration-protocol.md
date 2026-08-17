---
adr: '0022'
title: Agent Integration Protocol Surface
status: accepted
date: 2026-08-16
tags: [core, agents]
---

# 0022. Agent Integration Protocol Surface

## Context

[Agent Protocol (ACP)](../ideas/39-agent-protocol.md) proposes that oakterm speak the Agent Client Protocol as a first-class client: structured streaming, native tool-call rendering, permission prompts, and slash commands for any ACP-capable agent. The doc was written when ACP was the only visible alternative to reverse-engineering each agent's ANSI output.

The [Agent Tooling Landscape Audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md) complicated that premise from both directions:

- **Native structured interfaces exist and dominate shipping integrations.** Claude Code exposes a stream-json subprocess interface and the Claude Agent SDK (which drives the locally installed CLI); Codex exposes an app-server JSON-RPC interface. Zeron, bb, T3 Code, and Atomic all integrate against these, not ACP. The Claude ACP adapter (`@agentclientprotocol/claude-agent-acp`) is itself a third-party wrapper over the same Agent SDK — maintained in Zed's agentclientprotocol org, not by Anthropic — adding an extra hop, a Node subprocess dependency, and a maintenance surface oakterm would not control.
- **ACP simultaneously got stronger.** The protocol reached stable version 1 (`session/resume`, `session/close`, `logout`, `session_info_update` stabilized), an official `codex-acp` server landed in the agentclientprotocol org, and Copilot CLI's ACP support entered public preview.

So the decision is not "ACP: yes/no" but **which protocol surface(s) oakterm speaks to agents, with what precedence** — and, downstream of that, the version-pin policy idea 39 left open (its open question 7).

Constraints that bound every option:

- **One policy engine.** [ADR-0021](0021-agent-control-api.md) establishes a single permission-enforcement choke point in the daemon. Whatever transport delivers an agent's permission request (`session/request_permission` or anything else), it routes through that engine. No transport choice may create a second enforcement path.
- **Plain CLI agents keep working.** Idea 39 already commits to opaque PTY panes as the fallback; a structured channel is opt-in per agent. This ADR governs only the structured channel.
- **Auth posture is fixed.** Env passthrough only ([idea 39](../ideas/39-agent-protocol.md), reaffirmed by the audit's policy timeline). No transport choice may require oakterm to handle provider credentials.
- **Phase 3 feature.** Nothing here ships before the plugin system (Phase 2); the decision is needed now because idea 39 blocks on it and the pane/diff/sidebar primitives it consumes are being specced.

## Options

### Option A: ACP only

oakterm speaks ACP; agents without a maintained ACP server are opaque PTY panes.

**Pros:**

- One protocol to implement, test, and document. The Rust `agent-client-protocol` crate fits the stack.
- Vendor-neutral by construction — matches the "terminal that runs agents the user configured" positioning.
- The stable-v1 milestone, official `codex-acp`, and Copilot preview all reduce the bet's risk relative to when idea 39 was written.

**Cons:**

- Claude — the agent oakterm's users most run — is reachable only through a third-party adapter that wraps the vendor's own SDK. Adapter lag, abandonment, or a Node-runtime requirement becomes oakterm's problem with no recourse.
- Capabilities that exist in a native interface but not in ACP (or not in a given adapter) are simply unavailable, no matter how load-bearing.

### Option B: Native interfaces first

Per-agent adapters against vendor-maintained surfaces: stream-json/Agent SDK for Claude Code, app-server JSON-RPC for Codex, and so on. ACP only if an agent offers nothing else.

**Pros:**

- Highest fidelity per agent; each interface is maintained by the vendor that ships the agent.
- Matches what the surveyed ecosystem (Zeron, bb, T3 Code, Atomic) actually does today.

**Cons:**

- Re-creates the per-(agent, feature) multiplication idea 39 exists to escape — N adapters, each with its own event shapes, each churning on a vendor's schedule.
- Fragments the Phase 2 plugin story: plugins participating in agent flows would target per-agent event shapes instead of one vocabulary.
- Every new agent is integration work before it lights up, inverting the "any ACP agent just works" property.

### Option C: One event vocabulary; ACP first; native transports by evidence

The agent-session spec owns one event vocabulary that the UI and plugins target — sessions, streamed message chunks, tool calls, permission requests, plans, file operations. ACP is the first and only transport built. A native transport (stream-json, app-server) may be added later as a second producer of the same vocabulary, but only when a documented trigger fires — never speculatively. The Rust trait boundary is **extracted when the second transport lands**, not designed up front: with one implementation, a trait is speculative abstraction; with two, it is refactoring against known requirements.

**Pros:**

- Ships with Option A's simplicity: exactly one transport is built, tested, and documented at first, and no speculative trait is designed against a single implementation.
- Escapes Option A's hostage problem: because UI and plugins couple to the spec's vocabulary rather than to ACP types directly, a native transport can be added without touching them or the permission path.
- Keeps the plugin story unified: plugins see the vocabulary regardless of transport.

**Cons:**

- The vocabulary is a real design cost, and defined against one transport it risks being ACP-in-disguise — acceptable, since ACP's vocabulary is the de-facto shape of the domain (LSP had the same property).
- If a native transport is added, its capabilities must map into the shared vocabulary; anything unmappable extends the vocabulary through a spec revision — the spec, not the transport, governs drift. Two transports also mean two conformance-test targets, plus the one-time trait extraction.

## Decision

**Option C.** ACP is the only transport built now; the agent-session spec owns the event vocabulary so a native transport is an addition, not a rewrite; the trait boundary is extracted when a second transport lands; native transports are gated on explicit triggers rather than built speculatively. The initial session model is one ACP session per pane — a deferral with a default (idea 39's OQ6), revisited with real usage rather than designed now.

This refines the [landscape audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md)'s recorded recommendation, which suggested an up-front internal trait: the grilling session concluded that with one planned implementation the trait is speculative abstraction, and the spec-owned vocabulary provides the same insulation.

The triggers that justify writing a native transport, recorded so the future decision is evidence-driven:

1. A capability oakterm needs is available in a vendor's native interface but absent from ACP or its adapter for two consecutive bumps of the pinned `agent-client-protocol` crate — the release-cadence unit that exists before oakterm has releases of its own.
2. The adapter for a major agent is abandoned or falls more than one major agent-version behind.
3. A reproducible latency, throughput, or reliability regression attributable to the adapter hop, demonstrated against the same agent driven directly.

**The Claude Node-runtime constraint.** The Claude ACP adapter is an npm package requiring a Node runtime, and the `claude` CLI itself no longer implies one — the [native installer](https://code.claude.com/docs/en/setup) is the recommended path, and even the npm install links a native binary that does not invoke Node at runtime. A Node toolchain is **not acceptable as a requirement of oakterm's default Claude experience**. Consequences: the default Claude pane remains the opaque interactive PTY (which needs nothing); the ACP adapter path is a documented opt-in for users with Node; and the Claude stream-json transport is **pre-designated as the second transport** — it may be scheduled on Phase 3 demand for structured Claude without Node, without waiting for triggers 1–3. The triggers govern all other native transports.

This is not the "both surfaces" shape ADR-0021's Option C rejected. That rejection was about two **permission-enforcement paths**; here every transport delivers events into the same vocabulary and every permission request routes through ADR-0021's single engine. Transport plurality behind one policy choke point is exactly the structure ADR-0021's MCP-as-thin-wrapper stance already endorses in the other direction.

**Version-pin policy (settles idea 39's open question 7):** pin the `agent-client-protocol` crate to a specific version and bump deliberately, reviewing the upstream changelog per bump. Do not track tip; do not vendor — post-v1 stabilization means vendoring's control isn't worth its merge burden. Capability negotiation, not version sniffing, gates optional features at runtime. The npm adapter, where used, is pinned the same way.

## Consequences

- [Idea 39](../ideas/39-agent-protocol.md) moves `reviewing → decided`. Its remaining open questions disperse: pane-tree placement (OQ1) to a [Spec-0007](../specs/0007-pane-tree-layout.md) revision; `fs/write_text_file` policy (OQ2), diff display (OQ3), and plan rendering (OQ4) to the agent-session spec below; the `terminal/*` sandbox boundary (OQ5) to the ADR-0021 permission substrate, to be specified in the forthcoming Spec-0012 (Agent Control API, owed by ADR-0021); multi-session-per-pane (OQ6) is deferred — one session per pane initially, revisited with real usage; HTTP/WebSocket ACP transport (OQ8) waits for Phase 4; AGPL-via-subprocess (OQ9) is a documentation note, not a decision.
- A future **agent-session spec** formalizes the event vocabulary: session lifecycle, event types, capability flags, and the mapping rules any second transport must satisfy. The spec, not any transport, governs vocabulary drift. Implementation does not start before that spec exists.
- **Re-verification clause.** Before Phase 3 implementation of the ACP client begins, the ACP/adapter landscape is re-verified against this ADR's premises (ACP version and stabilization state, adapter health for Claude/Codex/Gemini/Copilot, the Node-runtime situation), using the [landscape audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md)'s method. A premise that no longer holds reopens only the affected part of this decision, recorded as a superseding ADR if the shape itself changes.
- Permission requests from any transport route through the ADR-0021 engine; the vocabulary carries them as events, it does not evaluate them.
- The native-transport triggers above are the revisit conditions for all agents except Claude, whose stream-json transport is pre-designated per the Node-runtime constraint. Absent a trigger or that demand signal, no native adapter work is scheduled.
- The pinned-version policy makes ACP and adapter upgrades deliberate, reviewable events rather than ambient churn.

## References

- [Agent Protocol (ACP)](../ideas/39-agent-protocol.md) — the proposal this ADR resolves
- [Agent Tooling Landscape Audit](../reviews/2026-08-16-215731-agent-tooling-landscape-audit.md) — the evidence base: native-interface ecosystem, ACP v1 status
- [ADR-0021 Agent Control API](0021-agent-control-api.md) — the single permission engine every transport routes through
- [Agent Management](../ideas/07-agent-management.md) — lifecycle layer; structured channels are opt-in per agent on top of it
- [Plugin System](../ideas/06-plugins.md) — plugins target the spec's event vocabulary, not per-agent shapes
- [Spec-0007 Pane Tree & Layout](../specs/0007-pane-tree-layout.md) — receives the ACP-session pane-placement question
