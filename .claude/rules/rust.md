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

## Module Organization

Split by cohesion and public surface, not by line count. Rust has no file-size
norm — the Book's guidance is to move a module to its own file "when modules get
large" to aid navigation, and it is idiomatic for one module to hold many
structs, enums, `impl` blocks, and functions with related functionality. Don't
port the one-item-per-file habit from TypeScript.

The signal to split is **a file that has stopped being one cohesive concept, or
whose interface to sibling modules has gone muddy** — not a threshold. A large
module that does one thing with a narrow `pub(crate)` surface and its tests
inline is fine; a small module doing three unrelated jobs should be split.

Watch the privacy cost: items in the same module reach each other's private
fields and helpers for free. Splitting to shrink a file, when the pieces are
still coupled, forces `pub(crate)` promotions that leak internals and make
encapsulation worse. Only carve off a unit whose public surface is genuinely
small.

- Tests stay in the same file as the code they test (`#[cfg(test)] mod tests`);
  their lines don't count toward "too big."
- Keep crate roots (`lib.rs` / the binary root) mostly `mod` + `pub use`.
- `clippy::too_many_lines` is per-function (the complexity signal Rust actually
  lints), never per-file.

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
