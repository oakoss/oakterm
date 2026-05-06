---
title: Herdr Agent Multiplexer Review
date: 2026-05-06T16:40:03
scope: ogulcancelik/herdr — agent multiplexer prior art for the agent control API (idea 32), agent protocol (idea 39), sidebar (idea 04), and multiplexer (idea 03)
---

# Herdr Agent Multiplexer Review

## Scope

[Herdr](https://github.com/ogulcancelik/herdr) (AGPL-3.0, Rust) is a TUI multiplexer that runs _inside_ any terminal and adds first-class agent-state awareness on top of tmux-shaped workspaces/tabs/panes. It ships a Unix-socket control surface (`herdr workspace ...`, `herdr pane run`, `herdr wait agent-status`), a hybrid state-detection model (process + hook + screen heuristics), and built-in installer hooks for Claude Code, Codex, OpenCode, and Pi.

This review reads herdr's `README.md`, `SOCKET_API.md`, `SKILL.md`, and `INTEGRATIONS.md` against existing oakterm idea docs: [03-multiplexer](../ideas/03-multiplexer.md), [04-sidebar](../ideas/04-sidebar.md), [07-agent-management](../ideas/07-agent-management.md), [29-remote-access](../ideas/29-remote-access.md), [32-agent-control-api](../ideas/32-agent-control-api.md), [34-notifications](../ideas/34-notifications.md), and [39-agent-protocol](../ideas/39-agent-protocol.md).

**Layer note.** Herdr is _above_ oakterm in the stack; it competes with tmux, not with Alacritty/Ghostty/oakterm. The two compose: herdr could run inside oakterm. That makes herdr a study target for the multiplexer/agent-aware layer (Phase 1 + Phase 3), not the renderer/parser layer (Phase 0).

**License constraint.** Herdr is AGPL-3.0. OakTerm is MPL-2.0. AGPL forbids code copying and linking. Treat herdr as study-only, like Warp. The patterns below are clean-room; the API names and shapes belong to herdr, but the underlying ideas (process detection + hook reporting + heuristics, dual ID forms, wait primitives) are general design.

**Method.** Read four top-level docs end-to-end via `gh api`; cross-checked against current oakterm idea doc text. No source files read; herdr's design is largely captured in its README and three reference docs.

## Findings

### Validated Decisions

#### CLI-over-daemon-socket as the agent control surface

Idea 32's central thesis — _"a CLI that talks to the daemon over its Unix socket, not an MCP server, not a special protocol"_ — is what herdr ships. `herdr workspace create`, `herdr pane split`, `herdr pane run`, `herdr wait` all wrap a newline-delimited JSON-RPC envelope over a Unix socket (`SOCKET_API.md`). Three layers (agent skill doc, CLI wrappers, raw socket) sit on the same control surface and are explicitly stacked.

Independent confirmation that the idea-32 architecture is sound: a Rust shop chose the same shape, ships it, and agents in the wild use it.

#### Stack of three integration layers (skill / CLI / raw socket)

Herdr's `SOCKET_API.md` calls out the three-layer stack explicitly: agents that just need workflow guidance load `SKILL.md`; shell scripts use the CLI wrappers; long-lived programs hit the raw socket. This mirrors the implicit shape in idea 32 (CLI for agents and scripts) and idea 06 (plugins for long-lived programs). Worth surfacing the layering explicitly in idea 32.

#### Persistent daemon with detach/reattach

`herdr` defaults to attaching to a background server; `ctrl+b q` detaches and agents keep running. Same design that idea 03 sketches for oakterm's daemon. Independent reference for the choice that the multiplexer should default to persistence rather than be a foreground-only process.

#### SSH-from-anywhere as a first-class story

Herdr leans hard on `ssh you@yourserver && herdr` as the remote-attach path. No app, no account. This validates idea 29's posture that remote access is "ssh + the same binary" rather than a custom protocol or web UI.

### Challenged Decisions

#### State vocabulary in idea 04 collapses two distinct states

Idea 04 ([04-sidebar.md](../ideas/04-sidebar.md)) uses `working / needs input / done / error`. Herdr distinguishes:

- `working` — actively running
- `blocked` — needs input or approval
- `done` — finished, **unseen**
- `idle` — finished, **seen**
- `unknown`

The `done` → `idle` transition (work finished → user has looked at it) is what makes "jump to next pane needing attention" (`Cmd+Shift+U` in idea 07) actually useful: without the distinction, every finished pane stays "done" forever. Idea 04 currently loses this affordance.

**Challenge:** add a fifth state in idea 04, name it explicitly. The cost is one extra word in the vocabulary; the value is a real UX primitive for inattentive review.

#### Idea 32's `pane input ... --enter` flag is ambiguous

Idea 32 ([32-agent-control-api.md](../ideas/32-agent-control-api.md)) currently sketches:

```bash
oakterm ctl pane input <pane-id> "npm run build"
oakterm ctl pane input <pane-id> --enter
```

Herdr splits this three ways:

- `pane.send_text` — literal text, no Enter
- `pane.send_keys` — keypresses (`Enter`, `Esc`, `C-c`)
- `pane.send_input` — atomic literal + keys ordered

The split removes ambiguity (does `--enter` mean append CR? send a separate Enter event? what about modifiers?) and matches what every TTY actually accepts. Refactor idea 32 before it hardens into a spec.

#### Idea 32 has no `wait` primitive

Idea 32 lists `pane create`, `pane input`, `pane output`, `notify`, `prompt`, `confirm`, `self set-status`, but no wait primitive. Every herdr agent example in `SKILL.md` revolves around one:

```bash
herdr wait output 1-3 --match "ready on port 3000" --timeout 30000
herdr wait agent-status 1-1 --status done --timeout 60000
```

Without a wait primitive, agents resort to polling loops in shell. With it, the daemon does the work and the agent blocks cleanly. Add `oakterm ctl wait output --match … --regex --timeout` and `oakterm ctl wait status --status done` to idea 32.

#### Idea 32's pane-output read has one source mode

Idea 32: `oakterm ctl pane output <pane-id> --lines 500` and `--follow`. Herdr `pane.read` takes a `source`:

- `visible` — current viewport
- `recent` — recent scrollback as rendered (with soft wraps)
- `recent-unwrapped` — recent text with soft wraps joined back together

Herdr's `wait_for_output` matches against the unwrapped form, and `recent-unwrapped` exposes that exact form to agents, so an agent can introspect _what would have matched_. Without this, regex matchers and read calls disagree at pane width boundaries. That's a real footgun: terminal width changes silently break agent scripts.

Idea 32 should adopt the source enum.

#### Idea 39 leaves the non-ACP fallback path implicit

Idea 39 ([39-agent-protocol.md](../ideas/39-agent-protocol.md)) says ACP is opt-in per agent and "a plain CLI agent without ACP support still works — it's just opaque output." Herdr ships a more useful answer: a hybrid three-tier model documented in `INTEGRATIONS.md`:

> - **process detection** owns pane identity, liveness, and "the process is gone"
> - **agent integrations** report semantic state like `working`, `blocked`, and `idle` over the local socket api when the tool exposes those events
> - **screen heuristics** remain the fallback for gaps, unsupported tools, or incomplete hook surfaces
>
> hooks/plugins do **not** become the source of truth for pane ownership. they enrich state reporting; they do not replace process detection.

For oakterm this maps to: process detection (always), structured marks via OSC 133 / ACP (when emitted, in flight as TREK-164), heuristic detection (fallback). The separation of concerns is the part to lift: identity is process-owned, state is hook-or-heuristic owned. Idea 39 should add a section formalizing this.

### Missing Specs / Idea-Doc Gaps

#### Empirical hook → state mappings for major agents

Herdr's `INTEGRATIONS.md` documents the reverse-engineered mapping from Claude Code / Codex / OpenCode hook events to agent state:

| Agent    | Hook event                          | State     |
| -------- | ----------------------------------- | --------- |
| Claude   | `UserPromptSubmit`                  | `working` |
| Claude   | `PreToolUse`                        | `working` |
| Claude   | `PermissionRequest`                 | `blocked` |
| Claude   | `Stop`                              | `idle`    |
| Claude   | `SessionEnd`                        | `release` |
| Codex    | `SessionStart`                      | `idle`    |
| Codex    | `UserPromptSubmit`                  | `working` |
| Codex    | `PreToolUse`                        | `working` |
| Codex    | `Stop`                              | `idle`    |
| OpenCode | `permission.asked`                  | `blocked` |
| OpenCode | `permission.replied: once`/`always` | `working` |
| OpenCode | `session.status: busy`/`retry`      | `working` |
| OpenCode | `session.status: idle`              | `idle`    |

(`release` in the Claude `SessionEnd` row is a lifecycle event, not a fifth state — it returns the pane from "claimed by an agent" to "plain shell." The state vocabulary stays at five: `working / blocked / done / idle / unknown`.)

This is empirical research; herdr discovered the right hook subset by integrating. When idea 39 / idea 07 sketch oakterm's own integration installers (or its claude-code/codex skill installers), cite this table rather than redoing the discovery.

One detail worth carrying over: Claude subagent stop/release events should _not_ flip the parent pane to idle. The bundled herdr hook converts subagent stop to `working` so a finished subagent doesn't make the parent look idle. That's the kind of detail that costs an afternoon to discover.

#### Pane `release` lifecycle for hook-claimed panes

Herdr exposes `pane.report_agent`, `pane.clear_agent_authority`, and `pane.release_agent`. The third is the interesting one: a pane was claimed by an agent (via hook), the agent exited, and the pane is _explicitly_ returned to plain shell state. Without this, a stale `working` badge can outlive the agent process.

Idea 32 has `self set-status` but no concept of "this pane was an agent and is now back to being a shell." Add the lifecycle to idea 32's status model: `claim → status updates → release`. Particularly important when hooks are uninstalled mid-session or when an integrated agent crashes.

#### Stable opaque IDs alongside compact human-readable forms

Idea 32 sketches `OAKTERM_PANE_ID=pane-a1b2c3d4` (stable opaque) but doesn't address ID compaction when panes/tabs/workspaces close. Herdr accepts both forms and is explicit about the contract:

- `w64e95948145ed1` — workspace ID, opaque, stable for the workspace's lifetime
- `w64e95948145ed1-2` — pane ID, workspace-scoped, stable across reorder
- `1`, `1:2`, `1-2` — compact human-readable shorthand, may compact when peers close

Responses always return stable IDs; requests accept either. This is the right answer for an API agents script against: humans type `1-2` interactively, agents store `w64...-2` for later use without surprise compaction.

Specify this in idea 32 (or its eventual spec).

#### Named sessions / socket namespacing

`herdr session list / attach / stop / delete` defines named runtime namespaces: separate persistent servers, separate sockets, **shared global config**. Resolution order:

1. explicit `herdr --session <name>` flag
2. `HERDR_SOCKET_PATH` env var (low-level override)
3. `HERDR_SESSION=<name>` env var
4. default session path

Use case: agents on `work` and agents on `side-project` don't stomp each other; closing the work laptop doesn't affect the side-project server.

Idea 03 ([03-multiplexer.md](../ideas/03-multiplexer.md)) doesn't sketch a multi-instance / namespaced-daemon story. Worth a section: oakterm could ship `oakterm --session <name>` with the same precedence rules. Idea 32's existing `OAKTERM_SOCKET` env var occupies the same slot as herdr's `HERDR_SOCKET_PATH` (explicit low-level override); a new `OAKTERM_SESSION` env var would parallel `HERDR_SESSION`.

#### Agent-facing skill doc as a shipped artifact

Herdr ships `SKILL.md`: a "you are running inside herdr, here's how to drive me" doc with a Claude Code skill frontmatter (`name`, `description`, `when to use`). Once `oakterm ctl` exists, a parallel `SKILL.md` is a low-effort, high-leverage lift. Already a Claude Code idiom.

Mention in idea 32 as a shipping artifact, not as a separate idea doc.

### Borrow-Worthy Patterns

1. **Hybrid three-tier state-source model** ([idea 39](../ideas/39-agent-protocol.md)). Process detection owns identity; hooks/protocol report semantic state; heuristics fill gaps. Single sentence in `INTEGRATIONS.md`, but it's the load-bearing architectural decision.

2. **`done` vs `idle` state distinction** ([idea 04](../ideas/04-sidebar.md)). Five-state vocabulary: `working / blocked / done(unseen) / idle(seen) / unknown`. Powers attention-cycling UX.

3. **`wait` as a first-class CLI primitive** ([idea 32](../ideas/32-agent-control-api.md)). `wait output --match … --regex --timeout` and `wait status --status done`. Daemon owns the wait; agent blocks cleanly.

4. **Three-way input split: `send-text` / `send-keys` / `send-input`** ([idea 32](../ideas/32-agent-control-api.md)). Removes the `--enter` flag ambiguity, matches what TTYs actually accept.

5. **`pane read --source visible|recent|recent-unwrapped`** ([idea 32](../ideas/32-agent-control-api.md)). The `recent-unwrapped` mode is the form `wait_for_output` matches against, so agents can introspect what would have matched.

### Honourable Mentions

6. **Stable opaque + compact human IDs** with explicit dual-form accept (idea 32 spec).
7. **`pane.release_agent` lifecycle** for explicit return-to-shell after hook claim (idea 32).
8. **Empirical hook → state mappings** for Claude/Codex/OpenCode (idea 39 / idea 07 reference).
9. **Ship a `SKILL.md`** alongside `oakterm ctl` (idea 32 mention).
10. **Named-session socket namespacing** with `--session` / `OAKTERM_SOCKET` / `OAKTERM_SESSION` precedence (idea 03).

### Where OakTerm Is Already Ahead — Don't Borrow Down

- **Permission model.** OakTerm's risk-scored 6-dimension model (destructiveness × scope × reversibility × privilege × externality × concurrency, idea 32) is more sophisticated than herdr's flat allow/deny. Keep.
- **Pane types.** Herdr is tile-only. OakTerm's floating / drawer / popup pane types (idea 32) are richer. Keep.
- **Plugin host.** OakTerm's Wasmtime plugin substrate (idea 06) has no analog in herdr. Keep.
- **GPU rendering, custom VT, OSC 133 marks, ACP.** Different layer; herdr stays at multiplexer. OakTerm is the renderer + parser + multiplexer + plugin host. Don't dilute.
- **Skill discovery / project rule engine.** Different concern (covered by Warp review); herdr has nothing here.

### License-Aware Notes

- AGPL forbids code lift. The patterns above are general design (three-tier state model, dual ID forms, wait/read primitives); same license posture as the Warp review.
- Herdr's hook scripts (`src/integration/assets/claude/herdr-agent-state.sh` etc.) are AGPL. Re-derive from Anthropic/OpenAI hook docs; don't mirror.
- The `SKILL.md` _format_ is the Claude Code skill convention (Anthropic-published), not herdr's invention. Safe to ship a parallel one.

## Action Items

### Idea Doc Updates

- **[`docs/ideas/04-sidebar.md`](../ideas/04-sidebar.md)** — expand state vocabulary to distinguish `done` (finished, unseen) from `idle` (finished, seen). Update the badge table and the `Cmd+Shift+U` cycle-to-attention behavior to consume the distinction.
- **[`docs/ideas/32-agent-control-api.md`](../ideas/32-agent-control-api.md)** — add (a) `wait output` / `wait status` primitives, (b) `pane output --source visible|recent|recent-unwrapped`, (c) split `pane input` into `send-text` / `send-keys` / `send-input`, (d) stable+compact dual-ID contract, (e) pane `release` lifecycle for hook-claimed panes, (f) note that a shipped `SKILL.md` is the agent-onboarding artifact.
- **[`docs/ideas/39-agent-protocol.md`](../ideas/39-agent-protocol.md)** — add "Non-ACP agent fallback" section formalizing the three-tier model: process detection (identity) → ACP / OSC 133 marks / hook integrations (semantic state) → screen heuristics (fallback). Cite herdr's `INTEGRATIONS.md` empirical mappings as research input.
- **[`docs/ideas/03-multiplexer.md`](../ideas/03-multiplexer.md)** — add named-session / socket-namespacing section with precedence rules (`--session` flag > `OAKTERM_SOCKET` > `OAKTERM_SESSION` env > default). Aligns with idea 32's existing `OAKTERM_SOCKET` env var.
- **[`docs/ideas/07-agent-management.md`](../ideas/07-agent-management.md)** — reference herdr's hook → state mappings for Claude/Codex/OpenCode as research input when sketching oakterm's own integration installers.

### ADR Candidates

- **ADR: Agent state vocabulary.** Five states (`working / blocked / done / idle / unknown`) vs four. Decides the contract that the sidebar, ACP `session/update` consumers, the `oakterm ctl wait status` primitive, and OSC 133 mark consumers all share.
- **ADR: Hybrid state-source model.** Formalize process-owned identity + protocol/hook-reported state + heuristic fallback. Resolves the ambiguity between idea 39 (ACP) and idea 07 (heuristics) about which is authoritative for what.

### Spec Candidates

- **`oakterm-control-api`** (formerly idea 32 → spec, gated on the above ADRs and idea-doc updates) — control-plane wire protocol, ID contract, state vocabulary, wait/read/send primitive surface.

### Trekker

- File icebox task: "Re-evaluate herdr design at Phase 2 boundary." Herdr is pre-1.0 and evolving fast; revisit when oakterm's multiplexer ships and ACP integration is in flight, particularly for new heuristic patterns and integration installers.
- File task under the spec-32 epic (or as a standalone P3 if not yet committed): "Specify state vocabulary, ID contract, and wait/read/send primitives in `oakterm-control-api` spec, citing this review."

### No Action

- **Don't import herdr code.** AGPL-3.0; clean-room only.
- **Don't pivot oakterm into a TUI multiplexer.** Different layer, different value prop. Compose, don't merge: herdr-shaped functionality lives in oakterm's own multiplexer + sidebar + agent-control surface, not as a wrapper around someone else's TUI.
- **Don't drop OSC 133 / ACP for heuristics.** Principled detection is the better long-term bet; heuristics are the fallback layer, not the primary.
