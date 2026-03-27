---
title: 'Notifications'
status: draft
category: cross-cutting
description: 'OS notifications, in-terminal notifications, history, DND mode'
tags: ['notifications', 'os-notification', 'badges', 'dnd', 'attention']
---

# Notifications

Referenced in 10+ docs but never fully specced. There are three notification surfaces and they serve different purposes.

## Three Surfaces

### 1. Sidebar Badges (in-terminal, always visible)

The primary notification surface. Badges on sidebar entries show state at a glance without interrupting you.

| Badge | Meaning                 | Set by                                  |
| ----- | ----------------------- | --------------------------------------- |
| ⟳     | Working                 | agent-manager, watcher                  |
| ❓    | Needs input/approval    | agent-manager                           |
| ✓     | Done/success            | agent-manager, watcher                  |
| ✗     | Error/failed            | agent-manager, service-monitor, watcher |
| ⚠     | Warning (memory, crash) | service-monitor, memory alerts          |

These are passive — you see them when you glance at the sidebar. No popup, no sound, no interruption.

### 2. In-Terminal Notifications (banners)

A non-modal banner that appears at the top or bottom of the terminal for time-sensitive information. Auto-dismisses after a few seconds or can be dismissed manually.

```text
┌──────────────────────────────────────────────────────────────┐
│ ℹ Agent feat/auth finished — 4 files changed  [View] [Merge]│
└──────────────────────────────────────────────────────────────┘
│                                                              │
│  Terminal content continues underneath                       │
│                                                              │
```

Used for:

- Agent finished / needs input
- Service crashed / restarted
- Tests went red
- Update available
- Plugin errors
- `oakterm ctl notify` messages from agents

### 3. OS Notifications (system-level)

Native notifications via NSUserNotification (macOS), libnotify (Linux), Windows Toast. Only used when the terminal is not focused or is minimized.

Used for:

- Long-running command finished (shell integration, configurable threshold)
- Agent needs approval (you walked away)
- Service crashed

OS notifications respect the system's Do Not Disturb settings automatically.

## Notification Flow

```text
Event (agent done, test failed, etc.)
  │
  ├── Always: update sidebar badge
  │
  ├── Terminal focused?
  │   YES → show in-terminal banner (if configured)
  │   NO  → send OS notification (if configured)
  │
  └── Add to notification history
```

## Attention Cycle

`Cmd+Shift+U` cycles through panes that need attention, most urgent first.

Priority order:

1. Errors (✗)
2. Needs input (❓)
3. Warnings (⚠)
4. Done (✓) — only if recent and unacknowledged

Pressing `Cmd+Shift+U` focuses the next pane in the cycle. The badge clears when you've viewed the pane.

## Notification History

`:notifications` in the palette shows recent notifications:

```text
Cmd+Shift+P → :notifications

┌──────────────────────────────────────────────────┐
│  notifications:                                  │
├──────────────────────────────────────────────────┤
│  2m ago   ✓ feat/auth finished (4 files)         │
│  5m ago   ✗ vitest: 2 tests failed               │
│  12m ago  ⚠ docker-manager: network timeout      │
│  18m ago  ✓ npm run build completed (exit 0)     │
│  1h ago   ℹ Update available: v0.7.1             │
└──────────────────────────────────────────────────┘
```

History is kept for the current session. Cleared on terminal restart.

## Do Not Disturb

```text
:dnd                    # toggle DND mode
:dnd on                 # enable
:dnd off                # disable
:dnd 30m                # enable for 30 minutes
```

When DND is active:

- Sidebar badges still update (passive, can't be silenced)
- In-terminal banners are suppressed
- OS notifications are suppressed
- `Cmd+Shift+U` still works (you can still check on demand)
- A small 🔕 indicator in the status bar shows DND is active

```lua
-- In config.lua
dnd = {
  enabled = false,              -- default
  suppress_banners = true,      -- suppress in-terminal banners during DND
  suppress_os = true,           -- suppress OS notifications during DND
}
```

## Configuration

```lua
-- In config.lua
notifications = {
  banners = true,
  os = true,
  os_min_duration = 10,
  sound = false,
  banner_duration = 5,
  banner_position = "top",
}
```

## Plugin API

Plugins send notifications via the `notify` capability:

```rust
notify.send(Notification {
    level: NotifyLevel::Success,      // info, success, warn, error
    title: "feat/auth finished",
    body: "4 files changed",
    pane_id: Some(pane_id),           // link to a pane (for "View" action)
    actions: vec!["View", "Merge"],   // buttons on the banner
    sticky: false,                    // true = don't auto-dismiss
    accessible_label: "Agent feat/auth finished with 4 files changed",
});
```

## What This Is Not

- Not a chat/messaging system
- Not a toast notification framework — we have three specific surfaces, each for a specific purpose
- Not a sound system — sound cues are a separate plugin (see [Accessibility](17-accessibility.md))

## Related Docs

- [Sidebar](04-sidebar.md) — badge display
- [Agent Management](07-agent-management.md) — agent state notifications
- [Shell Integration](18-shell-integration.md) — process completion notifications
- [Agent Control API](32-agent-control-api.md) — `oakterm ctl notify`
- [Accessibility](17-accessibility.md) — screen reader announcements, sound cue plugin
- [Health Check](28-health-check.md) — notification permissions in `:health`
