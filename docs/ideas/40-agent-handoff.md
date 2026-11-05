---
title: 'Agent Handoff'
status: draft
category: core
description: 'Hand running agent sessions from the laptop to an always-on host before the lid closes, and back in the morning'
tags: ['agents', 'remote', 'handoff', 'sessions', 'acp']
---

# Agent Handoff

Move agent sessions off the laptop at bedtime so they keep working on an always-on host, then bring them back.

## Problem / Why

Agent sessions now run for hours. Today they run on the laptop, so closing the lid either stops them or needs a keep-awake tool that holds a closed MacBook awake overnight, often in a bag, with the heat and battery drain that implies. The user wants to close the laptop and have the agents carry on.

Moving the running process is not an option: a live PTY is tied to one kernel's process, file-descriptor and socket state, and no practical tool moves it off macOS or between operating systems (CRIU and DMTCP checkpoint processes only between Linux hosts). What can move is the work: the repository state and the agent's conversation.

## How It Works

### Placement

oakterm can talk to more than one daemon ([Remote Access](29-remote-access.md)): the local one and one or more always-on hosts. Every agent pane has a host. Where a session can start on the always-on host, it should: closing the lid then needs nothing, because the work was never on the laptop. Handoff covers the sessions that started locally.

### The bedtime handoff

One action, "Hand off agents", from the palette (`:handoff`), a keybind (`handoff-agents`), or an offer when oakterm sees the system about to sleep:

1. **Wait for a safe point.** Each agent pane finishes its current turn; a long turn can be interrupted instead. Every product surveyed hands off between turns, never during one.
2. **Move the repository.** Uncommitted changes go across as a WIP ref (or a git bundle when the host has no shared remote), and the host checks out the same branch at the same path.
3. **Move the conversation.** The agent's session data has to reach the host first, then the host's agent resumes it. Where the agent has its own transfer (Codex hands a chat between hosts itself), oakterm triggers that. Otherwise the agent's session transcripts are synced to the host, never its credential files, and the host's agent resumes them: ACP `session/resume` reconnects to a session the host's agent can already restore ([ADR-0022](../adrs/0022-agent-integration-protocol.md)), and some agents have a native resume. oakterm never carries credentials: each host has its own agent login, following the auth posture in [Agent Protocol](39-agent-protocol.md) and [ADR-0024](../adrs/0024-agent-state-sources.md).
4. **Swap the pane.** The same pane, in the same tab and position, now shows the remote session. The laptop sleeps.

Before the laptop sleeps, oakterm shows the outcome per pane: moved, parked (below), or already remote.

### Continuous sync

A handoff is slow when everything ships at the end. While an agent pane runs locally with a handoff host configured, oakterm keeps the host current in the background: WIP refs for the repository and, where the agent allows it, its session transcripts as they grow (never its credential files). At bedtime only the last few seconds of changes remain, so the wait is roughly "finish the current turn" (an estimate; nothing has been measured).

### Guard and park

A host may lack something a session needs: a toolchain, an operating system, a secret. Each daemon advertises what it offers, and a session's needs are inferred from the project or set by the user. A session the host cannot run is **parked** instead: paused at a turn boundary, checkpointed, and resumed in the same pane when the laptop wakes. Nothing is half-moved, and the user sees which panes will keep working overnight before closing the lid.

### The morning

The remote panes are still in their tabs. The user keeps working against the host, or hands a session back with the same flow in reverse.

### What does not move

Processes the agent started in the pane, such as dev servers, watchers and background jobs, stay behind and stop with the laptop. Only the agent's conversation and the repository move. The handoff summary lists what was running so it can be restarted on the host.

## Reference Setup

An always-on Linux VM on a home server (for example a Proxmox guest) reached over Tailscale or another tunnel:

- toolchains installed, repositories cloned at the same paths as on the laptop;
- the agents installed and logged in with their own credentials;
- the oakterm headless daemon running ([Remote Access](29-remote-access.md), Headless Daemon section).

A VM rather than a container keeps Docker and full toolchains behaving normally. A session that needs another operating system needs a host that runs it; the guard parks it otherwise.

## Configuration

```lua
agent_handoff = {
  host = "homelab",             -- a name from remote_domains (Remote Access)
  continuous_sync = true,       -- keep the host current while agents run locally
  on_sleep = "ask",             -- "ask" | "handoff" | "park" | "off"
  interrupt_after_seconds = 60, -- stop waiting for a turn to end after this long
}
```

## Primitives Used

- **Remote daemons and remote panes** from [Remote Access](29-remote-access.md) (Phase 4).
- **Agent session lifecycle**, needed from the agent-session spec (OT-300, not yet written): turn boundaries, pause, and resume. ACP is the only transport [ADR-0022](../adrs/0022-agent-integration-protocol.md) builds now. The default Claude pane is an opaque PTY, so resuming it means relaunching the CLI's own resume on the host, or the stream-json transport ADR-0022 pre-designates for Claude.
- **Agent control surface** ([ADR-0021](../adrs/0021-agent-control-api.md)): `oakterm ctl` exposes handoff and park so scripts and agents can trigger them.
- **Session persistence** ([Spec-0010](../specs/0010-session-persistence.md)) keeps the pane's place in the layout. It saves no agent session or checkpoint, so recording a parked session is an extension it would need.

## Prior Art

Surveyed 2026-11-05. No product found does the whole flow; each piece exists somewhere.

- **Process stays on its host, the UI moves:** tmux, [Zellij](https://zellij.dev/documentation/web-client.html), [WezTerm domains](https://wezterm.org/multiplexing.html), Rex, [Herdr](https://github.com/herdrdev/herdr) (which also resumes supported agent sessions after a restart), and Claude Code [Remote Control](https://code.claude.com/docs/en/remote-control). These survive closing the lid only when the host is always on, and the host is fixed when the session starts.
- **Agent work sent to a cloud agent:** [Warp handoff](https://docs.warp.dev/agent-platform/cloud-agents/handoff/local-to-cloud.md) forks the local conversation and snapshots uncommitted changes into a cloud run. Claude Code [cloud sessions](https://code.claude.com/docs/en/claude-code-on-the-web) start new sessions in the cloud and teleport them back to the terminal; from the CLI that handoff is one-way, cloud to local. Cursor and GitHub Copilot also delegate to cloud agents (not checked against their docs).
- **The same conversation moved between the user's own hosts:** Codex [remote connections](https://learn.chatgpt.com/docs/remote-connections), in both directions, interrupting a running reply first. The closest match to this design.
- **Work sent to a chosen host from the start:** [T3 Code](https://github.com/pingdotgg/t3code) can run its server as a background service, dispatches tasks to other machines over peer links when asked, and carries a budgeted selection of history when switching agents rather than copying session files. It never moves a running session between hosts ([T3 Code source review](../reviews/2026-11-05-075636-t3code-source-review.md)).
- **Resume in the protocol:** ACP [`session/resume`](https://agentclientprotocol.com/announcements/session-resume-stabilized), stabilized in 2026.

Recurring patterns: git carries the repository, handoff happens between turns, running processes never move, and secrets stay on each host. Not found in this survey: automatic placement by host capability, continuous background sync for an instant handoff, and one action that decides per session whether to move, park, or leave it.

## Open Questions

1. How is a turn boundary detected for an agent that speaks no protocol, such as the default interactive Claude pane? Candidates: OSC 7501 program status, agent hooks, or an idle heuristic.
2. Should continuous sync push WIP refs to the shared remote, or sync directly to the host so nothing leaves the user's machines?
3. How are session needs declared: inferred from project files, a per-project config key, or both?
4. What happens when the host is unreachable at bedtime: park everything, or let the user choose?
5. Where does a handed-off conversation's history live afterwards, and how does the returning handoff avoid two diverging copies?
6. How does an agent's session data reach the host when the agent has no transfer of its own, and may oakterm copy an agent's private session files at all, given that they can hold secrets?

## What This Is Not

- **Not live process migration.** Running processes stay where they started.
- **Not a cloud service.** The host is the user's own machine; oakterm runs no infrastructure.
- **Not a credential broker.** oakterm never reads, copies, or forwards agent credentials; each host is logged in on its own. Syncing session transcripts excludes the credential files agents keep beside them.
- **Not a sync tool for general files.** It moves what an agent session needs: the repository state and the conversation.

## Related Docs

- [Remote Access](29-remote-access.md) — the remote daemons and panes this builds on
- [Agent Protocol](39-agent-protocol.md) — ACP, whose `session/resume` reconnects to a session the host's agent can restore
- [Agent Management](07-agent-management.md) — the agent panes being handed off
- [Agent Control API](32-agent-control-api.md) — the `ctl` surface that exposes handoff
- [Roadmap](33-roadmap.md) — Phase 4, where remote daemons land
