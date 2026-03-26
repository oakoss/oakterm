---
title: "Testing"
status: draft
category: cross-cutting
description: "Unit, integration, platform, perf, security, a11y, VT compliance"
tags: ["testing", "ci", "vttest", "fuzzing", "benchmarks", "a11y-testing"]
---
# Testing


Extensive testing across every layer. A regression in any area — rendering, performance, accessibility, plugins — is treated as a bug.

## Test Layers

### Unit Tests
- VT parser: every escape sequence against the xterm/VT220 spec
- Scroll buffer: ring buffer operations, archival, compression
- Unicode: grapheme cluster width, BiDi, combining marks, ZWJ emoji sequences
- Config parser: flat file, Lua, migration, validation
- Plugin host: capability checking, message routing, lifecycle

### Integration Tests
- Renderer: screenshot comparison tests for font rendering, ligatures, color
- Multiplexer: split/tab/workspace operations, session serialize/restore
- Shell integration: prompt markers, scroll-to-prompt, cwd tracking
- Clipboard: OSC-52 passthrough through splits and SSH domains
- Plugin API: full lifecycle test for each API surface (sidebar, panes, palette, notify, a11y)

### Platform Tests
- CI runs on macOS, Linux (Wayland + X11), and Windows
- Platform-specific: AppKit, GTK4, WinUI chrome behavior
- Font rendering comparison across platforms
- Keyboard input (IME, dead keys, modifier keys) on each platform
- Accessibility: VoiceOver (macOS), NVDA (Windows), Orca (Linux)

### Performance Tests (from ideas/12-performance.md)
- Input latency benchmark (target: <8ms)
- Throughput benchmark (cat large file)
- Memory at idle, under load, after 100k lines
- Startup time with and without plugins
- Scroll FPS through large buffer
- Plugin overhead per frame

Regressions fail the build. Performance dashboard is public.

### Security Tests
- Escape sequence injection fuzzing
- Plugin sandbox escape testing
- Lua config sandbox verification (no os.execute, no io.popen)
- OSC-52 clipboard read blocked by default
- Bracketed paste integrity

### Accessibility Tests
- Automated screen reader tree verification
- WCAG contrast ratio checks on all bundled themes
- Keyboard-only navigation of all UI elements
- `prefers-reduced-motion` respected (no animations)
- Tab order correctness for sidebar, palette, settings

### Plugin Compatibility Tests
- All bundled plugins tested against the current API version
- Plugin load/unload 1000 cycles — verify no memory leak
- Plugin crash recovery — verify terminal survives
- Plugin permission enforcement — verify blocked calls are rejected
- Plugin a11y label enforcement — verify unlabeled UI is rejected

### VT Compliance Tests
- vttest (standard VT terminal test suite)
- esctest (comprehensive escape sequence test suite from iTerm2)
- Our own test suite for modern extensions (Kitty graphics, OSC-52, synchronized output)
- Comparison tests against xterm behavior (the reference implementation)

### Memory Tests (from ideas/15-memory-management.md)
- 24-hour soak test with simulated AI agent output
- Verify ring buffer ceiling holds under sustained output
- Verify disk archive works for overflow
- Plugin memory cap enforcement
- Glyph atlas LRU eviction under font-heavy workloads

### Theme Tests
- All bundled themes pass WCAG AA contrast (4.5:1)
- High-contrast themes pass WCAG AAA (7:1)
- Theme validator catches missing fields
- Live theme switching doesn't cause visual artifacts

## Test Infrastructure

- CI on every PR — all platforms, all test layers
- Nightly extended runs — soak tests, fuzzing, performance regression
- Public performance dashboard tracking metrics over time
- Flaky test policy: a flaky test is a bug, not a retry

## Related Docs

- [Performance](12-performance.md) — performance targets and benchmarks
- [Memory Management](15-memory-management.md) — soak tests
- [Accessibility](17-accessibility.md) — a11y testing requirements
- [Security](21-security.md) — security fuzzing and sandbox testing
- [Theming](22-theming.md) — theme contrast validation
