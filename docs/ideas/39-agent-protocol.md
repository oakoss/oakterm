---
title: 'Agent Protocol (ACP)'
status: draft
category: core
description: 'Speak the Agent Client Protocol so any ACP-compatible agent (Claude, Codex, Gemini, OpenCode) lights up the terminal with structured prompts, tool calls, and permissions'
tags: ['agents', 'acp', 'claude-code', 'codex', 'gemini', 'opencode']
---

# Agent Protocol (ACP)

Adopt the [Agent Client Protocol](https://agentclientprotocol.com) so oakterm acts as a first-class ACP client. Any agent that speaks ACP — Claude, Codex, Gemini, OpenCode, future agents — works in the terminal with structured streaming, native tool-call rendering, permission prompts, and slash commands. The terminal becomes a vendor-neutral surface for agent interaction instead of a fancy launcher for one CLI tool at a time.

## Problem / Why

Agents today are growing structured surfaces: streaming text, tool calls with arguments and results, permission requests before destructive actions, file edits with diffs, slash commands, plan/todo updates. Every agent CLI invents its own conventions for these. From inside a terminal, all of that arrives as opaque ANSI-painted text — useful, but unrenderable as native UI affordances and unreusable across vendors.

Three consequences for oakterm:

1. **Per-agent integration cost.** Wiring "first-class Claude" + "first-class Codex" + "first-class Gemini" each requires reverse-engineering a different output format. With the input classifier landing (ADR-0014), this cost grows linearly per supported agent.
2. **Lost UX leverage.** A tool-call event painted as text can't trigger a permission modal, a diff pane, or an accessibility announcement. The terminal sees lines, not events.
3. **Plugin ecosystem fragmentation.** Phase 2 plugins ([Plugin System](06-plugins.md)) that want to participate in agent flows — exposing context, registering tools, intercepting permissions — would have to integrate per-agent, not per-protocol.

ACP is the editor-side equivalent of LSP for agents. Solving these once at the protocol layer scales linearly per agent instead of multiplying per (agent, feature).

## What ACP Is

Open protocol from Zed for connecting code editors to coding agents. JSON-RPC over stdio (HTTP/WebSocket transport in progress). Capability-negotiated, versioned. Spec at v0.12.x as of April 2026, actively maintained in the [agentclientprotocol](https://github.com/agentclientprotocol) GitHub org.

**Core method shapes:**

- `initialize` — version + capability handshake
- `session/new`, `session/load`, `session/prompt`, `session/cancel` — session lifecycle
- `session/update` (notification, streaming) — discriminated by `sessionUpdate`: `agent_message_chunk`, `tool_call`, `plan`, others
- `session/request_permission` — agent asks the client before privileged actions
- `fs/read_text_file`, `fs/write_text_file` — agent-asks, client-applies file ops (full-content replacement, not diffs)
- `terminal/create`, `terminal/output`, `terminal/wait_for_exit`, `terminal/kill`, `terminal/release` — agent-driven shell command execution through the client

**Capability negotiation.** Client capabilities include `terminal: true`, `readTextFile`, `writeTextFile`. Agent capabilities include session loading, content types, MCP server transports. Missing capabilities are treated as unsupported; new capabilities are non-breaking additions.

**Reference implementations.** Official SDKs in Rust, TypeScript, Python, Java, Kotlin. The Rust crate (`agent-client-protocol`, Tokio-based) is the natural fit for oakterm.

## Why oakterm Is Uniquely Good for This

The `terminal/*` capability is the single biggest piece of leverage. Every existing ACP client is an editor that has to _fake_ a terminal — embedded mini-shells, reduced PTY semantics, no scrollback, no GPU rendering. We _are_ a terminal. When an agent calls `terminal/create`, we hand it a real pane: full PTY, real scrollback, GPU-accelerated rendering, plugin hooks, accessibility tree. None of the existing clients can match that.

For each ACP primitive:

| Primitive                                 | Editor client today             | oakterm                                                                                                                                 |
| ----------------------------------------- | ------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `terminal/create`                         | Embedded mini-terminal widget   | Native pane in the existing layout tree                                                                                                 |
| `session/update tool_call`                | Inline expandable in chat panel | Status badge + collapsible block in the agent pane                                                                                      |
| `session/request_permission`              | Modal dialog                    | Native command-palette-style prompt; defaults driven by the per-pane permission model from [Agent Control API](32-agent-control-api.md) |
| `fs/write_text_file`                      | Inline diff in editor           | Floating diff pane via the user's configured difftool                                                                                   |
| Slash commands (`/login`, `/clear`, etc.) | Editor command palette          | Routed through the existing `:` palette ([Configuration](09-config.md), [Conventions](30-conventions.md))                               |
| Plan / TODO updates                       | Sidebar list                    | Sidebar entry under the pane ([Sidebar](04-sidebar.md))                                                                                 |

Agents drive the terminal that already exists. We don't bolt a chat UI on top.

## Multi-Agent Portfolio

ACP is one protocol; the agent ecosystem behind it is plural. Today's known servers:

- **Claude** — via `@agentclientprotocol/claude-agent-acp` (npm)
- **Codex CLI** — via Zed's adapter
- **Gemini CLI** — listed in the agent registry
- **OpenCode** — listed
- **GitHub Copilot** — public preview

Each agent owns its own auth, billing, and capabilities. oakterm doesn't pick winners. A user with Claude Pro picks Claude; a user with ChatGPT Plus picks Codex; a user in the Google ecosystem picks Gemini; a user who wants fully local picks OpenCode; a user paying per-token plugs in their API key.

The protocol bet is also a portfolio bet. The product is the agent surface in the terminal, not "AI in the terminal" — and ACP keeps that surface vendor-neutral.

It also forecloses marketing and policy traps. We never claim to be a Claude product or a ChatGPT product. We're a terminal that runs agents the user has configured.

## How It Fits With Existing oakterm Architecture

ACP slots between three existing concerns:

```text
                ┌──────────────────────────────────────────────────┐
                │                  oakterm core                    │
                │                                                  │
   user input ──┤  ADR-0014 input classifier                       │
                │      ├─ shell    → existing PTY child            │
                │      └─ ai mode  → ACP client (this doc)         │
                │                                                  │
                │  Pane lifecycle (07-agent-management)            │
                │  Per-pane permissions (32-agent-control-api)     │
                │  Plugin host (06-plugins)                        │
                │                                                  │
                └──────────────────┬───────────────────────────────┘
                                   │  ACP (JSON-RPC over stdio)
                                   ▼
                        ┌─────────────────────┐
                        │  configured agent   │
                        │  (subprocess)       │
                        └─────────────────────┘
```

**Relationship to [Input Classifier](../adrs/0014-input-classifier.md).** When the classifier routes input as AI rather than shell, the prompt is sent to whatever ACP agent the user has configured for the current pane (or workspace). The classifier is the gate; ACP is what's downstream.

**Relationship to [Agent Management](07-agent-management.md).** Idea 07 is about _lifecycle_ — worktrees, status badges, scroll pinning, merge/diff. It treats agents as opaque CLI processes. ACP doesn't replace any of that; it adds a _channel_. An ACP-aware agent pane gets:

- Structured status (no output-pattern guessing) from `session/update` events
- Tool-call rendering and permission prompts as native UI
- Slash commands routed through the `:` palette

A plain CLI agent without ACP support still works — it's just opaque output. ACP is opt-in per agent.

**Relationship to [Agent Control API](32-agent-control-api.md).** Idea 32 is the _inverse_ direction: agent → terminal control via `oakterm ctl`. ACP is terminal → agent in a structured way. The two are complementary:

- `oakterm ctl` lets any process (including non-ACP agents) set status, open panes, prompt the user.
- ACP gives ACP-aware agents a structured channel into the terminal's UI affordances without needing a separate CLI.

The permission model in idea 32 (per-pane, risk-scored, escalation prompts) is the policy substrate ACP's `session/request_permission` plugs into. ACP supplies the request; idea 32's machinery decides how to handle it.

**Relationship to [Plugin System](06-plugins.md).** Plugins exposing context as MCP servers (which ACP already speaks) become consumable by _any_ configured agent — one plugin, N agents. The Phase 3 [Context Engine](05-context-engine.md) follows the same pattern.

## Configuration Sketch

Extends the existing `agent_providers` shape from [Agent Management](07-agent-management.md):

```lua
agent_providers = {
  -- Plain CLI: opaque PTY child, output painted as ANSI text.
  aider = {
    command = "aider",
  },

  -- ACP-aware: structured channel, native tool-call rendering, permission prompts.
  claude = {
    command = { "npx", "@agentclientprotocol/claude-agent-acp" },
    protocol = "acp",
  },
  codex = {
    command = { "codex", "acp" },
    protocol = "acp",
  },
  gemini = {
    command = { "gemini", "acp" },
    protocol = "acp",
  },
  opencode = {
    command = { "opencode", "acp" },
    protocol = "acp",
  },
}

-- Default agent for AI-classified input. Per-workspace override allowed.
default_agent = "claude"
```

Agents inherit the user's shell environment by default. Per-agent overrides are supported but not required:

```lua
agent_providers.claude.env = {
  ANTHROPIC_API_KEY = os.getenv("ANTHROPIC_API_KEY"),
}
```

## Auth Posture

This is a constraint, not an open question. Anthropic's [published policy on the Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview) reads:

> "Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK. Please use the API key authentication methods described in this document instead."

The author of [Sandcastle](https://github.com/mattpocock/sandcastle) hit this same wall in [issue #191](https://github.com/mattpocock/sandcastle/issues/191) and has been unable to obtain a written exception. Until Anthropic publishes a clear approval channel, oakterm operates on the following stance:

**What oakterm ships and documents:**

- API key auth (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.) — unambiguously sanctioned.
- Bedrock / Vertex / Azure (`CLAUDE_CODE_USE_BEDROCK=1`, etc.) — unambiguously sanctioned.
- Environment passthrough to spawned agents — same as any other terminal does for any other tool.

**What oakterm explicitly does not do:**

- No `/login` UI built into oakterm.
- No `CLAUDE_CODE_OAUTH_TOKEN` plumbing in our config or onboarding.
- No "use your Claude subscription" copy in README, marketing, or default presets.
- No subscription presets shipped in the oakterm repo.

**What still works for users with subscription auth:**

A user who has run `claude setup-token` themselves and exported `CLAUDE_CODE_OAUTH_TOKEN` in their shell environment gets that token passed to the spawned agent like any other env var. oakterm is being a terminal; the user authenticated their own tool. The user owns that configuration; we don't facilitate it.

This mirrors how every terminal handles every other authenticated tool — iTerm2 doesn't ship "use your AWS root account" copy; it just runs `aws` with the user's environment.

**Revisit trigger.** If Anthropic publishes a clear approval process or amended policy, revisit. Until then, status quo.

## Prior Art

**[Warp](https://www.warp.dev) (gateway pattern, off-limits to us).** Warp solved the same auth surface area by becoming the merchant of record. They proxy LLM calls through their own backend, bill in credits ($18-$180/mo plans), and offer BYO-API-key as the escape hatch. Subscription auth (claude.ai, ChatGPT Plus) is not supported even in Warp — same Anthropic policy applies. Their approach requires a SaaS gateway, which is unavailable to a non-commercial MPL-2.0 project. See [Warp Architecture Review](../reviews/2026-04-28-210532-warp-architecture-review.md) for crate-level breakdown of the gateway client and the AGPL constraint on borrowing it.

Warp also enumerates the same agent portfolio at the _skill-discovery_ layer: their `SkillProvider` enum lists `Warp, Agents, Claude, Codex, Cursor, Gemini, Copilot, Droid, Github, OpenCode`, mapped to filesystem conventions. Two independent teams converged on "agents are plural"; we pick the protocol route, Warp picked the gateway route.

**[Zed](https://zed.dev/docs/ai/external-agents) (reference ACP client).** Zed's external-agent panel launches `claude-agent-acp` as a subprocess, exposes `/login` for Pro/Max OAuth via the adapter, and decoupled this from Zed's own AI subscription as of v0.202.7. Their auth flow works _in practice_ but rests on whatever private arrangement (if any) exists between Zed and Anthropic — not a published exception. We can't safely assume downstream clients of the same adapter inherit any approval.

**[Sandcastle](https://github.com/mattpocock/sandcastle) (auth gray zone, in writing).** Issue #191 documents the author's months-long unsuccessful attempt to get a written position from Anthropic on subscription auth in third-party tools. His public stance: "Anthropic publicly documents how to do this in Claude Code itself, via `claude setup-token` and `CLAUDE_CODE_OAUTH_TOKEN`. I can't legally recommend you use it. But you are able to do it." This is the same posture oakterm adopts.

## Open Questions

1. **Where does the ACP client live in the layout tree?** A pane that is "an ACP session" is structurally different from a pane that is a PTY. Does it occupy the same pane primitive with a different render path? A new pane type? See [Spec-0007](../specs/0007-pane-tree-layout.md) for the existing pane model.

2. **How do `fs/write_text_file` operations relate to the user's working tree?** ACP assumes "client applies edits." For an editor that means writing to the open buffer; for a terminal it could mean writing directly to disk, proxying to a paired editor (Helix, Neovim, VS Code), or refusing the capability and forcing agents to terminal-only mode. Each choice has very different security and UX implications.

3. **Diff display.** `fs/write_text_file` passes whole-file content. We compute the diff client-side for display. What's the rendering primitive — floating pane via `:diff` (idea 07), inline overlay, or sidebar entry?

4. **Plan / TODO rendering.** `session/update plan` events are list-shaped. Sidebar section ([Sidebar](04-sidebar.md)) is the natural home, but there's a UX question about per-pane vs. workspace-level surfacing.

5. **Sandbox boundary for `terminal/*`.** Letting an agent run _anything_ in the user's shell is the largest security surface in this design. The per-pane permission model from [Agent Control API](32-agent-control-api.md) plus the risk scoring from idea 32 is the substrate, but ACP-driven `terminal/create` calls need explicit policy: which commands need approval, which auto-approve within a permission class, which always escalate. See also [Security](21-security.md).

6. **Multi-agent in one pane vs. one-agent-per-pane.** The Rust SDK exposes `Conductor` and `Proxy` types. Do we use those to host multiple concurrent ACP sessions per pane (e.g. a coordinator agent that delegates to specialists), or stay one-session-per-pane and rely on idea 07's worktree-per-agent fan-out for multi-agent work?

7. **Protocol version-pin policy.** ACP is at v0.12.x with active churn. Do we track tip on every adapter release, pin a minor version and bump deliberately, or vendor the crate to control breakage timing? Each choice trades update cadence against test surface.

8. **HTTP/WebSocket transport.** The spec lists remote ACP as in-progress. Phase 4 ([Networking](29-remote-access.md)) might want to consume this; today, stdio-only is fine.

9. **AGPL contagion via agents.** Some agents (notably any forks of the AGPL-licensed Warp internals) might themselves be AGPL. Spawning an AGPL subprocess is generally considered safe (no linking), but documenting this for community-contributed agent presets is worth doing.

## What This Is Not

- **Not a chat UI.** ACP renders into the existing pane primitive. We're not building a conversational sidebar or split-with-input-box editor surface.
- **Not a proxy or gateway.** oakterm does not host or relay LLM calls. The agent subprocess talks to its own provider.
- **Not vendor-coupled.** The doc names Claude/Codex/Gemini/OpenCode for concreteness; nothing in core is Claude-specific. Agent identity is config.
- **Not auth infrastructure.** No `/login`, no token storage, no OAuth flows shipped in oakterm. The user's environment is the user's.
- **Not a replacement for [Agent Management](07-agent-management.md).** Lifecycle, worktrees, merge/diff, scroll pinning all still come from that plugin. ACP adds a structured channel; idea 07 manages the process.
- **Not a replacement for [Agent Control API](32-agent-control-api.md).** Plain-CLI agents and user scripts still use `oakterm ctl`. ACP is one of two complementary directions, not a replacement.
- **Not coupled to Anthropic.** The auth posture above keeps oakterm independent of any single vendor's policy.

## Related Docs

- [Agent Management](07-agent-management.md) — agent process lifecycle, worktrees, status, merge/diff
- [Agent Control API](32-agent-control-api.md) — inverse direction: agent → terminal via `oakterm ctl`
- [Plugin System](06-plugins.md) — primitives plugins use to participate in agent flows
- [Context Engine](05-context-engine.md) — Phase 3 context surface; ACP is one consumer
- [Sidebar](04-sidebar.md) — where agent status, plans, and tool calls render
- [Security](21-security.md) — permission model principles for agent-driven actions
- [Configuration](09-config.md) — config syntax for `agent_providers`
- [Conventions](30-conventions.md) — naming and palette command conventions
- [ADR-0014: Input Classifier](../adrs/0014-input-classifier.md) — what routes input to ACP in the first place
- [ADR-0005: Lua Sandboxed Config](../adrs/0005-lua-sandboxed-config.md) — config language used in examples
- [Warp Architecture Review](../reviews/2026-04-28-210532-warp-architecture-review.md) — gateway-pattern prior art and AGPL constraint
