---
adr: '0014'
title: Input Mode Classification
status: proposed
date: 2026-04-28
tags: [context-engine, ai, shell-integration]
---

# 0014. Input Mode Classification

## Context

[Idea 05 — Context Engine](../ideas/05-context-engine.md) Open Question 2: keep `?` as the sole AI affordance, or add probabilistic shell-vs-AI classification (Warp's ML-driven approach)?

The original idea-doc framing ("deterministic rules, no AI needed") leans toward `?`-only. The Warp review showed prior art for a heuristic-then-ML layered approach.

### Empirical context (from the Warp review)

`crates/input_classifier` in the Warp OSS uses three backends:

- `HeuristicClassifier` — pure-Rust, no ML, three wordlists (English, StackOverflow, Command), stems via `rust_stemmers`, scores `# NL words - # tokens with shell syntax`.
- `FasttextClassifier` — fasttext embeddings.
- `OnnxClassifier` — ONNX runtime model.

`ClassificationResult { p_shell, p_ai }`. `Context { current_input_type, is_agent_follow_up }` — sticky mode + follow-up disambiguation.

Notable detail from `crates/natural_language_detection/src/lib.rs:43-50`:

> If the first token is a known command and not in `RESERVED_KEYWORDS = ["what"]`, that token is excluded from the NL count.

This preserves `ls -a in my home` as a probable command despite `in my home` looking like natural language.

The heuristic baseline alone reportedly disambiguates ~80% of inputs without ML. Whether the remaining 20% justifies always-on ML inference isn't clear from public data.

## Options

### Option A: `?`-only (deterministic, original idea-05 stance)

User explicitly opts into NL with `?` prefix. Anything else is shell.

**Pros:**

- Zero ambiguity. User intent is unambiguous from the keystroke.
- No classifier infrastructure (no wordlists, no ML, no thresholds to tune).
- Preserves the "deterministic rules, no AI needed" stance of idea 05.
- Local-first by default.

**Cons:**

- Friction for users who expect "type English, get a command" UX (Warp's headline behavior).
- Cannot serve a hands-off "I forgot the command" workflow.

### Option B: Heuristic-only classifier (no ML, always-on)

Ship Warp's wordlist-and-syntax heuristic. Classify every keystroke; route NL to the AI backend automatically.

**Pros:**

- ~80% disambiguation with no ML, no model files, no runtime cost beyond a small wordlist scan.
- Pure data dependency (wordlists ship as static assets). No `onnx` / `fasttext` linking.
- Works offline (no AI backend required to _classify_; only to _resolve_).

**Cons:**

- ~20% misclassifications mean occasional wrong-mode behavior; confusing if it auto-routes a typo to AI.
- Auto-routing requires AI backend configured; users without one would get errors instead of shell execution.

### Option C: Full ML classifier (Warp's stack)

Heuristic + fasttext + ONNX, layered.

**Pros:**

- Highest accuracy.
- Tracks shell-vs-NL drift over time as users' patterns are learned.

**Cons:**

- Model files (~MBs) bloat the binary or require runtime download.
- ONNX runtime is a heavy dep (~10–20 MB linked).
- Per-keystroke inference latency, even small, adds up across the ghost-text hot path.
- Ships ML inference for what is, for most users, a bounded problem.

### Option D: Layered — `?` explicit, heuristic for ambiguity, ML opt-in via plugin

`?` stays as the explicit override. The heuristic classifier ships built-in but only fires when `?` is _not_ used and the input is ambiguous (i.e., neither obviously shell nor obviously NL by syntax). For users who want stronger disambiguation, an `input-classifier-ml` plugin (community-maintained, opt-in) layers on top.

**Pros:**

- `?` semantics preserved for users who want explicit control.
- Heuristic baseline serves the "type English, get a command" workflow without ML.
- ML is opt-in, not bundled — keeps the core binary lean and dependency-free.
- Wordlists ship as data; no runtime ML required for default behavior.
- Plugin escape hatch matches OakTerm's plugin philosophy (idea 06).

**Cons:**

- Two paths to NL (`?` and auto-routing) — two semantics for users to learn.
- Mitigation: both paths converge on the same AI backend; the _only_ difference is whether the user typed `?` themselves.

## Decision

**Option D — layered: `?` explicit, heuristic auto-classify, ML opt-in via plugin.**

### Behavior

1. **`?` prefix** — explicit, user-controlled, always routes to AI backend. Unchanged from idea 05's original design.

2. **Heuristic classifier** — runs on input that doesn't start with `?`. Returns `ClassificationResult { p_shell, p_ai }` where the two probabilities sum to 1.0. Routing uses a single threshold on `p_ai`, with shell as the safe default:
   - If `p_ai > ai_threshold` AND AI backend is configured → AI mode (show ghost text suggestion from LLM).
   - Otherwise → shell mode (default behavior; show ghost text from completer). This covers both clearly-shell inputs (`p_shell` high) and ambiguous inputs (`p_ai` below threshold).

   The `ai_threshold` is set high enough that auto-routing only fires on clearly-NL inputs. Shell is the safe fallback for everything that isn't a confident AI classification. Keying the decision on `p_ai` directly (rather than on a derived `confidence = max(p_shell, p_ai)` against two thresholds) avoids the failure mode where a high-`p_ai` input gets misrouted to shell mode because a `shell_threshold` arm fires first.

3. **ML opt-in** — community plugin `input-classifier-ml` registers a higher-accuracy classifier via a plugin primitive (`input.classifier(fn)`). When installed and enabled, replaces the heuristic. Not bundled.

### Defaults

- Default `ai_threshold = 0.85` (or similar high value, to be tuned in the spec). Auto-routing is conservative — most inputs flow to shell mode unless `p_ai` clearly dominates.
- Users without an AI backend never auto-route to AI (no error). The classifier silently drops AI suggestions.
- Sticky mode (Warp's `Context { current_input_type, is_agent_follow_up }`) is **not** part of the default behavior. Every keystroke is classified independently. Sticky mode is a follow-up enhancement to evaluate after real usage.

### Heuristic implementation

- Three wordlists: English, StackOverflow, command names. Shipped as static text data, embedded via `include_bytes!`.
- Stemming: `rust_stemmers` crate (Apache 2.0 / MIT — verify before adoption).
- Scoring: `# NL words - # tokens with shell syntax`, with the first-token-is-command exclusion rule.
- Per-keystroke cost: O(tokens) string scans, no allocations beyond a small Vec. Single-digit microseconds.

### Plugin primitive

A new core primitive in idea 06:

```text
input.classifier(fn)   # register a classifier; later registrations stack/override
```

The classifier callback returns `ClassificationResult`. The runtime uses the highest-priority registered classifier; if none, the built-in heuristic.

## Consequences

### Idea-doc updates

- [05-context-engine.md](../ideas/05-context-engine.md): mark Q2 in Open Questions as resolved by this ADR. Update the Natural Language section to describe both `?` and auto-classification with the threshold model.
- [06-plugins.md](../ideas/06-plugins.md): add `input.classifier` to the Context Engine primitives list.
- [18-shell-integration.md](../ideas/18-shell-integration.md): cross-reference if NL classification interacts with shell-integration events (it doesn't directly, but command exit codes feed the wordlist over time).

### New work surfaced

- **Spec candidate** `oakterm-input-classifier`: wordlist format, scoring algorithm, threshold defaults, configuration surface in Lua (`classifier = { ai_threshold = 0.85, ...}`).
- **Trekker task**: heuristic implementation in the daemon (idea 05's sidecar). Modest scope: ~1,000–2,000 lines Rust.
- **Wordlist sourcing**: Warp's wordlists are AGPL-tainted — re-derive from public sources (Apache-licensed English wordlists, top-1000 StackOverflow tags, OakTerm's own command catalog). Coverage report needed to confirm parity.

### What gets easier

- Heuristic + `?` covers the realistic UX without any ML stack.
- ML adoption stays a community decision rather than a core commitment.
- Idea 05's "deterministic, no AI needed" stance is preserved at the _core_ level. AI is opt-in via either `?` or threshold-triggered auto-route, both of which the user can disable.

### What gets harder

- Two paths to NL (`?` and auto) requires careful UX docs so users understand both.
- Heuristic accuracy bounds the auto-route quality. Users who expect Warp-grade disambiguation will need the ML plugin.

## References

- [Idea 05 — Context Engine](../ideas/05-context-engine.md)
- [Idea 06 — Plugins](../ideas/06-plugins.md)
- [Idea 18 — Shell Integration](../ideas/18-shell-integration.md)
- [Warp OSS Architecture Review (2026-04-28)](../reviews/2026-04-28-210532-warp-architecture-review.md)
- [ADR-0013 — Fig Autocomplete Schema](0013-fig-autocomplete-schema.md)
