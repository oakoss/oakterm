---
paths:
  - '**/*.rs'
  - '**/Cargo.toml'
---

# Rust Patterns

## Clippy Pedantic

These fire on every Rust file. Handle inline during GREEN step, not in a later pass.

- `doc_markdown`: backtick `CamelCase` type names in doc comments
- `cast_possible_truncation`: use `try_into()` instead of `as u16` on `.len()`. Never `debug_assert` + `as` — silently truncates in release
- `must_use_candidate`: add `#[must_use]` to pure functions returning values
- `missing_panics_doc`: add `# Panics` section if using `.expect()`/`.unwrap()`
- `checked_conversions`: use `u16::try_from(x).is_ok()` not `x <= u16::MAX as usize`
- `similar_names`: rename variables if clippy flags them
- `items_after_statements`: put `use` imports at top of scope, not after statements
- `field_reassign_with_default`: use struct update syntax `Foo { field: val, ..Default::default() }`

## Encoding Pattern

Any `as u16` or `as u32` on a `.len()` is a truncation bug in release mode. Always:

```rust
let len: u16 = data.len().try_into().map_err(|_| {
    io::Error::new(io::ErrorKind::InvalidInput, "data exceeds u16")
})?;
```

## Workspace Lints

`unsafe_code = "deny"` workspace-wide. `oakterm-pty` allows unsafe for PTY `pre_exec`, `oakterm-daemon` for `BorrowedFd::borrow_raw` on the PTY async read. Minimize unsafe blocks; prefer safe abstractions (rustix over raw libc). Future: oakterm-pty should expose a safe async-ready API to eliminate daemon unsafe.

## Bench Fixtures

Benches in `crates/*/benches/` should generate input synthetically by default — see `crates/oakterm-terminal/benches/vt_parser.rs` for the pattern (`make_plain_ascii`, `make_sgr_color`, etc.). Synthetic data lives in code, not in the repo as data, and stays trivially regeneratable.

Commit a captured byte-stream fixture under `benches/fixtures/` only when synthetic generation can't reproduce the failure mode the bench guards against — e.g. real `tree -C` output captures escape distributions and Unicode patterns that are hard to fake.

When committing a fixture:

- Trim aggressively. Target ~50 KB unless the failure mode genuinely needs more.
- Document the capture command + justification in `benches/fixtures/README.md`.
- Mark binary fixtures in `.gitattributes` so git's autocrlf doesn't munge them on Windows checkouts.
