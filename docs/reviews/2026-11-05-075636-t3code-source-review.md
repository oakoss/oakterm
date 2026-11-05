---
title: T3 Code Source Review
date: 2026-11-05T07:56:36
scope: T3 Code (pingdotgg/t3code) source at 337c585 — server and terminal, agent integration, product — against OakTerm's agent and remote-access decisions
---

# T3 Code Source Review

## Scope

[T3 Code](https://github.com/pingdotgg/t3code) is an MIT-licensed agent GUI from T3 Tools. The [agent tooling audit](2026-08-16-215731-agent-tooling-landscape-audit.md) covered it from public material in August 2026. Since then it has grown into an agent server with desktop, web, iOS and Android clients, and its scope now overlaps OakTerm's daemon, agent-pane and remote-access plans. This review reads its source at commit `337c585` (tag `v0.1.8-nightly.20261105.4218`). Nothing from the repository was run.

Citations of the form `path:line` refer to the T3 Code repository at that commit. Claims marked inferred were reasoned from the source rather than read directly.

## T3 Code as of 2026-11-05

### Server and clients

- **The server owns the work.** Provider processes, terminals, Git and project files belong to the server (`docs/internals/overview.md:10-14`). Clients talk to it over HTTP and a WebSocket using Effect RPC with JSON payloads (`apps/server/src/ws.ts:3480-3515`). Every call needs a scope; observing a terminal needs `terminal:read`, typing into one needs `terminal:operate` (`auth/RpcAuthorization.ts:180-186`).
- **Versioning.** The protocol version must match exactly; the client tells the user which side to update (`compatibility.ts:11-44`), and the server answers a mismatched socket with HTTP 426 (`apps/server/src/ws.ts:3452-3460`). Features added later are announced as named capability flags in the server's public descriptor (`packages/contracts/src/environment.ts:150-183`).
- **Sessions outlive the UI.** `t3 service install` runs the server as a systemd user unit with lingering on Linux (`cloud/bootService.ts:96-132,715-764`) or a LaunchAgent on macOS, which stops at logout (`docs/user/background-service.md:90-93`). On Linux and Windows, closing the desktop app's last window stops its bundled server.
- **Persistence.** Threads, queued messages and an outbox live in SQLite at `~/.t3/userdata/statev2.sqlite` (`apps/server/src/config.ts:148`), committed in one transaction (`docs/internals/overview.md:45-53`). Interrupted runs continue after a restart when the provider's resume identity still matches, at most three times in a row (`docs/internals/server-updates.md:66-106`). Shell processes do not survive a server restart (`docs/user/background-service.md:26-27`); their history is restored from disk (`terminal/Manager.ts:2006-2030`).

### Terminal

- **PTYs** are node-pty, spawned as `xterm-256color` with `COLORTERM=truecolor` (`NodePtyAdapter.ts:258-272`, `terminal/Manager.ts:1371-1372`).
- **The server keeps raw output, not a grid.** History is a ring capped at 5,000 lines and 8 MiB (`terminal/Manager.ts:94-95`). Clients are full emulators: libghostty-vt compiled to WASM for the web and native for Android (`docs/internals/terminal-runtime.md:30-38`), drawn with Canvas 2D on the web (`apps/web/src/terminal/ghostty/surface.ts`). The server never parses VT.
- **Replay needs three guards** because each client re-emulates the bytes:
  1. Terminal queries and replies (DSR, DA, DECRQM, XTVERSION, Kitty keyboard, OSC 10/11/12 and more) are stripped from saved history (`terminal/Manager.ts:1094-1126,1181-1271`).
  2. History restored from disk gets a reset appended that leaves the alternate screen and clears inherited mouse, paste and keyboard modes (`terminal/Manager.ts:1047-1084`).
  3. The client detaches its PTY writer while replaying, so replies the emulator generates are discarded (`apps/web/src/terminal/ghostty/core.ts:366-385`).
- **Sizing with several viewers.** The PTY takes the smallest size among attached viewers; a resize from a client that is not attached is ignored (`terminal/Manager.ts:1707-1739,3350-3351`). There is no input ownership: any client with `terminal:operate` types straight into the PTY (`terminal/Manager.ts:3315-3328`).

### Remote access

- **Identity separate from route.** A persistent `environment-id` identifies the server whatever address reaches it (`docs/internals/remote.md:9-25`).
- **Ordered routes.** A saved environment lists routes (direct, bearer, relay, SSH). The client probes each route's public descriptor, which carries no credential, to order the routes, and treats a route that answers with a different environment ID as a poor match (`connection/driver.ts:69-160`). The ID comparison that rejects a wrong server runs after authorization (`packages/client-runtime/src/connection/resolver.ts:297-307`); T3's docs say a learned route "has to answer as this environment before it is used" (`docs/internals/remote.md:55-56`).
- **Authentication.** One-time pairing codes with a five-minute lifetime (`PairingGrantStore.ts:252`), signed session tokens, credentials bound to the client's key with DPoP, and short-lived WebSocket tickets (`docs/internals/environment-auth.md:9-18,73-86`).
- **The relay.** T3 Connect brokers a one-time credential bound to the client's key and never receives the session token; application traffic runs through a Cloudflare tunnel (`docs/internals/t3-connect.md:3-12,46-72`). The docs claim no end-to-end encryption, so Cloudflare's edge can see plaintext (inferred).
- **Peer links.** One environment signs in to another's MCP endpoint and can dispatch work there, fenced by the link's origin and mode limits (`docs/internals/remote.md:72-134`).

### Agent integration

- **Providers** implement one adapter interface: capabilities, a session with `startTurn`, `steerTurn`, `interruptTurn`, `resumeThread`, `forkThread` and more, and one normalized event stream (`packages/provider-core/src/server/ProviderAdapter.ts:83-183,607-713`). Capabilities are a fixed struct declared per adapter, not negotiated at runtime (`packages/contracts/src/orchestrationV2.ts:381-397`).
- **Claude** runs through the Agent SDK, which spawns the `claude` binary; approvals come through the SDK's `canUseTool`, and a turn ends on the SDK's result message (`apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:1199-1218,3735-3795,9927`).
- **Codex** runs as `codex app-server` over JSON-RPC.
- **ACP.** `initialize` always offers version 2, and the client decides v1 or v2 from the shape of the reply, because some agents claim 2 but answer in v1 shapes (`effect-acp/src/client.ts:1082-1090`). To resume, it uses `session/load` when the agent advertises it, otherwise `session/resume` (`packages/provider-acp/src/server/AcpSessionRuntime.ts:2217-2246`). Agents can be installed from the ACP Registry, with SHA-256-checked binaries.
- **No provider runs in a PTY.** A PTY appears only for ACP Registry terminal sign-in.
- **State.** Runs move through `preparing`, `queued`, `starting`, `running`, `waiting`, `completed`, `interrupted`, `failed`, `cancelled` and `rolled_back`; failures carry a class, including `usage_limit` with a reset time (`packages/contracts/src/orchestrationV2.ts:590-601,1447-1464`). Approval and question requests are durable records that state whether they can still be answered after a restart (`orchestrationV2.ts:1205-1229`). What clients and notifications see is a smaller set: starting, running, waiting for approval, waiting for input, completed, failed and stale, with a pending request outranking run status (`packages/shared/src/agentAwareness.ts:98-128`).
- **Switching providers mid-thread** carries a budgeted selection of history, not a summary: 16,000 tokens by default, at most 64,000, keeping messages, commands and file-change paths and dropping reasoning and tool state. The new agent can page through the rest with a thread-reading tool (`packages/provider-core/src/server/handoffBudget.ts:14-19,98-168,256-266`).
- **An MCP server** exposes T3 to agents: delegating tasks to child agents on any provider, reading and messaging threads, scheduling, worktrees and pull-request watches. A child cannot run with more authority than its parent, and a parent can answer a child's questions but not approve its permissions (`packages/contracts/src/orchestratorMcp.ts:168-240`). On the host, `t3 thread --wait` exits 1 when the run fails or stops and 2 when it waits for the user (`docs/user/outside-agents.md:113`).
- **Usage limits** are detected from provider errors and rate-limit events; an opt-in worker resumes after the reset time (`UsageLimitRecoveryWorker.ts:25-78`).
- **Credentials.** To redeem Claude reset credits, T3 reads `~/.claude/.credentials.json` for the OAuth access token on Linux and Windows and calls Anthropic's API with it (`apps/server/src/provider/claudeResetCredits.ts:138-143,159,223`).
- **Its FAQ** says Agent SDK usage drains Claude's 5-hour limit faster than the CLI and calls this a confirmed Anthropic-side bug (`docs/user/faq.md:36`). That is T3's claim, not verified here.

### Product

- **Threads** are tasks with a lifecycle: pinned, active, working, snoozed and settled, settling after three idle days or when their pull request merges (`docs/user/thread-sidebar.md:61-186`). Threads can share a worktree. A side chat (`/side`) forks the thread's context into a panel and can bring its findings back (`thread-sidebar.md:293-315`).
- **Git.** The commit button's action follows the repository's state: commit, push and open a pull request when none exists, otherwise less (`apps/web/src/components/GitActionsControl.logic.ts:214-226`). Pull-request watches wake the agent on failed checks, comments or conflicts (`docs/user/source-control.md:304-320`). Worktree cleanup rules and a settle hook are opt-in (`docs/user/project-settings.md:196-230`).
- **Notifications.** One coordinator drives sound, toasts, OS notifications and the dock badge: a toast only when the app is focused on another thread, an OS notification only when the app is unfocused (`apps/web/src/components/ThreadNotificationCoordinator.tsx:131-195`). Mobile push needs the hosted relay (`docs/user/mobile-notifications.md:3-11`). Viewing a thread on one device does not clear the alert on another.
- **Project.** MIT; outside contributions mostly not accepted (`README.md:100-102`); stable releases are promoted from nightly builds.

## Findings

### Corrections

None. The [agent tooling audit](2026-08-16-215731-agent-tooling-landscape-audit.md)'s "None of the eleven implements terminal emulation" still holds: T3 now embeds Ghostty's VT core (libghostty-vt) in its terminal panel and iOS app rather than writing an emulator.

### Contradictions

None found across OakTerm's docs.

### Validated Decisions

- **The core terminal layer stays uncontested** (the audit's composition stance, from the [herdr review](2026-05-06-164003-herdr-architecture-review.md)'s "Compose, don't merge"). T3 builds around Ghostty's VT core rather than writing its own.
- **The daemon parses VT; clients are views** ([ADR-0007](../adrs/0007-daemon-architecture.md), [Spec-0002](../specs/0002-vt-parser.md)). T3's clients re-emulate raw bytes, which forces the three replay guards above. OakTerm's daemon answers terminal queries itself and ships grid rows, so a client reattaching cannot provoke replies to the shell. The cost is VT state per pane in the daemon.
- **Pull-based rendering bounds memory per client** ([Spec-0001](../specs/0001-daemon-wire-protocol.md)). T3 needs acknowledgement windows and a full resync past 8 MiB of backlog (`apps/server/src/terminal/OutputProtocol.ts:15-60,73,173`). That resync shape is worth copying for a Phase 4 WebSocket hop.
- **Shells do not survive a daemon restart.** T3 loses its shell processes too, so [ADR-0007](../adrs/0007-daemon-architecture.md)'s failure table (running processes survive a GUI crash; a daemon crash restores sessions from disk) is parity, not a gap. T3 does keep scroll history across the restart, which OakTerm does not (see Challenged Decisions).
- **The interactive PTY as the default Claude pane** ([ADR-0022](../adrs/0022-agent-integration-protocol.md)). T3 builds on the Agent SDK and reports, as its own claim, that SDK use drains the 5-hour limit faster than the CLI.
- **ADR-0024's credential rule.** T3 reads Claude's OAuth token from its credential file to call Anthropic's API, the same pattern the August audit flagged in Orca. T3 also uses the SDK's own usage call for its limits display, which shows the sanctioned route exists.
- **ADR-0024 rule 4.** T3 keeps a run "running" while background work continues, as ADR-0024 rule 4 keeps a subagent's completion from completing its parent. T3 also ranks a pending request above run status, as ADR-0023 ranks `blocked` ahead of `done+success`; unlike ADR-0023, it ranks a pending request above a failure too (`packages/shared/src/agentAwareness.ts:98-128`).
- **CLI-first control with one enforcement point** ([ADR-0021](../adrs/0021-agent-control-api.md)). T3 went MCP-first with a thin CLI; OakTerm goes CLI-first with MCP as an optional wrapper. Both keep one place where permissions are checked. T3 needed per-thread tokens and OAuth only because it serves HTTP, which supports deferring capability tokens to Phase 4.
- **A daemon-global "seen"** ([ADR-0023](../adrs/0023-agent-state-vocabulary.md)). T3 lacks it, so viewing a thread on one device leaves the alert on another.

### Challenged Decisions

- **Feature gating on the minor version** ([Spec-0001](../specs/0001-daemon-wire-protocol.md)). T3 pairs an exact version check with named capability flags in its public descriptor. Spec-0001 gates features on the minor number and has already needed lockstep exceptions (1.2, 1.4 and 1.5). Capability bits in `ServerHello` would also give [ADR-0020](../adrs/0020-daemon-upgrade-version-skew.md)'s passive version indicator something specific to report. ADR candidate.
- **ADR-0023's agent states.** T3 needs three things the vocabulary lacks: a run that is alive but held by child or background work, `interrupted` as distinct from `cancelled`, and a usage-limit failure that carries a reset time, which is what drives auto-resume. `done`+`error` cannot carry "resume at reset". The agent-session spec (OT-300) should decide whether the outcome gains a reason and reset time or resuming stays out of scope.
- **Claude's stream-json transport as the Node-free path** ([ADR-0022](../adrs/0022-agent-integration-protocol.md)). If the faster limit drain T3 reports comes from the Agent SDK rather than from structured output generally, `claude -p --output-format stream-json` may drain the same way. That is untested; OT-300 should check it before relying on stream-json.
- **The bedtime move in [Agent Handoff](../ideas/40-agent-handoff.md).** T3 never moves a running session between hosts. It places work on the right environment from the start through peer links, and recovers interrupted runs on the same host. That supports the idea's Placement section as the primary path.
- **Scrollback is ephemeral** ([Spec-0010](../specs/0010-session-persistence.md)). T3 saves an 8 MiB tail and restores it. [ADR-0006](../adrs/0006-scroll-buffer-architecture.md)'s disk archive is the natural place for OakTerm to do the same; it would save cells rather than text and so need no query stripping, but it would still need T3's reset of inherited modes on restore.

### Missing Specs

- **Multi-client sizing and input ownership** (already open in the [Rex review](2026-11-04-233107-rex-architecture-review.md)). T3 is a second concrete candidate: smallest attached viewer wins, viewers counted per connection, resizes from unattached clients ignored. T3 has no input ownership; OakTerm should decide it, and idea 29's interactive setting maps onto T3's read and operate scopes.
- **Phase 4 identity, routes and authentication** ([Remote Access](../ideas/29-remote-access.md)). Idea 29 has a token and mTLS. It lacks a stable daemon ID separate from the route, ordered routes with fallback, a public descriptor that identifies the daemon without a credential, scoped sessions, pairing codes, key-bound credentials and short-lived socket tickets. The Phase 4 authentication ADR should be written against T3's model.
- **Daemon service mode.** Idea 29 has `--daemon` and a PID file. T3 uses a systemd user unit with lingering or a LaunchAgent, plus trial-and-commit updates with database rollback. A macOS LaunchAgent stops at logout, which matters for an always-on Mac host.
- **Durable permission requests** (OT-300). OakTerm's daemon outlives its clients and agents. A blocked request whose agent can no longer receive the answer needs a state, as T3's "can still be answered after a restart" field provides.
- **Context for a session whose files cannot be copied** ([Agent Handoff](../ideas/40-agent-handoff.md), open question 6). T3 never copies an agent's session files; it rebuilds context from history it owns, under a budget, with a tool to read the rest. OakTerm could only do the same for agents whose transcript it owns, which an opaque PTY pane does not provide.

### Worth Borrowing

- **ACP version detection by reply shape,** and trying `session/load` before `session/resume` (OT-300, [Agent Protocol](../ideas/39-agent-protocol.md)).
- **Exit codes for waiting:** `--wait` returns 1 when the run fails and 2 when it waits for the user (OT-289).
- **Children capped at their parent's authority;** a parent answers a child's questions but never approves its permissions (OT-289).
- **Terse agent instructions** that tell the agent to end its turn rather than poll (OT-288).
- **A `:pr` action that follows the repository's state** instead of always running the full chain ([Command Palette](../ideas/08-command-palette.md)).
- **Status icons with a "waiting on you since" timer,** and the focus rules for notifications ([Sidebar](../ideas/04-sidebar.md), [Notifications](../ideas/34-notifications.md)).
- **Settle and snooze as agent-pane lifecycle states,** worktree cleanup rules and a settle hook ([Agent Management](../ideas/07-agent-management.md)).
- **A compaction marker and a manual compact action** on the context meter ([Agent Management](../ideas/07-agent-management.md)).
- **A budgeted history handoff** when switching agents, as a fallback where native resume is unavailable ([Agent Handoff](../ideas/40-agent-handoff.md)).

### Not Borrowed

- **The hosted push relay.** It breaks local-first operation; idea 29's approach needs no hosted service.
- **Reading provider credential files** for usage or credits. ADR-0024 forbids it.
- **Secret-store integration** (1Password), outside OakTerm's environment-passthrough posture, and still behind a build flag in T3.
- **Managed policies before 1.0,** a nine-surface side panel, and pull-request review guides: scope OakTerm does not need.
- **Code.** T3 is TypeScript and React, and its terminal is Ghostty's. MIT code could be used in an MPL-2.0 project with its notice kept, but what transfers here is design, not code.

## Action Items

- **OT-300 (agent-session spec):** decide waiting, interrupted and usage-limit states; durable permission requests; ACP reply-shape detection and load-then-resume order; test whether stream-json drains Claude's limit like the Agent SDK.
- **OT-289 (agent control API spec):** consider `--wait` exit codes and the parent/child authority rules.
- **Spec-0001:** consider capability bits in `ServerHello`; ADR candidate.
- **Phase 4:** write the authentication and identity ADR against T3's model; specify service mode; decide multi-client sizing and input ownership.
- **[Agent Handoff](../ideas/40-agent-handoff.md):** add T3 to its prior art.
- **No action:** the remaining Worth Borrowing items are prior art to consult when their area is designed.
