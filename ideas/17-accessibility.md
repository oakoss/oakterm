---
title: 'Accessibility'
status: reviewing
category: cross-cutting
description: 'AccessKit, screen reader, color blindness, extensible a11y API'
tags: ['a11y', 'accesskit', 'screen-reader', 'voiceover', 'color-blindness']
---

# Accessibility

> **Note:** [ADR 0001](../docs/adrs/0001-accessibility-in-phase-zero.md) moved accessibility from Phase 5 to Phase 0. AccessKit integration is built alongside the renderer from day one.

Accessibility in terminal emulators is broken across the board. Zero modern GPU-rendered terminals on macOS or Linux have functional screen reader support. This is a massive gap and an opportunity to lead.

## The Problem

GPU-rendered terminals paint pixels to a framebuffer. Screen readers see nothing — there's no accessibility tree to traverse. Blind developers are forced to choose their entire stack based on screen reader compatibility, not preference.

| Terminal           | Screen Reader Status                        |
| ------------------ | ------------------------------------------- |
| Windows Terminal   | Only GPU terminal with real a11y (UIA tree) |
| macOS Terminal.app | Basic VoiceOver support                     |
| iTerm2             | Better VoiceOver than Terminal.app          |
| Ghostty            | "Basically nonexistent" (their words)       |
| Alacritty          | None                                        |
| Kitty              | None                                        |
| WezTerm            | Designed on paper, nothing implemented      |
| Warp               | Custom UI doesn't expose to system a11y API |

## Our Approach: AccessKit Integration

[AccessKit](https://github.com/AccessKit/accesskit) is a cross-platform Rust accessibility library. It provides a unified API that maps to:

- NSAccessibility on macOS (VoiceOver)
- UIA on Windows (NVDA, JAWS)
- AT-SPI on Linux (Orca)

Since we're building in Rust, AccessKit is a natural fit. The terminal maintains an accessibility tree alongside the visual rendering. Screen readers can traverse terminal content, get notified of changes, and navigate semantically.

## Specific Requirements

### Screen Reader Support

- Full accessibility tree for terminal content
- Announce new output as it arrives (configurable verbosity)
- Navigate by line, word, character in scrollback
- Announce command prompts distinctly from output
- Read pane/tab names and status
- Navigate sidebar entries
- Palette is fully screen-reader navigable

### Low Vision

- macOS zoom follows keyboard focus (Ghostty #4053 — broken there, we fix it)
- High-contrast theme bundled and auto-activated from system settings
- Configurable minimum contrast ratio enforcement on ANSI colors
- Large cursor option
- Adjustable line height and character spacing

### Color Blindness

- Default theme passes WCAG AA (4.5:1 contrast)
- Bundled high-contrast theme passes WCAG AAA (7:1)
- Status indicators never rely on color alone — always include shape/icon/text
  - Sidebar badges: ❓ ✓ ✗ ⟳ (not just red/green dots)
  - Test results: "14/14 passing" text, not just a green bar
- Respect `NO_COLOR` env var in our own output
- Color simulation mode for testing (deuteranopia, protanopia, tritanopia)

### Motor Accessibility

- Full Keyboard Access works (Ghostty #6764 — broken there, we fix it)
- Voice Control / dictation works (Ghostty #8717 — broken there, we fix it)
- Keyboard text selection without mouse (Alacritty #3855)
- All UI elements reachable by keyboard — tabs, sidebar, palette, settings
- Hints mode is the keyboard alternative to clicking links/paths

### Cognitive Accessibility

- Respect `prefers-reduced-motion` — disable cursor blink, tab transitions, animations
- Discoverable mode bar (from Zellij) — always know what mode you're in
- Progressive disclosure in settings — simple view by default, advanced on demand
- Consistent, predictable keybinds

### Dyslexia

- Font fallback chain means any font works — including OpenDyslexic Mono
- Configurable character spacing and line height
- No visual clutter by default

## Core + Extensible

The accessibility tree lives in the core — it can't be bolted on as a plugin. But the accessibility system itself is extensible so the community can build on it.

### What the core provides

- AccessKit integration maintaining the accessibility tree alongside the renderer
- Every built-in component (panes, sidebar, palette, settings) exposes a11y information
- Platform adapters for VoiceOver (macOS), NVDA/JAWS (Windows), Orca (Linux)
- Accessible announce API for plugins to send screen reader announcements
- System preference detection (high contrast, reduced motion, color filters)

### Performance consideration

Maintaining an a11y tree alongside the renderer is not free. To stay within our performance budgets:

- A11y tree updates are **batched and async** — not per-frame unless a screen reader is actively querying
- When no assistive technology is detected, the tree is maintained lazily (structural updates only, no per-character tracking)
- When a screen reader attaches, the tree activates fully with real-time updates
- A11y overhead is tracked in `:debug perf` as a separate line item
- Target: <0.5ms/frame overhead when a screen reader is active, ~0ms when inactive

### What plugins must provide

Plugins that add UI are **required** to provide accessibility labels. The API enforces this — entries without labels are rejected:

```rust
// Plugin API requires accessible labels
sidebar.add_entry(SidebarEntry {
    label: "feat/auth",
    accessible_label: "Agent feat/auth, Claude, needs input, 62% context",
    badge: Badge::NeedsInput,
    // ...
});

// Palette commands need accessible descriptions
palette.register(PaletteCommand {
    name: ":docker logs",
    accessible_description: "View logs for a Docker container",
    // ...
});
```

### What plugins can extend

Plugins get accessibility primitives to build on:

```text
Plugin Accessibility API
├── announce(message, priority)       — send text to screen reader
│   priority: polite (queued) or assertive (interrupts)
├── set_live_region(pane, politeness) — mark a pane as a live region
│   screen reader auto-announces new content
├── set_role(element, role)           — semantic role for custom UI
│   roles: alert, status, log, timer, progressbar
├── label(element, text)              — accessible name
├── description(element, text)        — accessible description
├── value(element, current, min, max) — for progress bars, meters
└── relationship(element, rel, target)— links related elements
```

### Community plugin examples using the a11y API

**Screen reader verbosity profiles:**

```lua
-- Plugin: a11y-verbosity
-- Lets users choose how much the screen reader announces
profiles = {
  minimal = { announce_output = false, announce_status = true },
  moderate = { announce_output = "summary", announce_status = true },
  verbose = { announce_output = "full", announce_status = true },
}
```

**Sound cues plugin:**

```lua
-- Plugin: a11y-sounds
-- Maps terminal events to distinct audio cues
sounds = {
  command_success = "gentle-chime.wav",
  command_failure = "low-buzz.wav",
  agent_needs_input = "attention.wav",
  agent_complete = "complete.wav",
  test_pass = "tick.wav",
  test_fail = "alert.wav",
}
```

**High-contrast sidebar plugin:**

```lua
-- Plugin: a11y-high-contrast-sidebar
-- Replaces icon-based badges with large text labels
-- For users who can't distinguish small icons
```

**Braille display optimization plugin:**

```lua
-- Plugin: a11y-braille
-- Optimizes output for braille displays
-- Strips decorative Unicode, simplifies box-drawing characters
-- Provides braille-friendly alternatives to TUI elements
```

### The rule

Accessibility in the core is **mandatory and non-negotiable** — the tree, the platform adapters, the enforcement on plugin labels. But _how_ that accessibility is experienced is extensible — verbosity, sounds, braille optimization, contrast profiles. The core guarantees the floor. Plugins raise the ceiling.

## Testing

- Automated screen reader testing in CI (not just visual regression)
- Manual testing with VoiceOver, NVDA, and Orca before each release
- Accessibility audit as part of the release checklist
- Invite blind/low-vision developers to beta test

## Reference

- [AccessKit](https://github.com/AccessKit/accesskit) — the Rust library we'd use
- [Windows Terminal UIA implementation](https://github.com/microsoft/terminal/pull/1691) — reference for how to do this right
- [GitHub CLI a11y redesign](https://github.blog/engineering/user-experience/building-a-more-accessible-github-cli/) — model for accessible CLI output
- [NO_COLOR standard](https://no-color.org/) — respect this in our own output
- [Modus Themes](https://protesilaos.com/emacs/modus-themes) — WCAG AAA color reference
- [ACM CHI 2021: CLI Accessibility](https://dl.acm.org/doi/fullHtml/10.1145/3411764.3445544) — academic research on the problem

## Related Docs

- [Plugin System](06-plugins.md) — accessibility API primitives for plugins
- [Theming](22-theming.md) — WCAG contrast requirements, high-contrast themes
- [Renderer](02-renderer.md) — AccessKit tree alongside GPU rendering
- [Platform Support](20-platform-support.md) — VoiceOver, NVDA, Orca per-platform
- [Testing](25-testing.md) — automated a11y testing
