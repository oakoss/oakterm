---
title: 'Security'
status: draft
category: cross-cutting
description: 'Escape injection, plugin sandbox, secure input, clipboard controls'
tags: ['security', 'escape-injection', 'sandbox', 'clipboard', 'privacy']
---

# Security

Security is a core principle. Terminals handle the most sensitive data on your machine — passwords, API keys, SSH credentials, production access. The terminal itself must be trustworthy.

## Threat Model

### Escape Sequence Injection

Malicious content in `curl` output, log files, or git repos can contain terminal escape sequences that:

- Rewrite visible text (hide malicious commands behind innocent-looking output)
- Set window titles to misleading text
- Attempt to paste commands via bracketed paste abuse
- Exfiltrate data via OSC sequences that report terminal state

**Mitigations:**

- Bracketed paste mode enabled by default — pasted content is always clearly delimited
- Dangerous escape sequences (title reporting, clipboard read) disabled by default
- OSC sequences that exfiltrate data (e.g., OSC 52 read, DA responses) are opt-in
- Configurable escape sequence allowlist/blocklist
- Visual indicator when a pasted command contains control characters

### Plugin Security

WASM plugins are sandboxed, but a malicious plugin could still:

- Request excessive permissions (sidebar + network + fs = data exfiltration)
- Exfiltrate visible terminal content via the `pane.output` API
- Inject keystrokes via the `pane.input` API

**Mitigations:**

- Capability-based permissions — plugins can only access what they requested and the user approved
- Permission prompts show exactly what the plugin can do in plain language
- `pane.input` requires explicit `pane.input` capability — separate from `pane.create`
- Network capability shows which domains the plugin communicates with
- Plugin checksums verified against the registry on install and update
- Community plugins are WASM binaries — auditable, no native code execution
- Sideloaded plugins show a stronger warning than registry plugins

### Secure Input

- **Secure keyboard entry** mode: when a password prompt is detected, other processes cannot read keystrokes (macOS Secure Event Input, equivalent on other platforms)
- Password detection is heuristic (common patterns: `Password:`, `passphrase`, `sudo`) + configurable patterns
- Visual indicator in the tab/status bar when secure input is active
- Secure input can be toggled manually via keybind

### Clipboard Security

- Clipboard write (OSC 52 write) is allowed by default — programs can copy to clipboard
- Clipboard read (OSC 52 read) is denied by default — programs cannot read your clipboard without permission
- Configurable per-pane: agent panes may need different clipboard permissions than shells

### Configuration Security

- Config files are never executed with elevated privileges
- Lua config runs in a restricted sandbox — no `os.execute`, no `io.popen`, no shell access
- Lua config can read files (for project detection) but cannot write files or make network calls
- Plugin configs are validated against declared schemas

### Supply Chain

- Plugin registry entries include checksums (SHA-256)
- Plugins are signed by their authors (optional but surfaced in the UI)
- `phantom plugin audit` checks installed plugins against known vulnerabilities
- Plugin updates require explicit user action — no auto-update

## Privacy

- Zero telemetry. No analytics. No crash reporting. No phoning home. Ever.
- No account required. No login. No registration.
- AI features use BYOK — API keys stored locally, never sent to us
- Local LLM support (Ollama) for zero-network AI features
- No first-party cloud service of any kind
- The terminal never communicates with any server unless a plugin with network permission does

## Security Defaults

Everything secure by default, relaxable when needed:

| Setting                | Default             | Can be changed |
| ---------------------- | ------------------- | -------------- |
| Bracketed paste        | On                  | Yes            |
| OSC 52 clipboard write | On                  | Yes            |
| OSC 52 clipboard read  | Off                 | Yes            |
| Title reporting        | Off                 | Yes            |
| Secure keyboard entry  | Auto-detect         | Yes            |
| Plugin auto-update     | Off                 | Yes            |
| Plugin network access  | Per-plugin approval | Yes            |
| Lua sandbox (no os/io) | On                  | No (hardcoded) |

## Auditing

`:debug security` shows the current security state:

```text
┌──────────────────────────────────────────────────┐
│  Security Status                                 │
├──────────────────────────────────────────────────┤
│  Bracketed Paste       ✓ enabled                 │
│  Secure Keyboard Entry active (password prompt)  │
│  Clipboard Read        ✗ blocked                 │
│  Clipboard Write       ✓ allowed                 │
│  Title Reporting       ✗ blocked                 │
│  Active Plugins        3 (all verified)          │
│  Plugin Network Access docker-manager → unix sock│
│  Sideloaded Plugins    0                         │
│  Last Plugin Audit     2h ago (clean)            │
└──────────────────────────────────────────────────┘
```

## Related Docs

- [Plugin System](06-plugins.md) — capability-based plugin permissions
- [Debugging](14-debugging.md) — `:debug security` status
- [Health Check](28-health-check.md) — security section in `:health`
- [Remote Access](29-remote-access.md) — auth and encryption for daemon connections
