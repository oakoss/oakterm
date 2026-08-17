---
title: Agent Tooling Landscape Audit
date: 2026-08-16T21:57:31
scope: 'competitive landscape of agent-adjacent tooling (11 products) + verification of Anthropic auth/billing policy'
---

# Agent Tooling Landscape Audit

## Scope

Deep-dive review of eleven agent-adjacent tools (bb, Zeron, Oh My Pi, ZCode, Atomic, Shep, Pi, Hermes, OpenCode, T3 Code, Orca), plus source-level verification of Orca and live verification of Anthropic's 2026 auth/billing policy changes. Purpose: check oakterm's agent-integration decisions (ADR-0021, ideas 07/32/39, remote-access plans) against where the ecosystem actually moved, and refresh the stale claims those docs carry.

Method: six parallel research passes over product sites, GitHub APIs, npm registry data, and third-party coverage; a shallow clone and grep of Orca's source; live fetches of Anthropic support/docs pages. Point-in-time snapshot — star counts and policy statements are as of 2026-08-16.

## The Landscape

None of the eleven implements terminal emulation. The wrapper layer embeds xterm.js (Shep), portable-pty panes (Zeron), or spawns CLIs; the agents themselves talk to provider APIs directly. Seven of the eleven stack into layers around the agent:

| Layer                | Products                                                                       | Integration mechanism                                                     |
| -------------------- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------- |
| Steer & observe      | Zeron (cross-device pilot), Shep (per-repo dashboard), Orca (worktree cockpit) | Zeron: stream-json + Codex app-server; Shep/Orca: interactive CLI in PTY  |
| Orchestrate & verify | bb (manager/worker agent IDE), Atomic (workflow runtime)                       | bb: Claude Agent SDK over local CLI; Atomic: Claude/Copilot/OpenCode SDKs |
| The agent itself     | Oh My Pi (25.3k★, fork of Pi), ZCode (Z.ai's GLM-first ADE)                    | Direct provider APIs                                                      |

Traction is wildly asymmetric among the six deep-dive subjects (bb, Zeron, Oh My Pi, ZCode, Atomic, Shep): Oh My Pi (25.3k★) has ~7× the stars of the other four open-source subjects combined — bb 2.1k★, Zeron 885★ in ~4 weeks, Atomic 616★, Shep 82★ (Orca, stablyai/orca, sits mid-hundreds). ZCode substitutes corporate backing (HKEX-listed Z.ai) for community. Of those six, all but ZCode are MIT + free + bring-your-own-subscription; ZCode is the proprietary, paid exception. Nobody charges for the wrapper layer.

The remaining four (Pi, Hermes, OpenCode, T3 Code) entered the survey through the auth/billing thread: Pi and OpenCode as subscription-auth case studies, Hermes (Nous Research's self-hosted autonomous agent) confirmed API-key/OpenRouter-only with no Claude subscription path, and T3 Code as the wrapper with explicit Anthropic blessing. Orca additionally received source-level verification of its integration mechanics.

Convergent features across independent wrappers: run many agent sessions per project, live branch diffs, remote/mobile steering, session resume, usage meters. Remote steering from a phone exists in three products (Zeron, ZCode Bot Channel, Orca mobile companion) — the strongest demand signal in the set.

## Findings

### Corrections

Fixed directly alongside this review:

1. **`ideas/29-remote-access.md` was stale against ADR-0007.** ADR-0007's Consequences section required the doc to state that remote access reuses the daemon wire protocol (Spec-0001) over WebSocket; the doc still described a bespoke WebSocket message format. It also showed a dotted `remote-domain.homelab.host` config syntax contradicting both its own Lua example and ADR-0005's snake_case convention, and used `remote-allow-interactive` (dashed) in the Security section.
2. **`ideas/39-agent-protocol.md` landscape facts were four months stale.** ACP is no longer "v0.12.x as of April 2026": the protocol reached **stable version 1**, with `session/resume`, `session/close`, `logout`, and `session_info_update` stabilized. Codex is no longer "via Zed's adapter" — an official `codex-acp` server exists (updated 2026-08-15). Copilot's ACP support went public preview 2026-01-28. The auth-posture section rested on the Sandcastle-limbo picture, superseded by the policy timeline below.
3. **`ideas/07-agent-management.md` scrollback claim re-verified — still true.** Claude Code's scrollback breakage (jump-to-top/auto-scroll) remains open upstream (anthropics/claude-code #34845, #36816, #37627); Anthropic attributes it to Ink's ANSI handling and is rewriting the renderer. Scroll pinning's motivation stands; citations added.
4. **Spec-0011 frontmatter/index listed only ADR-0016.** The spec's own body is structured around the ADR-0007 daemon/client boundary; `0007` added to its `adrs` list and the specs index row.

### Validated Decisions

- **[ADR-0007](../adrs/0007-daemon-architecture.md) (daemon/client split).** Zeron performed a ground-up Electron→Rust/GPUI rewrite specifically to get a single binary with headed and headless modes — the architecture oakterm has natively. The remote-attach model (client renders, daemon computes) matches what Zeron/Orca/ZCode all converged on.
- **[ADR-0017](../adrs/0017-rust-implementation-language.md)/[ADR-0018](../adrs/0018-gpu-rendering-wgpu.md) (Rust, GPU rendering).** The wrapper layer is going Rust-native and explicitly anti-Electron: Zeron on GPUI, Shep on Tauri ("not another Electron app"), Oh My Pi's ~80k-LOC Rust core. bb is the Electron holdout.
- **[ADR-0021](../adrs/0021-agent-control-api.md) (CLI over daemon socket for agent control).** bb ships a CLI as a first-class surface; Orca's entire agent interface is `orca <command>` over a local runtime; herdr converged the same way ([herdr review](2026-05-06-164003-herdr-architecture-review.md)). Three independent confirmations that CLI-over-socket beats MCP for reach.
- **Auth posture ([idea 39](../ideas/39-agent-protocol.md)) — validated and strengthened.** See policy timeline. The interactive-PTY pattern oakterm inherently uses is the _most_ sanctioned integration pattern in the ecosystem, with explicit public confirmation (T3 Code) that wrapping the local Claude Code CLI is allowed.
- **Composition stance ([herdr review](2026-05-06-164003-herdr-architecture-review.md)).** The entire 2026 wave builds _around_ terminals, not terminals. The core layer (VT parsing, GPU rendering, PTY mux) is uncontested; the earlier "compose, don't merge" conclusion holds.
- **Web-client monitor-mode default ([idea 29](../ideas/29-remote-access.md)).** Zeron's mobile clients are monitor-first with opt-in interaction — independent convergence on the `remote_allow_interactive = false` default.

### Challenged Decisions

1. **Idea 39's premise that ACP is the only escape from per-(agent, feature) integration cost.** No doc in the tree mentions Claude Code's stream-json interface, the Claude Agent SDK, or Codex's app-server JSON-RPC — yet those are what shipping wrappers actually integrate against: Zeron drives Claude Code via stream-json and Codex via app-server; bb and T3 Code drive the local Claude Code CLI through the Agent SDK; Atomic embeds three vendors' SDKs. The Claude ACP adapter is itself a wrapper over the same Agent SDK. ACP simultaneously got stronger (stable v1, official codex-acp, Copilot preview). The real decision is not "ACP: yes/no" but which protocol surface(s) oakterm speaks and with what precedence. **ADR candidate: agent integration protocol surface (proposed as ADR-0022).**
2. **Agent state vocabulary and state-source precedence (parked in TREK-184 since the herdr review).** Now corroborated beyond herdr: Shep ships provider-aware status indicators; Zeron surfaces per-session state; Orca tracks agent liveness per terminal. Three shipping products display agent state that oakterm's docs still leave as an idea-07/idea-39 ambiguity (heuristics vs protocol-authoritative). **ADR candidates: state vocabulary (ADR-0023), state-source precedence (ADR-0024).**
3. **Usage/cost meter sourcing (currently only in [31-brainstorm](../ideas/31-brainstorm.md)).** Orca demonstrates the anti-pattern in production: its primary rate-limit fetcher reads Claude Code's OAuth token from the macOS Keychain, calls `api.anthropic.com/api/oauth/usage`, and performs its own token refresh against `platform.claude.com/v1/oauth/token` — third-party handling of subscription credentials, even for a read-only endpoint. Its _fallback_ is the clean pattern: spawn a hidden `claude`, send `/usage`, parse the TUI — data from PTY-owned output. A terminal uniquely gets the clean method for free. **Decision to record (proposed as a constraint in ADR-0024): agent usage surfaces read from PTY output or agent self-report (`oakterm ctl self set-badge`); never from provider credential stores.**

### Missing Specs

- **Spec-0012 (Agent Control API)** — already owed by [ADR-0021](../adrs/0021-agent-control-api.md)'s Consequences (TREK-278), including the reconciliation between [Spec-0001](../specs/0001-daemon-wire-protocol.md)'s `CtlCommand`/`CtlResponse` envelope (0xC8/0xC9) and the typed messages TREK-275 shipped. The herdr-derived primitives (TREK-183: `wait`, the three-way input split, `read --source`, dual IDs, pane `release`, and the `SKILL.md` deliverable) belong in it, gated on ADR-0023/0024.
- **Remote protocol spec** — deliberately _not_ needed yet. [ADR-0007](../adrs/0007-daemon-architecture.md) plus [Spec-0001](../specs/0001-daemon-wire-protocol.md)'s reserved compression bit cover Phase 4; this review only strengthens the priority of Phase 4, not its schedule.

## Verified Policy Timeline (Anthropic, 2026)

The load-bearing facts behind the auth posture, each verified against primary or near-primary sources on 2026-08-16:

| Date       | Event                                                                                                                                                                                                                                                                                                        |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Jan 2026   | Anthropic blocks third-party tools from using subscription credentials; OpenCode breaks for Max users                                                                                                                                                                                                        |
| Mar 2026   | OpenCode removes Anthropic references after a legal request; docs now state Pro/Max in OpenCode is unsupported, API key only                                                                                                                                                                                 |
| 2026-04-04 | Reclassification: third-party apps authenticating with a Claude subscription work, but draw from claude.ai "extra usage," billed per token — not plan limits (Pi's `/login` uses this path)                                                                                                                  |
| 2026-05-14 | Anthropic announces Agent SDK / `claude -p` usage will move to a separate capped monthly credit on June 15                                                                                                                                                                                                   |
| 2026-06-15 | That change is **paused** before taking effect; Agent SDK, `claude -p`, and third-party apps built on the Agent SDK still draw from subscription limits ("for now"), per Anthropic's support article. Independent subscription OAuth (tier 3 below) is unaffected — it stays on the April 4 extra-usage path |
| 2026       | T3 Code receives explicit public confirmation from Anthropic that tools wrapping the local Claude Code CLI are allowed; contrast drawn with OpenCode's independent OAuth flows                                                                                                                               |

Resulting taxonomy, most- to least-sanctioned: (1) spawn the interactive official CLI in a PTY — Shep, Orca, and inherently oakterm; (2) drive the local official CLI via Agent SDK/stream-json — T3 Code (explicitly blessed), bb, Zeron; (3) third-party subscription OAuth via the sanctioned extra-usage path — Pi's `/login`; (4) token scraping/impersonation of the official client — `pi-claude-auth`, old OpenCode — ToS-violating, enforcement-tested. oakterm's env-passthrough posture sits in tier 1 by construction. The posture's _revisit trigger_ ("Anthropic publishes a clearer policy") has partially fired — and the clearer policy confirms the posture rather than loosening it.

## Action Items

**Corrections (done with this review):** 29-remote-access protocol/config fixes; 39-agent-protocol landscape + auth-posture refresh; 07-agent-management scrollback citations; Spec-0011 ADR listing.

**ADRs (proposed, in order):**

1. ADR-0022 — Agent integration protocol surface (resolves [idea 39](../ideas/39-agent-protocol.md); recommend layered: ACP as the abstraction, native transports as first-class adapters behind one internal trait; settles version-pin policy).
2. ADR-0023 — Agent state vocabulary (five states incl. done→idle transition; un-parks the state-vocabulary item from TREK-184).
3. ADR-0024 — Agent state-source precedence (process identity → protocol/hook state → heuristics; includes the usage-meter sourcing constraint; un-parks the state-source item from TREK-184). TREK-184's six other parked decisions (skill-provider precedence, project-rule discovery, agent-action protocol, floating/drawer/popup model, OSC 52 ownership, multi-client focus arbitration) stay parked.

**Specs (after ADRs):** Spec-0012 Agent Control API (TREK-278) + Spec-0001 control-surface reconciliation.

**Idea docs (gated on ADRs):** TREK-183 edits (five-state vocabulary into [04-sidebar](../ideas/04-sidebar.md); all six [32-agent-control-api](../ideas/32-agent-control-api.md) updates listed in that task; the non-ACP state-source fallback section into [39-agent-protocol](../ideas/39-agent-protocol.md)); promote [31-brainstorm](../ideas/31-brainstorm.md)'s "Agent Cost/Usage Visibility" into 04-sidebar; flip 39 (moved to `reviewing` with this audit) to `decided` on ADR-0022 acceptance.

**Trekker:** file one task per ADR; keep TREK-278; close TREK-183 when its edits are absorbed; trim TREK-184 to its six remaining parked decisions rather than closing it.
