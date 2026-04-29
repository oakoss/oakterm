# Reviews

Point-in-time audits of the project's design docs, architecture, or competitive landscape. Reviews surface corrections, ADR candidates, and missing specs.

## Format

```text
YYYY-MM-DD-HHMMSS-short-title.md
```

Timestamped because reviews are snapshots — their findings are true at the time of writing and may become stale. The timestamp establishes order when multiple reviews happen on the same day.

## Template

```markdown
---
title: Short Review Title
date: YYYY-MM-DDTHH:MM:SS
scope: what was reviewed (e.g., "all 36 idea docs", "plugin system docs", "competitive landscape")
---

# Short Review Title

## Scope

What was reviewed and why.

## Findings

### Corrections

Factual errors, duplicates, broken references. Fix directly — no ADR needed.

### Contradictions

Conflicts across docs that need a decision. Each is an ADR candidate.

### Missing Specs

Formal definitions that don't exist yet but are needed before implementation.

### Validated Decisions

Design choices confirmed by research or competitive analysis.

### Challenged Decisions

Design choices that research suggests need revisiting.

## Action Items

Summary of what to do next, organized by type (corrections, ADRs, specs).
```

Not every review needs every section. Competitive research reviews may only have Validated/Challenged. Internal audits may only have Corrections/Contradictions.

## Index

| Date       | Review                                                                      | Scope                                                          |
| ---------- | --------------------------------------------------------------------------- | -------------------------------------------------------------- |
| 2026-03-26 | [Idea Docs Audit](2026-03-26-140000-idea-docs-audit.md)                     | All 36 idea docs + competitive landscape                       |
| 2026-03-28 | [Renderer Architecture](2026-03-28-160000-renderer-architecture-review.md)  | TREK-15 wgpu renderer — pipeline, atlas, color, cursor         |
| 2026-03-29 | [Handler-Grid Architecture](2026-03-29-013200-handler-grid-architecture.md) | Alt screen patterns, community pain points, feature priorities |
| 2026-04-28 | [Warp OSS Architecture](2026-04-28-210532-warp-architecture-review.md)      | warpdotdev/warp client — Phase 2/3 prior art, AGPL-aware       |
