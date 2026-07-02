---
adr: '0020'
title: Daemon Upgrade and Version Skew
status: proposed
date: 2026-07-01
tags: [core]
---

# 0020. Daemon Upgrade and Version Skew

## Context

[ADR-0007](0007-daemon-architecture.md) deferred the graceful-upgrade mechanism: what happens when the user installs a new OakTerm version while a daemon is running, especially with `daemon_persist = true`. The deferral condition — session persistence landing — is met ([Spec-0010](../specs/0010-session-persistence.md) accepted), and Phase 1 makes long-lived daemons the norm.

The wire protocol already gives us the detection half ([Spec-0001](../specs/0001-daemon-wire-protocol.md) version negotiation): minor mismatches are tolerated (unknown message types ignored), major mismatches reject the connection. What is undecided is what the new GUI does after a major rejection, and whether a tolerated minor mismatch is surfaced to the user at all.

Prior art (seeded in ADR-0007): tmux and WezTerm reject mismatched clients and force a manual, lossy server restart. Zellij's fix was a versioned IPC contract decoupled from the app version, so most upgrades attach cleanly — OakTerm already has this property via Spec-0001's protocol-version negotiation. The remaining problem is only the breaking (major) upgrade.

A constraint that bounds every option: running child processes cannot survive their daemon's exit unless PTY file descriptors are handed off between processes. Session persistence (Spec-0010) deliberately restores layout, working directories, and allowlisted commands — not running processes and not scrollback.

## Options

### Option A: Reject and require manual restart (tmux/WezTerm status quo)

The new GUI shows "daemon is version X, client is version Y — quit the daemon to upgrade." The user runs `oakterm quit`; Spec-0010 saves the session file on exit; the next launch restores.

**Pros:**

- Zero new mechanism; works today.
- The user chooses the moment sessions restart.

**Cons:**

- Manual step every major upgrade; the worst UX of the surveyed terminals.
- Users who kill the daemon instead of quitting cleanly skip the session save entirely.
- "Install update, open window, get an error" is a support-ticket generator.

### Option B: Coordinated serialize-and-restart

On major mismatch, the new GUI offers one action: "Upgrade daemon (shells will restart)." Accepting sends the old daemon a serialize-and-exit request; the old daemon saves the Spec-0010 session file and exits; the GUI spawns the new daemon, which restores the session through the normal Spec-0010 path (including the restartable-commands allowlist and partial-restore rules).

The request must be a message the _old_ daemon already understands — the exit ramp has to be built before the version that needs it. Spec-0001's `Shutdown` (0x06) is the daemon-to-client notification push, so the request direction is a new client-to-daemon `RequestShutdown` message (allocated from the infrastructure range, an additive minor bump). Shipping it early means every daemon from that point forward can be upgraded gracefully.

Delivery detail: a major mismatch rejects the new client's native handshake (Spec-0001, `status=1`), so the client re-connects speaking the daemon's older protocol major — which it learns from the rejecting ServerHello and, being newer, knows how to speak — solely to deliver `RequestShutdown`.

**Pros:**

- One-click upgrade; layout, cwds, and allowlisted commands survive. Reuses Spec-0010 wholesale — no second serialization format to version.
- The serialized session file is version-bridged by Spec-0010's own rules (unknown JSON fields ignored, `version` field for migration), so old-daemon-writes / new-daemon-reads skew is already specified.
- Degrades to Option A when the old daemon predates `RequestShutdown` support.

**Cons:**

- Running processes are killed (build watchers, ssh sessions, agents). The prompt must say so.
- Scrollback is lost (not persisted, per Spec-0010) — same loss profile as any daemon restart.

### Option C: Side-by-side version coexistence

Versioned socket paths (`socket-v1`, `socket-v2`). Old windows stay on the old daemon; new windows get a new daemon. Old sessions drain naturally.

**Pros:**

- Nothing is ever killed; no data loss at upgrade time.

**Cons:**

- Two daemons' worth of memory, sockets, and session files; `oakterm ctl` and single-instance assumptions (startup lock, session persistence file) now need version-aware routing.
- Sessions are split across daemons with no way to move a pane between them — the upgrade never completes until the user closes every old window, which is Option A's manual step in disguise.
- No surveyed terminal ships this; the complexity lands in exactly the lifecycle code that is hardest to test.

### Option D: Live handoff via FD passing

Old daemon passes PTY master FDs (SCM_RIGHTS) plus full terminal state to the new daemon; running processes survive.

**Pros:**

- The only option where running processes survive a major upgrade.

**Cons:**

- Requires a complete, version-bridged serialization of all daemon state — grids, scrollback handles, parser state — which is precisely what changed in a breaking upgrade. The mechanism is most fragile exactly when it is needed.
- Large, risky surface for a rare event; no terminal ships it.

## Decision

**Option B — coordinated serialize-and-restart, built on Spec-0010 and a new client-to-daemon `RequestShutdown` message.** It converts the breaking-upgrade case from a manual, lossy restart into a one-prompt restart with layout restoration, and its cost is machinery we already committed to (session persistence) plus one small protocol message. Option D remains a possible Phase 4+ enhancement once daemon state serialization exists for remote access; nothing in Option B forecloses it.

Two subsidiary decisions:

- **Minor version skew is surfaced, not silent.** A tolerated minor mismatch (new client, older daemon: new capabilities silently absent) shows a passive indicator — a status-bar note and a `:health` line saying "daemon vX.Y is older than client vX.Z; restart daemon to enable new features" — never a modal. Rationale: ADR-0007 flagged invisible capability loss as the risk; a passive surface fixes discoverability without nagging.
- **`RequestShutdown` semantics are save-then-exit.** Payload carries a reason (`quit` | `upgrade`); both save the session file first, and the daemon sends the existing `Shutdown` (0x06) push to remaining clients before closing. This gives `oakterm quit` and the upgrade flow one code path, so the save-on-exit logic cannot drift between them.

## Consequences

- Upon acceptance, Spec-0001 gains a client-to-daemon `RequestShutdown` message (payload, response, error cases — an additive minor bump in the infrastructure range) alongside the existing `Shutdown` (0x06) push, and a Trekker task implements save-then-exit in the daemon. This must ship before the first breaking protocol change — the mechanism only helps if old daemons already understand the request; until it ships, every major upgrade is Option A.
- The GUI gains the major-mismatch upgrade prompt (wording must state that running processes restart) and the minor-skew status-bar indicator.
- The startup path already handles "socket exists but connection refused" (stale-socket recovery); the upgrade flow reuses it after the old daemon exits.
- Upgrade UX degrades gracefully with daemon age: pre-`RequestShutdown` daemons fall back to Option A's manual message.
- Scrollback loss on upgrade is inherited from Spec-0010's scope; if that ever becomes unacceptable, the fix is persisting scrollback (a Spec-0010 revision), not a new upgrade mechanism.
- ADR-0007's "deferred to a later ADR" note is resolved by this ADR.

## References

- [ADR-0007 Daemon Architecture](0007-daemon-architecture.md) — deferral and prior art
- [Spec-0001 Daemon Wire Protocol](../specs/0001-daemon-wire-protocol.md) — version negotiation, message catalog
- [Spec-0010 Session Persistence](../specs/0010-session-persistence.md) — serialization format and restore rules
