# Accessibility

Accessibility in terminal emulators is broken across the board. Zero modern GPU-rendered terminals on macOS or Linux have functional screen reader support. This is a massive gap and an opportunity to lead.

## The Problem

GPU-rendered terminals paint pixels to a framebuffer. Screen readers see nothing — there's no accessibility tree to traverse. Blind developers are forced to choose their entire stack based on screen reader compatibility, not preference.

| Terminal | Screen Reader Status |
|----------|---------------------|
| Windows Terminal | Only GPU terminal with real a11y (UIA tree) |
| macOS Terminal.app | Basic VoiceOver support |
| iTerm2 | Better VoiceOver than Terminal.app |
| Ghostty | "Basically nonexistent" (their words) |
| Alacritty | None |
| Kitty | None |
| WezTerm | Designed on paper, nothing implemented |
| Warp | Custom UI doesn't expose to system a11y API |

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

## Accessibility Is Not a Plugin

This is one area where accessibility belongs in the core, not in a plugin. The accessibility tree must be maintained by the renderer — it can't be bolted on after the fact. Every core component (panes, sidebar, palette, settings) exposes accessibility information.

Plugins that add UI (sidebar sections, palette commands) must provide accessibility labels as part of the plugin API:

```rust
// Plugin API requires accessible labels
sidebar.add_entry(SidebarEntry {
    label: "feat/auth",
    accessible_label: "Agent feat/auth, Claude, needs input, 62% context",
    badge: Badge::NeedsInput,
    // ...
});
```

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
