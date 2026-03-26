---
adr: '0003'
title: Update Check Policy
status: accepted
date: 2026-03-26
tags: [security, core]
---

# 0003. Update Check Policy

## Context

Two idea docs contradict each other on network behavior:

- [21-security.md](../../ideas/21-security.md): "the terminal never communicates with any server"
- [24-updates.md](../../ideas/24-updates.md): describes an HTTP GET to a version manifest on every launch

The review audit flagged this as a contradiction requiring a decision. The broader question is OakTerm's network policy and how update checking should work across different installation methods.

Ghostty launched with opt-out update checks, received backlash ([Discussion #3859](https://github.com/ghostty-org/ghostty/discussions/3859)), and changed to ask-on-first-run in v1.1. WezTerm and Kitty default to opt-out and receive ongoing complaints about unwanted network requests and popup notifications.

## Options

### Option A: Pure zero network (Alacritty model)

No update checks, ever. Users rely on their package manager or manually check GitHub releases.

**Pros:**

- Simplest implementation. Strongest privacy guarantee.
- No network-related complaints possible.

**Cons:**

- Direct-install users won't know about security patches.
- No mechanism to notify users of critical updates.

### Option B: Ask on first run (Ghostty post-v1.1 model)

First launch shows a one-time prompt. User's choice saved. No network before consent. Applies to all install types.

**Pros:**

- User consents before any network request.
- Discoverable — every user sees the option once.

**Cons:**

- Prompts users who installed via package manager and don't need it.
- One more dialog on first run.

### Option C: Install-source-aware

Package manager installs (Homebrew, winget, scoop, cargo, distro packages) skip update checks entirely — the package manager is the update path. Direct installs (dmg, GitHub release) ask on first run. Config override always wins.

Two build-time flags identify the binary:

- `INSTALL_SOURCE`: how the binary was installed
- `RELEASE_CHANNEL`: what stream of builds (stable, nightly, dev)

**Pros:**

- No unnecessary prompts for package manager users.
- Direct-install users get notified about updates.
- Config override provides an escape hatch in all cases.
- Build-time flags are the standard approach (Ghostty, WezTerm, Kitty, VS Code all use variants of this).

**Cons:**

- Requires build system integration for each packaging pipeline.
- More complex than Options A or B.

### Option D: Opt-out (Kitty/WezTerm model)

Update checks on by default. Users disable in config.

**Pros:**

- Maximum update coverage.

**Cons:**

- Violates "no surprise network" principle.
- Generated backlash for Ghostty. Ongoing complaints for WezTerm.

## Decision

**Option C — install-source-aware update checks.**

The security posture is minimal network, not zero network. No telemetry, no analytics, no user tracking, no account — ever. Network requests only for update checks, only with user consent or when the install method doesn't provide its own update path.

### Build-Time Flags

Two compile-time constants set by packaging pipelines via `build.rs`:

**`INSTALL_SOURCE`** — who manages updates:

| Value              | Set by              | Update behavior     |
| ------------------ | ------------------- | ------------------- |
| `homebrew`         | Homebrew formula    | No check, no prompt |
| `winget`           | Winget manifest     | No check, no prompt |
| `scoop`            | Scoop manifest      | No check, no prompt |
| `github-release`   | CI release workflow | Ask on first run    |
| `app-bundle`       | macOS .app bundling | Ask on first run    |
| `source` (default) | Source builds       | No check, no prompt |

**`RELEASE_CHANNEL`** — what stream of builds:

| Value           | Meaning                  | Update manifest          |
| --------------- | ------------------------ | ------------------------ |
| `stable`        | Tagged releases          | Stable version manifest  |
| `nightly`       | Nightly builds from main | Nightly version manifest |
| `dev` (default) | Local/source builds      | No update check          |

### Implementation

- `build.rs` reads `INSTALL_SOURCE` and `RELEASE_CHANNEL` environment variables and emits them as `cargo:rustc-env` constants. Defaults to `source` and `dev`.
- Use `vergen` or `shadow-rs` crate for git hash, build timestamp, and target triple.
- Cargo feature `disable-auto-update` available for distro packagers as a hard kill switch.
- Runtime fallback detects Homebrew prefix (`/opt/homebrew/Cellar/`), `.app` bundle structure, and system package paths for source builds that didn't set the env var.
- Config option `check-for-updates` (`off` / `check`) overrides all detection. Always wins.

### Update Check Mechanics

- Fetch a static version manifest file (no server-side logic, no telemetry).
- Compare client-side. No data sent beyond standard HTTP headers.
- Notification is unobtrusive (no popup windows — learn from Ghostty's "demogate" and WezTerm's complaints).
- User's first-run choice is saved to config and never asked again.

### Version Output

```text
oakterm 0.1.0 (stable, homebrew)
oakterm 0.1.0-nightly.2026.03.27 (nightly, github-release)
oakterm 0.1.0-dev+abc1234 (dev, source)
```

## Consequences

- Update [21-security.md](../../ideas/21-security.md) from "never communicates with any server" to "no network requests except an optional update check with explicit user consent. No telemetry, analytics, or user data is ever collected or transmitted."
- Update [24-updates.md](../../ideas/24-updates.md) to reflect install-source-aware behavior instead of unconditional launch-time checks.
- Homebrew formula, winget manifest, scoop manifest, and CI release workflows must set `INSTALL_SOURCE` and `RELEASE_CHANNEL` environment variables during build.
- Release infrastructure needs a static version manifest hosted on a CDN (no server-side logic).
- Phase 0 includes `build.rs` with metadata embedding. Update check UI deferred to when releases exist.

## References

- [21-security.md](../../ideas/21-security.md)
- [24-updates.md](../../ideas/24-updates.md)
- [Ghostty update backlash (Discussion #3859)](https://github.com/ghostty-org/ghostty/discussions/3859)
- [Ghostty "demogate" redesign (PR #9116)](https://github.com/ghostty-org/ghostty/pull/9116)
- [WezTerm update popup complaints (Issue #248)](https://github.com/wezterm/wezterm/issues/248)
