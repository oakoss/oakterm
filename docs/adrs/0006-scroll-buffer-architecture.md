---
adr: '0006'
title: Scroll Buffer Architecture
status: accepted
date: 2026-03-26
tags: [renderer, core]
---

# 0006. Scroll Buffer Architecture

## Context

Two idea docs make conflicting claims about scrollback behavior:

- [12-performance.md](../ideas/12-performance.md): scrolling "doesn't allocate or copy — adjusts a viewport offset"
- [15-memory-management.md](../ideas/15-memory-management.md): describes compressed disk archives with memory-mapped access for old scrollback

Both claims are individually correct but describe different tiers of the buffer. The review audit flagged this as needing reconciliation.

Additionally, CLI agents (Claude Code, Codex, Aider) use the alternate screen buffer, which prevents native terminal scrollback. Users cannot scroll back through long agent sessions. This is a top complaint across all terminals — Claude Code issue [#28077](https://github.com/anthropics/claude-code/issues/28077) (26 upvotes) and [#2479](https://github.com/anthropics/claude-code/issues/2479) (53 upvotes) document the pain.

Ghostty experienced a catastrophic memory leak (71 GB in 20-30 minutes) caused by its arena allocator never returning memory to the OS when pruning scrollback with non-standard pages from Claude Code's heavy Unicode output. This class of bug must be prevented by design.

## Options

### Option A: Memory-only scrollback (Ghostty/Alacritty/WezTerm model)

All scrollback in RAM. Fixed byte or line limit. Old lines are discarded when the limit is reached.

**Pros:**

- Simple implementation. Fast access.

**Cons:**

- Unbounded memory growth without hard limits (Ghostty's 71 GB leak).
- Fixed limits mean losing old output. Ghostty's 10 MB default is too small for power users. iTerm2's "unlimited" mode can exhaust all RAM.
- No alternate screen scrollback — CLI agents are unusable for reviewing history.

### Option B: Two-tier buffer with disk archive

Hot ring buffer in memory for recent lines (zero-copy viewport shift). Cold archive compressed to disk for older lines (zstd, memory-mapped on access). Hard memory ceiling on the hot buffer. Memory returned to OS on pruning — no arena pooling.

**Pros:**

- Hard memory ceiling prevents the Ghostty leak class entirely.
- Effectively unlimited scrollback without unbounded memory growth.
- Zero-copy viewport shift for the common case (scrolling recent output).
- Disk archive is cheap — 1 GB compressed holds ~2-4M lines at 200 columns.
- zstd decompression at ~1,500 MB/s means archive reads add ~13-20 microseconds per 4 KB block.

**Cons:**

- More complex implementation than memory-only.
- Disk I/O for cold scrollback (mitigated by memory-mapping and fast decompression).
- Disk space consumption needs limits and monitoring.

### Option C: Unlimited memory scrollback

No pruning, grow forever.

**Pros:**

- Simplest API — never lose output.

**Cons:**

- A resource bomb. 1M lines at 200 cols = 2.5-6.4 GB depending on cell size. With 10 panes, system lockup.

## Decision

**Option B — two-tier buffer with disk archive, plus alternate screen scrollback capture.**

### Buffer Architecture

| Tier            | Storage           | Access Pattern                        | Behavior                                                                                             |
| --------------- | ----------------- | ------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Hot ring buffer | Memory            | Zero-copy viewport offset shift       | Recent lines. Hard byte ceiling. Memory returned to OS on pruning (no arena pooling).                |
| Cold archive    | Disk (compressed) | Memory-mapped, decompressed on access | Older lines. zstd level 3 compression (~5:1 to 10:1 on terminal output). Encrypted with AES-256-GCM. |

The transition from hot to cold is invisible to the user — no visual stutter when scrolling across the boundary.

### Alternate Screen Capture

Lines that scroll off the top of the alternate screen viewport are appended to the primary scrollback (iTerm2 model). This is on by default. Works with every CLI agent today without agent-side changes.

Future layers designed into the architecture but implemented later:

- **Shadow buffer transcript** — a parallel VT emulator that processes all alternate screen bytes and produces a scrollable transcript.
- **Agent push API** — `oakterm ctl` lets agents push structured content directly to the terminal's history.

### Byte-Based Limits

Limits are byte-based, not line-count-based. A line at 200 columns costs 2.5x what a line at 80 columns costs. Line-count limits give unpredictable memory usage across different terminal widths.

### User-Facing Configuration

| Option                      | Default  | Description                                           |
| --------------------------- | -------- | ----------------------------------------------------- |
| `scrollback_limit`          | `"50MB"` | Hot buffer size per surface. Byte-based.              |
| `scrollback_archive`        | `true`   | Enable disk-backed cold archive.                      |
| `scrollback_archive_limit`  | `"1GB"`  | Per-surface disk archive limit.                       |
| `save_alternate_scrollback` | `true`   | Capture alternate screen lines to primary scrollback. |

### Internal Defaults (Not User-Configurable)

| Parameter       | Value                                 | Rationale                                                                                                       |
| --------------- | ------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| Compression     | zstd level 3                          | ~5:1+ ratio, 200+ MB/s compress, 1,500+ MB/s decompress.                                                        |
| Encryption      | AES-256-GCM                           | Terminal output contains secrets (passwords, tokens, API keys). VTE learned this in a 2012 security disclosure. |
| Disk free floor | 1 GB or 5% free (whichever is larger) | Stop archiving when disk is tight.                                                                              |

### Sizing Rationale

Memory impact at default 50 MB hot buffer:

| Scenario | Memory |
| -------- | ------ |
| 1 pane   | 50 MB  |
| 5 panes  | 250 MB |
| 10 panes | 500 MB |
| 20 panes | 1 GB   |

Safe on 8 GB machines with 10 panes. Power users on 16-32 GB machines can increase `scrollback_limit` up to 512 MB per surface.

Disk archive at default 1 GB per surface with 5:1 compression:

- ~5 GB raw content = ~2M lines at 200 columns
- Covers a full day of log tailing or multiple long Claude Code sessions

### PTY Size Reporting

Correct `TIOCGWINSZ` before the first byte is written to the PTY. Send `SIGWINCH` immediately after the child process starts. This prevents the resize-to-correct bug seen with Claude Code in Ghostty and other terminals.

## Consequences

- Update [12-performance.md](../ideas/12-performance.md) to clarify zero-copy applies to the hot ring buffer only.
- Update [15-memory-management.md](../ideas/15-memory-management.md) to document the two-tier architecture, encryption, and configurable limits.
- The ring buffer implementation must return memory to the OS on pruning — no arena allocator pooling.
- Phase 0 includes hot ring buffer and alternate screen capture. Disk archive can follow in Phase 0 or early Phase 1.
- Encryption key management for the disk archive needs a spec (per-session ephemeral key is the simplest approach).
- Shadow buffer transcript and agent push API are deferred to later phases but the architecture must not preclude them.

## References

- [12-performance.md](../ideas/12-performance.md)
- [15-memory-management.md](../ideas/15-memory-management.md)
- [Ghostty memory leak blog post](https://mitchellh.com/writing/ghostty-memory-leak-fix)
- [Ghostty memory leak fix (PR #10251)](https://github.com/ghostty-org/ghostty/pull/10251)
- [Claude Code scrollback issue #28077](https://github.com/anthropics/claude-code/issues/28077)
- [Claude Code scrollback issue #2479](https://github.com/anthropics/claude-code/issues/2479)
- [VTE scrollback disk security disclosure (2012)](https://seclists.org/fulldisclosure/2012/Mar/32)
- [iTerm2 alternate screen scrollback](https://iterm2.com/documentation-preferences-profiles-terminal.html)
- [Zstd benchmarks](https://facebook.github.io/zstd/)
