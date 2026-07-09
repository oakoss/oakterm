---
adr: '0021'
title: Agent Control API — Transport and Permission Model
status: accepted
date: 2026-07-08
tags: [core, security, agents]
---

# 0021. Agent Control API — Transport and Permission Model

## Context

[Agent Control API](../ideas/32-agent-control-api.md) proposes `oakterm ctl`: a way for agents — or any process running in a pane — to drive and observe the terminal (list/read/input panes, create panes, set their own status, notify, prompt the user) by talking to the daemon over its Unix socket. Two questions must be settled before the surface can grow past a developer tool:

1. **Transport.** Is the agent-facing surface a **CLI** (`oakterm ctl`, the idea doc's stance) or an **MCP server** oakterm exposes for MCP-capable agents to connect to?
2. **Permission and security model.** Idea 32 sketches a rich model — per-pane default-deny permissions, escalation prompts, six-dimension risk scoring — and the earlier "is `oakterm ctl` a security risk?" question is unanswered. What does the socket actually guarantee, what is the threat model, and in what order does the model land?

The first slice already shipped. TREK-275 built the thin `ctl` client (`pane list` / `input` / `output`) as a **local developer tool** behind an explicit gate: `OAKTERM_SOCKET` is _not_ injected into pane environments and the tool is not advertised to agents, precisely because none of the permission model below exists yet. This ADR decides the contract that lifts that gate, and unblocks the production agent skill (TREK-277).

Constraints that bound every option:

- The daemon socket already exists (server/client split, [ADR-0007](0007-daemon-architecture.md)) and is created `0700` in a per-UID directory ([Spec-0001](../specs/0001-daemon-wire-protocol.md), socket helpers now in `oakterm-protocol`, TREK-176). It is reachable only by the same UID.
- On a same-UID local Unix socket there is **no isolation boundary between processes of that user**. Any same-user process can `connect()` to a `0700` socket and read another same-user process's environment (`/proc/<pid>/environ`, `ps eww`) and — subject to platform debugging controls — often its memory (`ptrace`, `task_for_pid`). This fact drives the security framing below.
- Escalation prompts ("Agent wants to read pane X — Allow Once / Always / Deny") need a modal/palette surface that does not exist until the EPIC-11 command-palette and status-bar chrome lands.

This ADR is the inverse-direction counterpart to [Agent Protocol (ACP)](../ideas/39-agent-protocol.md): idea 39 is terminal → agent (structured streaming, `session/request_permission`); this is agent → terminal. The permission substrate decided here is what ACP's `session/request_permission` plugs into, so the two must not invent separate policy engines.

## Options

### Option A: CLI (`oakterm ctl`)

A binary that speaks the daemon's existing wire protocol. Agents invoke it through their normal bash/tool-use path; scripts and humans invoke it the same way.

**Pros:**

- **Universal reach with zero per-agent integration.** Any agent that can run a shell command (Claude Code, Codex, Aider, Goose, custom scripts) uses it as-is. No MCP client, no capability negotiation, no per-agent config.
- Works in bash scripts, makefiles, and CI — contexts that have no MCP client at all.
- Debuggable and scriptable: run `oakterm ctl pane list` yourself in a shell to see exactly what an agent sees.
- Reuses the socket and wire protocol that already exist; one enforcement point in the daemon.
- The first slice (TREK-275) already proves the shape end to end.

**Cons:**

- Output is text/JSON the agent must parse, rather than typed tool results.
- Agents must discover the command surface (mitigated by `--help`, structured `--format json`, and the production skill in TREK-277).

### Option B: MCP server

oakterm exposes an MCP server; MCP-capable agents connect and call typed tools (`pane_list`, `pane_input`, …).

**Pros:**

- Native typed tool schemas and discovery for MCP-capable agents; no bash string parsing.
- Aligns with the direction agent tooling is standardizing on.

**Cons:**

- **Requires additional setup per agent** — an MCP client, a configured server entry, and a running server/transport. A plain script, a CI job, or any non-MCP agent gets nothing.
- Chicken-and-egg with the per-pane socket: the agent must be told which socket/endpoint to connect to before it can call a tool, which is the same discovery problem the CLI solves with one env var and an exec.
- A second protocol surface to version, document, and secure, parallel to the wire protocol the daemon already speaks.

### Option C: Both, as independent surfaces

Ship the CLI and a co-equal MCP server, each talking to the daemon directly.

**Pros:**

- Serves both audiences natively.

**Cons:**

- Two capability surfaces and — worse — **two permission-enforcement paths** to keep in sync. A gap in either is a security gap. Doubles the surface for the exact subsystem (agent authority) that most needs a single, auditable choke point.

## Decision

**Option A — the CLI (`oakterm ctl`) is the agent-control surface.** Its universal reach (any agent with a shell, plus scripts, CI, and humans) with zero per-agent setup is decisive; MCP's typed-tool advantage does not outweigh requiring an MCP client and server wiring that a large fraction of callers will never have. This matches idea 32's stance and the surface already shipped in TREK-275.

**MCP is not foreclosed, but if it is ever added it is a thin wrapper over `ctl`, never a parallel path.** An MCP server would translate tool calls into the same daemon requests `ctl` makes and inherit the same permission enforcement — so there remains exactly one capability surface and one enforcement choke point in the daemon. This preserves MCP's option value (typed tools for agents that want them) without Option C's double-enforcement hazard. No MCP work is scheduled; this is a compatibility statement, not a commitment.

## Permission and Security Model

### The security boundary is the socket; the permission model is a consent layer

The honest framing, and the answer to "is `oakterm ctl` a security risk?":

- **The `0700` per-UID socket is the actual security boundary.** It keeps _other users_ off the daemon. That boundary is real and already in place.
- **The permission model is not a boundary against a malicious same-UID process** — no same-UID mechanism can be, because that attacker can already read the pane's environment and memory directly and connect to the socket itself. The permission model is a **consent, safety, and policy layer for cooperative agents**: it constrains a semi-trusted agent that should be _told_ what it may do, requires user consent for dangerous actions, and records what happened. It defends against an agent overreaching or a script misfiring — not against an attacker who already owns the user's session.

Stating this plainly matters: it prevents the model from being sold as isolation it cannot provide, and it correctly scopes the effort — the goal is _governed cooperation and auditability_, not sandboxing a hostile local process (out of scope; unwinnable on a same-UID socket).

### Caller and pane identity

The daemon must attribute a request to a pane to enforce per-pane policy. Because identity cannot be made unforgeable against a same-UID caller (see above), it is scoped to what each action needs:

- **`self` actions** (set-status/title/color/progress on the calling pane) authenticate via the injected `$OAKTERM_PANE_ID`. The stakes are low — an agent misreporting its own pane ID only mislabels itself — so an env-based claim is acceptable here.
- **Peer credentials corroborate where cheap.** The daemon reads the connecting process's UID/PID (`SO_PEERCRED` on Linux, `LOCAL_PEERPID` + `getpeereid` on macOS) and rejects any connection whose UID is not the daemon's own — belt-and-suspenders over the `0700` socket, and a cheap sanity check that the caller is who the env says.
- **Cross-pane and creation actions do not rest on unforgeable identity** — they rest on the per-pane policy below plus explicit user consent for anything default-denied. That is the layer doing the real work, and it degrades correctly: the worst a forged identity buys is an action the user is asked to approve anyway.
- **Capability tokens are deferred.** A per-pane secret adds no boundary on a same-UID socket (the token is readable from the pane's environment by the same user). It becomes meaningful only with a cross-user or remote story, so it is left to Phase 4 remote access ([29-remote-access](../ideas/29-remote-access.md)), not shipped now.

### The policy: per-pane, default-deny, escalation

Adopt idea 32's per-pane permission table, evaluated by the daemon, sourced from Lua config ([ADR-0005](0005-lua-sandboxed-config.md)):

| Permission                                                            | Default        | Rationale                                           |
| --------------------------------------------------------------------- | -------------- | --------------------------------------------------- |
| `self` (own status/title/color)                                       | Always allowed | An agent labelling itself controls nothing          |
| `notify`                                                              | Allowed        | Passive; controls nothing                           |
| `prompt`                                                              | Allowed        | The user owns the answer                            |
| `pane_create` / `pane_read` / `pane_input` / `pane_close` / `sidebar` | Denied         | Visible actions, cross-pane I/O, or work-destroying |

A denied action **escalates** to a user prompt (Allow Once / Allow Always / Deny); "Allow Always" updates the pane's session policy. Every attempt — allowed, escalated, denied — is written to an append-only **audit trail** (in-memory, optionally `$OAKTERM_STATE_DIR/audit.log`), per [Security](../ideas/21-security.md)'s Agent Action Audit Trail.

**The six-dimension / 60-point risk scoring from idea 32 is reduced to a future refinement, not part of the initial contract.** The initial model is the simpler tiered rule: permission-class grant → auto-allow; anything default-denied → escalate. Risk scoring (rate-limiting rapid pane creation, scoring destructiveness) is a conservative enhancement layered on later once there is real usage to tune against; shipping it up front would be speculative precision on an unproven surface.

### Phasing — and the gate this lifts

1. **Shipped (TREK-275).** Thin `ctl` (`list`/`input`/`output`), local dev tool. No env injection, no policy. **Gate closed:** `OAKTERM_SOCKET` not injected, not advertised to agents. Driven manually with a matching `TMPDIR`.
2. **Daemon enforcement.** The per-pane policy table (Lua), default-deny evaluation, peer-UID check, and the append-only audit log. Still no env injection — the tool is still operator-driven — but the daemon now governs every request.
3. **Lift the gate.** Inject `$OAKTERM_SOCKET` and `$OAKTERM_PANE_ID` into pane environments, and wire escalation prompts into the GUI. **This step is gated on EPIC-11 chrome** (the palette/modal surface escalation needs). After it, agents can be pointed at the terminal safely.
4. **Fuller surface + production skill.** The rest of idea 32's `ctl` surface (`pane create`, `self`, `notify`, `prompt`, `sidebar`), the production agent skill (TREK-277), optional risk-scoring refinement, and — only if demand appears — the optional MCP wrapper.

## Consequences

- Upon acceptance, [idea 32](../ideas/32-agent-control-api.md) moves `draft → decided`, and a spec formalizes the enforced contract: the permission table shape, the escalation/audit semantics, the identity rules, and the env-injection variables. That spec also reconciles [Spec-0001](../specs/0001-daemon-wire-protocol.md)'s control-protocol framing — the `CtlCommand`/`CtlResponse` JSON envelope at `0xC8`/`0xC9`, which the shipped `ctl` bypasses in favor of the typed binary messages — so the wire spec and the implementation agree on one control surface. The fuller `ctl` command surface is specified there rather than in this ADR.
- The daemon gains a permission-enforcement layer keyed on pane identity, a peer-credential UID check on connect, and an append-only audit log — a single choke point that ACP's `session/request_permission` ([idea 39](../ideas/39-agent-protocol.md)) also routes through, so agent authority has one policy engine regardless of direction.
- The "dev-tool-until-permissions-land" gate from TREK-275 is now explicit and ordered: env injection and agent advertisement wait for Phase 3, which itself waits on EPIC-11's modal chrome. Until then `ctl` stays operator-driven.
- Security posture is documented honestly: the `0700` socket keeps other users out; the permission model governs cooperative agents and is not sandboxing against a hostile same-UID process. [Security](../ideas/21-security.md) should absorb this framing when the spec lands.
- Risk scoring and capability tokens are explicitly deferred, with the conditions that would revive them (real usage to tune against; a remote/cross-user story). Neither blocks the initial contract.
- MCP remains available as a future thin wrapper over `ctl`; choosing the CLI now does not have to be revisited to add it later.

## References

- [Agent Control API](../ideas/32-agent-control-api.md) — the `ctl` surface and the permission model this ADR decides
- [Agent Protocol (ACP)](../ideas/39-agent-protocol.md) — inverse direction; consumes this permission substrate
- [Agent Management](../ideas/07-agent-management.md) — agent lifecycle that launches the panes governed here
- [Security](../ideas/21-security.md) — permission principles and the agent audit trail
- [ADR-0007 Daemon Architecture](0007-daemon-architecture.md) — the socket and server/client split
- [ADR-0005 Lua Sandboxed Config](0005-lua-sandboxed-config.md) — config language for the permission table
- [Spec-0001 Daemon Wire Protocol](../specs/0001-daemon-wire-protocol.md) — socket, handshake, message catalog
- TREK-275 — the shipped thin `ctl` slice this contract builds on
- TREK-277 — the production agent skill this ADR unblocks
