---
title: 'Circuit Breakers & Graceful Degradation'
status: draft
category: cross-cutting
description: 'Resilience patterns for external service dependencies'
tags: ['resilience', 'circuit-breaker', 'degradation', 'reliability']
---

# Circuit Breakers & Graceful Degradation

Multiple OakTerm components depend on external services that can fail or become slow. When they do, the terminal should degrade gracefully — not hang, not crash, not silently swallow errors.

## Problem

The terminal talks to external services at multiple layers:

| Component       | External Dependency         | Failure Mode                       |
| --------------- | --------------------------- | ---------------------------------- |
| Context engine  | LLM API (Ollama, Anthropic) | Timeout, rate limit, API down      |
| Plugin registry | HTTPS registry              | Network unreachable, DNS failure   |
| SSH domains     | Remote SSH server           | Connection refused, auth failure   |
| Remote domains  | Remote OakTerm daemon       | Network drop, daemon crashed       |
| Docker plugin   | Docker socket               | Docker not running, socket missing |
| Service monitor | Various sockets/APIs        | Service crashed, port closed       |

Without resilience patterns, a single slow API call can block the context engine's ghost text, a flaky SSH host can hang tab creation, and a crashed Docker daemon can spam error logs.

## Circuit Breaker Pattern

A circuit breaker tracks failures per service and transitions through states. A single success in the Closed state resets the failure count to zero:

```text
Closed (normal) ──[N failures]──→ Open (failing)
                                    │
                                    │ [timeout expires]
                                    ▼
                                  Half-Open (probing)
                                    │
                              ┌─────┴─────┐
                              │           │
                         [success]    [failure]
                              │           │
                              ▼           ▼
                           Closed       Open
```

- **Closed**: requests flow normally. Failures are counted.
- **Open**: requests fail immediately without attempting the call. Avoids hammering a dead service.
- **Half-Open**: after a cooldown, one probe request is allowed through. Success resets the breaker; failure reopens it.

### Thresholds

Configurable per service class, with conservative defaults:

```lua
resilience = {
  llm = {
    failure_threshold = 3,      -- consecutive failures before opening
    cooldown_secs = 30,         -- how long to stay open before probing
    timeout_ms = 5000,          -- per-request timeout
  },
  ssh = {
    failure_threshold = 2,
    cooldown_secs = 15,
    timeout_ms = 10000,
  },
  plugin_registry = {
    failure_threshold = 2,
    cooldown_secs = 60,
    timeout_ms = 10000,
  },
}
```

## Graceful Degradation

When a circuit opens, the feature degrades rather than fails:

| Component       | Degraded Behavior                                                 |
| --------------- | ----------------------------------------------------------------- |
| Context engine  | Ghost text disabled, deterministic completions still work         |
| Plugin registry | "Registry unavailable" in palette, installed plugins unaffected   |
| SSH domain      | Tab shows "reconnecting..." with retry countdown, other tabs work |
| Remote domain   | Sidebar shows disconnected icon, local panes unaffected           |
| Docker plugin   | Section shows "Docker unavailable", badge ✗, no container entries |

A broken dependency never blocks the terminal's core function (rendering, typing, local shell). Features built on failing services become unavailable with clear visual feedback and resume when the service recovers.

## Visibility

### Status Bar / Health

`:health` shows circuit breaker state for all services:

```text
## Service Health
✓ Context engine (Ollama)   closed    0 failures
⚠ SSH: homelab              half-open  probing...
✗ Docker socket             open       next probe in 45s
✓ Plugin registry           closed    0 failures
```

### Notifications

When a circuit opens, the terminal shows an in-terminal banner:

```text
⚠ Docker connection lost — docker plugin degraded. Retrying in 30s.
```

When a circuit closes (service recovered):

```text
✓ Docker connection restored.
```

## What This Is Not

- Not retry logic for individual requests — that's handled at the call site
- Not a health check system — `:health` is diagnostic, circuit breakers are runtime protection
- Not a load balancer — we don't route between multiple instances of the same service
- Not a rate limiter — we don't throttle outgoing requests, we stop them when the service is down

## Related Docs

- [Health Check](28-health-check.md) — diagnostic infrastructure that would surface breaker state
- [Context Engine](05-context-engine.md) — LLM API dependency
- [Remote Access](29-remote-access.md) — remote domain connection resilience
- [Plugin System](06-plugins.md) — registry and plugin runtime dependencies
- [Performance](12-performance.md) — latency and resource budgets
