---
title: 'Updates'
status: reviewing
category: cross-cutting
description: 'Every update path works, staged updates, rollback'
tags: ['updates', 'rollback', 'release-channels', 'package-manager']
---

# Updates

> **Note:** [ADR 0003](../adrs/0003-update-check-policy.md) decided the update check policy: install-source-aware checks where package-manager installs defer to the package manager and standalone installs check a static version manifest. Update checks are opt-out, contain no telemetry, and respect the install source.

Updates should be frictionless. When you're told there's an update, every path to installing it should work.

## The Ghostty Problem

Ghostty shows an "update available" notification in the bottom-right corner. You try to update via the command palette — nothing happens. You have to click the specific notification. This is bad UX.

**Our rule: if you can see the update, you can install it from wherever you are.**

## How Updates Work

### Detection

- Check for updates on launch (configurable interval, default: daily)
- No background process — check happens when the terminal opens
- Zero telemetry — the check is a single HTTP GET to a static version manifest
- `oakterm --version` and `:debug` show current version and whether an update is available

### Notification

When an update is available:

```text
┌──────────────────────────────────────────────────┐
│  Update available: v0.6.0 → v0.7.0              │
│  [View Changes]  [Update Now]  [Later]  [Skip]  │
└──────────────────────────────────────────────────┘
```

- Shows once per session, non-blocking
- Dismissible
- "Skip" skips this specific version (won't nag again until the next release)

### Every Path Works

| Where you see the update | How you install it                                       |
| ------------------------ | -------------------------------------------------------- |
| Notification banner      | Click "Update Now"                                       |
| Command palette          | `:update`                                                |
| Settings palette         | "Update Available" entry at the top                      |
| CLI                      | `oakterm update`                                         |
| Status bar               | Click the version indicator                              |
| Package manager          | `brew upgrade oakterm` / `winget upgrade oakterm` / etc. |

All of these trigger the same update flow. None of them silently fail.

### Update Flow

1. Download the new binary (or defer to the system package manager)
2. Verify checksum against the signed manifest
3. Show changelog summary
4. "Restart to apply" — the update is staged, not applied mid-session
5. On restart, the new version runs. Old version kept as rollback.

### Rollback

If the new version crashes on launch or has a critical bug:

```bash
oakterm rollback
```

Reverts to the previous version. Keeps the last 2 versions on disk.

### Release Channels

```ini
update-channel = stable    # default — tested releases
update-channel = nightly   # latest builds, may be unstable
update-channel = none      # disable update checks entirely
```

### Package Manager Awareness

On macOS with Homebrew, the terminal detects it was installed via `brew` and defers to:

```bash
brew upgrade oakterm
```

On Linux with Flatpak, it defers to the Flatpak update system. On Windows with winget, it defers to winget. The built-in updater only runs when installed standalone.

### Plugin Updates

Plugin updates are separate from core updates:

```bash
oakterm plugin update              # update all plugins
oakterm plugin update agent-manager # update specific plugin
```

Or from the palette: `:plugins` shows which plugins have updates available.

Plugins never auto-update. Always explicit.

## What We Don't Do

- No auto-update without consent
- No silent background downloads
- No update that requires closing all windows first (stage it, apply on next launch)
- No broken command palette update — if it's in the palette, it works
- No telemetry in the update check (we don't even know how many users we have)

## Related Docs

- [Command Palette](08-command-palette.md) — `:update` command
- [Health Check](28-health-check.md) — version check in `:health`
- [Platform Support](20-platform-support.md) — package manager awareness per platform
