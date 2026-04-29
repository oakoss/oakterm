---
adr: '0013'
title: Fig Autocomplete Schema
status: proposed
date: 2026-04-28
tags: [context-engine, completion, plugins]
---

# 0013. Fig Autocomplete Schema

## Context

[Idea 05 — Context Engine](../ideas/05-context-engine.md) lists four Open Questions blocking Phase 3 work. Three of them constrain each other:

1. **Signature schema shape** — adopt [Fig's autocomplete schema](https://github.com/withfig/autocomplete) (MIT, ~600+ existing specs) or design our own.
2. **Input classifier** — deferred to [ADR-0014](0014-input-classifier.md).
3. **Baseline location** — separate crate vs embedded constants in the daemon.
4. **Storage format** — JSON, TOML, Lua DSL, or Rust constants compiled in.

This ADR resolves Q1, Q3, and Q4 as a single decision because the schema choice cascades into format and location.

### Spike findings

A Rust binary using [oxc](https://github.com/oxc-project/oxc) parsed all 1,484 Fig spec files in `withfig/autocomplete`. It walked each spec's AST and classified the `script`, `generators`, `postProcess`, `custom`, `trigger`, and `getQueryTerm` fields, recording import dependencies along the way. Results:

| Measurement                                                                | Value                            |
| -------------------------------------------------------------------------- | -------------------------------- |
| Files parsed via oxc                                                       | 1,484 / 1,484 (100%, ~3 seconds) |
| `script` fields that are pure string-literal arrays                        | 86.6%                            |
| `generators` fields that are named references (member + identifier + call) | 94%                              |
| `postProcess` auto-convertible (Tier A) or DSL-pattern-matched (Tier B)    | ~76%                             |
| `postProcess` requiring named hooks (Tier C)                               | ~24%                             |
| Distinct external helper modules referenced                                | 9                                |
| Top 2 modules' share of external helper usage                              | 93.3%                            |
| Top 6 modules' share of external helper usage                              | 99.6%                            |
| Total `custom` async functions in catalog                                  | 148                              |

The spike confirmed two things: the catalog parses cleanly in pure Rust, and no JS runtime is needed in OakTerm.

### Warp comparison

Warp adopted the same Fig-derived schema in `crates/command-signatures-v2` but executes it via JS-in-a-separate-plugin-host process (see `app/src/completer/js.rs`: `CallJsFunctionService` IPC). This requires `yarn build` at compile time and a JS engine (in their plugin host) at runtime. OakTerm intentionally diverges: pure-Rust pipeline end to end.

## Options

### Q1: Schema shape

#### Option A: Adopt Fig's autocomplete schema

Use Fig's TS schema as the canonical signature shape. Inherit ~600+ specs.

**Pros:**

- ~600+ specs free, including major tools (git, kubectl, docker, npm, gcloud, aws, az, deno).
- Cross-tool consistency: same shape Warp, Amazon Q, and Fig itself use; users get a familiar mental model.
- Active community keeps specs current as commands evolve.
- Empirically validated parseable in pure Rust (oxc, 100% pass rate).

**Cons:**

- ~24% of `postProcess` functions need hand-porting (Tier C); not zero-cost adoption.
- Schema design control is given up; if Fig changes shape, we have to follow or fork.

#### Option B: Custom OakTerm schema

Design a fresh schema and build the catalog ourselves.

**Pros:**

- Full design control. No external dependency.
- Schema can be tuned for Rust ergonomics from day one.

**Cons:**

- Build the catalog from zero. Realistic effort: hundreds of person-hours just for the popular commands.
- Throws away the strongest argument (Fig's existing catalog).
- Cross-tool inconsistency: users coming from Warp/Amazon Q have a different mental model.

#### Option C: Hybrid — Fig schema, OakTerm-authored catalog

Adopt the schema; ignore the catalog.

**Pros:**

- Familiar shape, full control over content.

**Cons:**

- Same catalog-from-zero problem as Option B.
- "Why adopt Fig if not for the catalog?" The schema alone isn't unique enough to justify the dependency.

### Q3: Baseline location

#### Option A: Embedded constants in the daemon binary

Convert specs at build time into Rust source code (or `phf`/`include_str!`'d JSON) compiled directly into the daemon.

**Pros:**

- Fastest startup (no file I/O, no parse step).
- Single binary, no extra files to ship.

**Cons:**

- Binary bloat: ~600+ specs add measurable size.
- Slow incremental compile when specs change.
- Mixes spec data with daemon code.

#### Option B: Separate `oakterm-completer-baseline` crate with checked-in JSON

A workspace crate that owns the converted JSON and exposes it as a static dataset. The daemon depends on this crate.

**Pros:**

- Clean separation: spec data lives in one place, daemon code lives elsewhere.
- The crate can use `include_bytes!` to embed JSON into its compiled artifact, getting Option A's startup speed.
- Easier to swap implementations (e.g., dynamic loader for testing) by feature-gating.
- Updates to the catalog don't trigger daemon recompile.

**Cons:**

- One more crate to maintain.

### Q4: Storage format

#### Option A: JSON

The natural output of a TS-AST-walking converter. Universal tooling, schema-validatable via JSON Schema.

**Pros:**

- Falls out of Fig's TS structure with minimal transformation.
- Schema-validatable (`schemars`/JSON Schema).
- Loads into Rust via `serde_json` without ceremony.
- Plugin authors can write or generate JSON without OakTerm-specific tooling.

**Cons:**

- Verbose vs TOML.
- Less human-pleasant than Lua/TOML for hand authoring.

#### Option B: TOML

**Pros:**

- More readable for hand-authored specs.

**Cons:**

- Awkward for nested arrays (subcommands within subcommands within options); Fig specs are deeply nested.
- Requires extra translation step from Fig's natural shape.

#### Option C: Lua DSL on the config side

**Pros:**

- Power users could author specs in Lua alongside config.

**Cons:**

- Couples spec authoring to Lua sandbox; orthogonal concern.
- Plugins can't easily generate specs at install time.

#### Option D: Rust constants compiled into binary

**Pros:**

- Maximally fast.

**Cons:**

- Specs become source code. Unmaintainable by non-Rust contributors.
- Eliminates plugin-contributed specs unless we run a Rust compiler at runtime.

## Decision

**Q1: Option A — adopt Fig's autocomplete schema and catalog.**
**Q3: Option B — separate `oakterm-completer-baseline` crate.**
**Q4: Option A — JSON, embedded via `include_bytes!`.**

Together these answer Q1, Q3, and Q4 with one architecture: a **three-tier execution model** fed by a **pure-Rust build-time converter**.

### Three-tier execution model

| Tier                     | What it covers                                                                                                                                                                                     | How it runs at runtime                                                                             |
| ------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| **A — Static data**      | Positionals, options, descriptions, simple generator references. ~70% of catalog by spec count.                                                                                                    | Direct deserialization from JSON; no logic.                                                        |
| **B — Declarative DSL**  | Common `postProcess` patterns: `split_map`, `json_extract`, `regex_extract`, `column_split`, `filter_then_map`. ~10–15 named primitives expressed as data. ~65% of inline `postProcess` functions. | Native Rust DSL interpreter; sub-millisecond per call.                                             |
| **C — Named Rust hooks** | Bespoke logic that can't be DSL-expressed. Spec references a hook by name (`{"hook": "git_branches"}`); hook resolved against a registry at runtime.                                               | Direct Rust function call. WASM plugins (idea 06) can register additional hooks for the long tail. |

When a spec references a hook that isn't registered, the spec falls back to Tier A behavior (data-only completion for that argument). Specs are never disabled wholesale by missing hooks.

### Build-time converter

A Rust binary (working name: `oakterm-fig-converter`) parses Fig's TS specs via oxc, walks the AST, and emits JSON. For each spec:

1. **Static fields** (`name`, `description`, `args`, `options`, subcommands) → JSON as data.
2. **`script` arrays of string literals** → JSON arrays as-is.
3. **`generators` references** → JSON with `{"helper": "<name>"}` referencing the hook registry.
4. **`postProcess` functions** → AST pattern-matched against the DSL primitives. If a match: emit DSL JSON. If not: emit `{"hook": "<auto-generated-name>"}` and add the hook name to a coverage report.
5. **`custom` async functions** → always Tier C; emit hook reference and coverage entry.

The converter ships in OakTerm's repo (likely `tools/oakterm-fig-converter/` or as a workspace member). It runs once per Fig upstream version against a vendored or pinned `withfig/autocomplete` checkout. Its output (JSON files plus a coverage report listing required hooks) is checked into the `oakterm-completer-baseline` crate. **End users do not run the converter; they load the pre-converted JSON.**

### `oakterm-completer-baseline` crate

```text
crates/oakterm-completer-baseline/
├── Cargo.toml
├── data/
│   ├── git.json
│   ├── kubectl.json
│   ├── ...               # one JSON file per Fig spec, generated
│   └── hooks-required.json   # coverage report
├── src/
│   ├── lib.rs            # include_bytes! the data, expose as &'static [Spec]
│   ├── dsl/              # Tier B DSL primitive types and interpreter
│   ├── hooks/            # Tier C built-in Rust hook registry
│   │   ├── git.rs
│   │   ├── npm.rs
│   │   └── shared.rs
│   └── schema.rs         # Spec, Option, Argument, Generator types
```

The daemon depends on this crate. The completer plugin (idea 05) loads `&'static [Spec]` and runs Tier A/B/C resolution per completion.

### Coverage targets

| Phase     | Work                                                                                                             | Spec functional coverage |
| --------- | ---------------------------------------------------------------------------------------------------------------- | ------------------------ |
| Day 1     | Converter binary; port `@fig/autocomplete-generators` (~12 stock functions); port `./shared`; ~10 DSL primitives | ~80–85%                  |
| Phase 2   | Port `./npm`, `./git`, `./deno`, `./yarn`; expand DSL primitives based on coverage report gaps                   | ~95%                     |
| Long tail | Hand-port or skip remaining bespoke `custom` functions                                                           | ~98–99%                  |

The 9 external helper modules surfaced by the spike define the porting universe. Empirically: top 2 modules cover 93.3% of external helper usage; top 6 cover 99.6%. There is no scenario where porting work explodes beyond a knowable, finite list.

### What this explicitly excludes

- **No JavaScript runtime in OakTerm** — neither linked nor sidecar.
- **No Node/yarn dependency** in OakTerm's build pipeline. The converter is pure Rust (oxc).
- **No IPC for completion** — Tier A/B/C all run in-process.
- **No on-startup spec parsing cost** — JSON is `include_bytes!`'d and deserialized once.

## Consequences

### Idea-doc updates

- [05-context-engine.md](../ideas/05-context-engine.md): mark Q1, Q3, Q4 in Open Questions as resolved by this ADR. Strike the _"sketch below paraphrases Warp's `CommandSignature`; the final shape is decided by ADR"_ hedge in the Command signatures section. Status moves from `reviewing` to `decided` when this ADR is accepted (along with [ADR-0014](0014-input-classifier.md)).
- [06-plugins.md](../ideas/06-plugins.md): clarify that the Context Engine primitives (`context.signature`, `context.generator`) operate over Fig-shaped JSON, and that plugin-contributed specs use the same schema.

### New work surfaced

- **Spec candidate** `0011-fig-converter-architecture.md`: the build-time converter binary's contract — input format (vendored Fig commit hash), output structure, coverage report shape, error handling for unsupported patterns.
- **Spec candidate** `0012-completer-dsl.md`: the Tier B DSL primitive set, semantics, and JSON encoding.
- **Trekker epic** for Phase 3 work: converter binary, `oakterm-completer-baseline` crate scaffolding, stock helper ports, DSL implementation. Estimate: ~30–60 person-hours to reach the Day-1 coverage target above.
- **Spike code retention**: the classifier built during this ADR's discovery phase (parsing 1,484 Fig specs via oxc) should be relocated into the OakTerm repo as the converter binary's seed when the converter spec is opened. The classifier's pattern-detection logic is the empirical evidence behind this ADR.

### What gets easier

- Phase 3 implementation has a clear contract.
- Plugin authors can ship JSON specs without learning OakTerm-specific syntax.
- Open-source contributors can submit hooks or DSL extensions without owning the daemon.

### What gets harder

- We are now coupled to Fig's schema. Schema breakage upstream forces converter updates.
- The converter must track Fig upstream versions; expect a CI job or pinned-version process for re-conversion.

## References

- [Idea 05 — Context Engine](../ideas/05-context-engine.md)
- [Idea 06 — Plugins](../ideas/06-plugins.md)
- [Warp OSS Architecture Review (2026-04-28)](../reviews/2026-04-28-210532-warp-architecture-review.md)
- [Fig autocomplete repository](https://github.com/withfig/autocomplete)
- [oxc — JavaScript/TypeScript parser in Rust](https://github.com/oxc-project/oxc)
- [Warp `command-signatures-v2`](https://github.com/warpdotdev/warp/tree/master/crates/command-signatures-v2) (architecture comparison only — AGPL, not adopted)
