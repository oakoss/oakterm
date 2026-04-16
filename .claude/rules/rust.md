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

Benches in `crates/*/benches/` should generate input synthetically by default — see `crates/oakterm-terminal/benches/vt_parser.rs` for the pattern (`make_plain_ascii`, `make_sgr_color`, etc.). Synthetic data lives in code, stays regeneratable, and doesn't bloat git history.

Commit a captured byte-stream fixture under `benches/fixtures/` only when synthetic generation can't reproduce the failure mode the bench guards against — e.g. real `tree -C` output captures SGR-per-line density and Unicode in real filenames that are hard to fake.

When committing a fixture:

- Trim aggressively. ~100 KB target; up to ~250 KB if the failure mode genuinely needs more samples for stable measurement.
- Document the capture command, the size, and explicitly why synthetic doesn't suffice in `benches/fixtures/README.md`.
- Confirm the file's extension is marked `binary` in `.gitattributes` so the workspace's `* text=auto` rule doesn't classify the capture as text and normalize line endings on checkout (the failure mode autocrlf creates on any platform with it configured, most commonly bites Windows).
