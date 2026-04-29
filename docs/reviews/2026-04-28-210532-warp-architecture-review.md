---
title: Warp OSS Architecture Review
date: 2026-04-28T21:05:32
scope: warpdotdev/warp client codebase — Phase 2 (plugins) and Phase 3 (shell intelligence + context engine) prior art
---

# Warp OSS Architecture Review

## Scope

Warp open-sourced its client in April 2026. It is the closest commercial-grade prior art for OakTerm's Phase 2 (plugins) and Phase 3 (shell intelligence + context engine). This review reads ~20 of Warp's ~60 crates focused on the agent, context, classifier, persistence, IPC, and isolation layers, and skims the UI framework. Goal: extract architectural patterns OakTerm can clean-room implement.

**License constraint.** Warp is **AGPL-3.0** for everything except `crates/warpui_core` (**MIT**). OakTerm is **MPL-2.0**. AGPL forbids both code copying and linking; we cannot vendor or depend on Warp crates without relicensing OakTerm. This review treats Warp as study-only and flags patterns that are too implementation-specific to clean-room.

**Method.** Read Cargo.toml + lib.rs/mod.rs + 1-2 representative files per crate via `gh api` and `curl`. Patterns mapped against existing OakTerm idea docs (05-context-engine, 06-plugins, 07-agent-management, 08-command-palette, 18-shell-integration, 27-harpoon) and ADRs 0001-0012.

## Findings

### Validated Decisions

#### Custom VT model over Alacritty wrapping

Despite Warp's marketing about wrapping Alacritty, `crates/warp_terminal` uses the `vte` crate directly with a custom `model::grid::flat_storage` backing. Modules are `model`, `shared_session`, `shell`; no Alacritty in sight. OakTerm's decision (since Phase 0) to build a custom handler + grid on top of `vte` rather than vendor `alacritty_terminal` mirrors what Warp ships.

Independent reference for [ADR-0006 scroll buffer architecture](../adrs/0006-scroll-buffer-architecture.md): if Warp ships a custom flat-storage grid, the choice is sound.

#### `SumTree<T>` for command history / scrollback indexing

`crates/sum_tree` is a B-tree-with-summaries structure (Zed lineage, MIT-licensed in upstream). Trait shape:

```rust
trait Item {
    type Summary: AddAssign<&Summary> + Default + Clone;
    fn summary(&self) -> Self::Summary;
}
trait KeyedItem { type Key: Dimension + Ord; }
```

Summaries amortize "find by line number / timestamp / exit code" to O(log n) with O(1) streaming inserts. Use this for [`docs/ideas/27-harpoon.md`](../ideas/27-harpoon.md) (command history harpoon); it would also simplify the [scroll/archive_manager.rs](../../crates/oakterm-terminal/src/scroll/archive_manager.rs) hot-buffer indexing. Borrow from Zed (MIT) directly, not from Warp.

#### Splitting JSON-RPC and IPC transports

Warp keeps `crates/jsonrpc` (LSP, JSON-over-stdio) and `crates/ipc` (plugin host, bincode-over-UDS) as **separate crates** with different wire formats. The plugin host service uses bincode framing with an opaque `bytes: Vec<u8>` payload so each service chooses its own serialization. The implicit OakTerm direction in [`docs/ideas/06-plugins.md`](../ideas/06-plugins.md) — distinct LSP-style vs plugin-RPC paths rather than one omnibus protocol — matches Warp.

### Challenged Decisions

#### Whether plugin IPC should be UDS-bincode or stdio-protobuf

OakTerm has chosen `prost` (protobuf) for the wire protocol (`workspace.dependencies.prost = "0.14.3"`). Warp's `crates/ipc` uses **bincode-over-Unix-Domain-Sockets** for plugin host RPC, with `Request { id: Uuid, service_id, bytes }` framing and `/tmp/warp-ipc-{rand}.sock` addressing. Bincode is faster but Rust-only and has zero schema evolution discipline. Protobuf forces schema thinking and lets non-Rust plugins join later.

**Challenge:** the OakTerm decision is correct, but borrow Warp's `Service` / `ServiceCaller` / `ServiceImpl` trait split for the RPC ergonomics layer above the wire format. ADR candidate.

#### "Command-as-block" implementation strategy

OakTerm's [`docs/ideas/08-command-palette.md`](../ideas/08-command-palette.md) and idea 27 imply some block-like command model. Warp's actual implementation is split across:

- `crates/warp_terminal::model::BlockId` / `BlockIndex` — terminal-side identity
- `crates/persistence` — `blocks` table (SQLite via diesel)
- `crates/warp_completer::ParsedTokensSnapshot` — token-level structure

Warp does **not** have a single "block" crate. Blocks emerge from the intersection of grid, persistence, and parser. This challenges the assumption that blocks are a single bounded concern; they may be a _cross-cutting_ concept that reaches into three OakTerm crates (`oakterm-terminal`, future `oakterm-store`, `oakterm-shell-integration`).

### Missing Specs

These OakTerm gaps are now sharply defined by Warp's prior art:

#### Skill / agent-context provider registry (maps to idea 05)

Warp's `crates/ai/src/skills/skill_provider.rs:104-148` defines a precedence-ordered enum:

```rust
SkillProvider::{Warp, Agents, Claude, Codex, Cursor, Gemini, Copilot, Droid, Github, OpenCode}
```

mapped to filesystem conventions (`.warp/skills`, `.claude/skills`, `.codex/skills`, `.cursor/skills`, `.gemini/skills`, `.copilot/skills`, `.factory/skills`, `.github/skills`, `.opencode/skills`). `SkillScope::{Home, Project, Bundled}` distinguishes `~/.agents/skills/`, `./repo/.agents/skills/`, and binary-bundled skills. `get_provider_for_path()` walks path components.

OakTerm has no spec for this, but [`docs/ideas/05-context-engine.md`](../ideas/05-context-engine.md) needs it. **Spec candidate**: `oakterm-skill-providers` covering provider list, path layout, scope precedence, and bundled-skill embedding.

#### Project-rule discovery (maps to idea 05)

`crates/ai/src/project_context/model.rs:62-99` walks the repo for `WARP.md` / `AGENTS.md` (case-insensitive) up to `MAX_SCAN_DEPTH=3` and `MAX_FILES_TO_SCAN=5000`. Returns:

```rust
ProjectRulesResult {
    root_path,
    active_rules: Vec<RuleAtPath>,        // ancestor of edited file
    additional_rule_paths: Vec<PathBuf>,  // advertised but not auto-injected
}
RuleAtPath { warp_md, agents_md }  // warp_md takes precedence
```

**Summaries** of inactive rules are sent to the LLM so it can request them on demand, instead of injecting everything. The active-vs-available split is what makes the prompt budget tractable. **Spec candidate**: `oakterm-project-rules` with the depth caps, precedence, and active/available split.

#### NL-vs-shell input classifier (maps to ideas 07, 18)

`crates/input_classifier::ClassificationResult { p_shell, p_ai }` with backends `HeuristicClassifier`, `FasttextClassifier`, `OnnxClassifier`. The pure-Rust heuristic in `crates/natural_language_detection/src/lib.rs:36-71` uses three wordlists (English, StackOverflow, Command), stems via `rust_stemmers`, and scores `# NL words - # tokens with shell syntax`. At lines 43-50:

> If the first token is a known command and not in `RESERVED_KEYWORDS = ["what"]`, that token is excluded from the NL count.

This preserves "ls -a in my home" as a probable command despite "in my home" being natural language.

OakTerm has no idea doc covering input classification yet. **Idea-doc candidate**: NL-vs-shell mode disambiguation, with the heuristic baseline as v1 and ML as a later opt-in.

#### Agent action enum with self-supplied risk metadata (maps to idea 07)

`crates/ai/src/agent/action/mod.rs:32-170` defines `AIAgentActionType` with variants like:

```rust
RequestCommandOutput {
    command, is_read_only, is_risky, wait_until_completion,
    uses_pager, rationale, citations,
}
```

The model classifies its own actions as risky / read-only / needs-pager, encoded in the wire schema. The client doesn't guess from command strings. The other variants (28 total) form a shopping list for tool calls a terminal-native agent needs.

**Spec candidate**: `oakterm-agent-action-protocol`. The _idea_ of self-classifying tool calls is borrowable. The exact variant list is AGPL-tainted (see below); re-derive from OakTerm's threat model.

### Top 5 Borrow-Worthy Patterns

1. **Cross-vendor skill registry** (idea 05/06). Borrow `SkillProvider` enum shape, `SkillScope::{Home, Project, Bundled}`, path-component walking. Add `Oak` provider. The list and path layout are facts about _other_ tools; not Warp's IP.

2. **Path-scoped rule engine with active/available split** (idea 05). Borrow `MAX_SCAN_DEPTH=3`, `MAX_FILES_TO_SCAN=5000`, the `RuleAtPath` structure, and the active-vs-available distinction. Algorithm is generic; rename the file.

3. **NL-vs-shell heuristic baseline** (ideas 07, 18). Three-wordlist scoring, `RESERVED_KEYWORDS = ["what"]` carve-out, "first-token-is-command excludes from NL count" rule. Pure heuristic gets ~80% before any ONNX model is needed.

4. **Agent action contract with LLM-self-supplied risk** (idea 07). Model declares `is_read_only` / `is_risky` / `uses_pager` / `wait_until_completion` per call. The contract is the part to borrow, not the variant list.

5. **`SumTree<T>` for indexed history/scrollback** (idea 27, ADR-0006). Borrow from upstream Zed (MIT), not from Warp. Use for harpoon command index, scroll archive metadata, future block index.

### Top 3 Cloud-Coupled Traps to Avoid

1. **Don't fold cloud sandbox tokens into a local isolation detector.** `crates/isolation_platform` looks like a local Docker/k8s detector but is actually the entry point for issuing `WorkloadToken`s to Warp's hosted Docker Sandboxes / Namespace.so instances (`issue_workload_token`, lines 113-134). For OakTerm: container _detection_ is a fine local feature; _workload tokens_ are a cloud abstraction we don't need.

2. **Don't ship a JS/TypeScript completion engine via `rust-embed` + yarn build.** `crates/command-signatures-v2` requires Node 18.14.1, `corepack`, and `yarn`, with multiple paragraphs of `build.rs` panic copy explaining the toolchain. For a Rust-MPL terminal this is a serious build-system tax. Use a TOML/JSON command-shape format read at runtime instead.

3. **Don't co-mingle local-only and team/cloud rows in one SQLite schema.** `crates/persistence/src/model.rs` declares 40+ tables in a single Diesel schema covering `team_members`, `team_settings`, `teams`, `workspace_teams`, `cloud_objects_refreshes`, etc. all behind one `local_fs` feature flag. Splitting the schema along the cloud boundary from the start saves a giant migration later. Relevant if/when OakTerm grows team-sync features.

### AGPL-Tainted Patterns (Re-Derive, Don't Mirror)

- **Full layout of `AIAgentActionType` variant-by-variant.** The _idea_ of per-action risk metadata is fine; the exact variant list, field names, and ordering is specific enough that mirroring it risks derivation. Re-derive from OakTerm's threat model.
- **`crates/warp_terminal::shell::mod.rs` shell-bootstrap DCS payload.** The "Bootstrapped" handshake (shell sends type/version/options/plugins/path back through DCS) is a wire contract Warp ships shell-side hooks for. Pick a different escape sequence and your own field set.
- **`SumTree` Cursor/FilterCursor surface.** Already MIT in Zed, so AGPL isn't really the issue — but copy from **Zed**, not from Warp's repo, so the upstream license is unambiguous.
- **`warpui_core`** is MIT despite its workspace being AGPL. Legally borrowable, but socially fraught: pulling MIT-tagged subcrate files out of an AGPL repo invites a license dispute even if formally fine. Borrow patterns, not files.

### Specs Methodology

Warp's `specs/` directory has 123 `APP-NNNN` directories (Linear ticket-prefixed), each containing `PRODUCT.md` + `TECH.md`. Sample titles: APP-1915 ("Copy URL / Copy path in AI response right-click context menu"), APP-3076 ("Block List Markdown Table Rendering"), APP-3637 ("CLI Agent Rich Input: /skills").

PRODUCT.md mandatory sections: Title, Summary, Problem, Non-goals (sometimes Goals), Figma, User Experience / Behavior (numbered behavioral assertions — APP-1915 has 11), Success Criteria, Validation, Open Questions. **Behavioral assertions are numbered and testable**, not prose.

This matches OakTerm's existing `docs/specs/` discipline (typed definitions, no hand-waving). Consider the ticket-prefixed directory layout for OakTerm: pair each spec with its trekker task ID.

## Action Items

### Idea Doc Updates

- **`docs/ideas/05-context-engine.md`** — add sections on (a) cross-vendor skill provider registry, (b) path-scoped project rule discovery with active/available split. Cite Warp findings.
- **`docs/ideas/06-plugins.md`** — add reference to Warp's `Service` / `ServiceCaller` / `ServiceImpl` ergonomics layer above the protobuf wire format.
- **`docs/ideas/07-agent-management.md`** — document the LLM-self-supplied risk metadata pattern.
- **`docs/ideas/18-shell-integration.md`** OR new idea doc — NL-vs-shell input classification, heuristic baseline before ML.

### ADR Candidates

- **ADR: Skill provider precedence and path layout** — formalizes which `.{provider}/skills/` directories OakTerm reads, in what order, with what scope precedence (Home / Project / Bundled).
- **ADR: Project rule discovery — depth caps, file precedence, active vs available** — `OAK.md` / `AGENTS.md`, `MAX_SCAN_DEPTH`, summary-vs-injection split.
- **ADR: Agent action protocol — self-classifying risk metadata** — confirms whether OakTerm's tool-use schema includes `is_read_only` / `is_risky` / etc. as model-supplied fields.

### Spec Candidates

- `oakterm-skill-providers` — skill provider registry (after ADR accepted)
- `oakterm-project-rules` — project-rule discovery algorithm (after ADR accepted)
- `oakterm-agent-action-protocol` — agent tool-call wire schema (after ADR accepted)

### Trekker

- File icebox task: "Re-evaluate Warp competitive landscape at Phase 2 and Phase 3 boundaries" — Warp will keep evolving; revisit when OakTerm reaches each phase gate.

### No Action

- **Don't migrate to libghostty.** (Separate review, decided 2026-04-28.)
- **Don't adopt warpui.** Same trap; would scrap wgpu/swash investment.
- **Don't borrow `command-signatures-v2`.** JS/yarn build tax not worth it. The eventual format decision is captured in [ADR-0013](../adrs/0013-fig-autocomplete-schema.md) (build-time TS→JSON via oxc).
