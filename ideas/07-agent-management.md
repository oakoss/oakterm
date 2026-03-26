---
title: "Agent Management"
status: draft
category: plugin
description: "Worktree lifecycle, notifications, merge/diff"
tags: ["agents", "git-worktree", "claude-code", "codex", "notifications"]
---
# Agent Management


A bundled plugin, not a core feature. When enabled, the terminal becomes agent-aware without becoming an agent dashboard.

## Launching Agents

```
:agent claude --task="add rate limiting to /api/users"
```

This:
1. Creates a git worktree on a new branch
2. Opens a pane in that worktree
3. Starts the agent process
4. Adds it to the sidebar with metadata
5. Names the tab from the branch

Agents never touch your working directory. Run 5 agents in parallel on the same repo — each on its own branch, in its own pane.

## Status Tracking

The plugin watches agent processes for state changes:

| Badge | Meaning |
|-------|---------|
| ⟳ | Working |
| ❓ | Needs approval or input |
| ✓ | Finished |
| ✗ | Errored |

Context window usage shown as a progress bar in the sidebar.

## Scrollback Handling

Community pain point: Claude Code breaks terminal scrollback — jumping to top/bottom erratically.

Solution: the terminal knows a pane is an agent process. Agent panes get scroll pinning — the agent's output doesn't hijack your scroll position. You can scroll up to review while the agent keeps working below.

## Notifications

`Cmd+Shift+U` jumps to the most recent pane that needs attention.

Tab badge shows status at a glance:
```
● server │ ◉ feat/auth ❓ │ ◉ fix/typo ✓ │
```

## Quick Actions

| Command | What it does |
|---------|-------------|
| `:diff` | Opens floating pane with your diff tool showing agent's changes |
| `:diff --all` | Summary across all agent panes |
| `:merge` | Commit + merge worktree to parent branch + cleanup + close pane |
| `:agents` | Palette view of all agents with status |

## Agent Palette

```
Cmd+Shift+P → :agents

┌──────────────────────────────────────────────────┐
│  agents:  Search agents                          │
├──────────────────────────────────────────────────┤
│  ◉  feat/auth     claude   ❓ needs approval     │
│  ◉  fix/typo      claude   ✓  done 2m ago       │
│  ◉  add-tests     codex    ██████░░ working      │
│                                                  │
│  Actions:                                        │
│     New Agent          Cmd+Shift+A               │
│     Approve All        Cmd+Shift+Y               │
│     Kill Agent                                   │
│     View Diff                                    │
│     Merge & Cleanup                              │
└──────────────────────────────────────────────────┘
```

## Provider Agnostic

Works with any CLI agent. Providers are registered in config:

```lua
agent_providers = {
  claude = { command = "claude" },
  codex  = { command = "codex" },
  aider  = { command = "aider" },
  goose  = { command = "goose" },
}
```

The plugin detects state from process output. Provider-specific detection patterns are configurable.

## Workspace Setup Scripts (from Conductor)

Configurable scripts run when a new agent workspace is created:

```lua
workspace.on_create = function(ws)
  if ws:has_file("package.json") then
    ws:run("pnpm install")
  end
  if ws:has_file(".env.example") and not ws:has_file(".env") then
    ws:run("cp .env.example .env")
  end
end
```

## Workspace Forking (from T3 Chat)

Fork a workspace at its current state to try something risky. Git worktrees make this natural. Discard or keep the fork.

## What This Is Not

- Not a three-panel agent dashboard (Conductor)
- Not a chat UI with a terminal drawer (T3 Code)
- Not a custom protocol agents must support (cmux notification API)
- No built-in diff viewer — uses your tools (delta, difftastic, etc.)
- No PR review UI — that belongs in GitHub/Linear

## Related Docs

- [Plugin System](06-plugins.md) — API primitives this plugin uses (sidebar, pane, process, notify)
- [Sidebar](04-sidebar.md) — where agents appear in the process dashboard
- [Memory Management](15-memory-management.md) — child process memory attribution
- [Shell Integration](18-shell-integration.md) — command completion notifications
