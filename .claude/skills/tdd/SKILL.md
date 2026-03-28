---
name: tdd
description: Test-driven development with red-green-refactor loop. Use when user wants to build features or fix bugs using TDD, mentions "red-green-refactor", wants integration tests, or asks for test-first development.
---

# Test-Driven Development

## Philosophy

Tests verify behavior through public interfaces, not implementation details. Code can change entirely; tests shouldn't.

**Good tests** exercise real code paths through public APIs. They describe WHAT the system does, not HOW. A good test reads like a spec: "print_ascii writes characters at cursor position" tells you the capability. These survive refactors.

**Bad tests** mock internal collaborators, test private methods, or verify through external means. Warning sign: test breaks on refactor but behavior hasn't changed.

See [tests.md](tests.md) for Rust examples and [mocking.md](mocking.md) for boundary guidelines.

## Anti-Pattern: Horizontal Slices

DO NOT write all tests first, then all implementation.

Vertical slices via tracer bullets. One test, one implementation, repeat. Each test responds to what you learned from the previous cycle.

```text
WRONG (horizontal):
  RED:   test1, test2, test3, test4, test5
  GREEN: impl1, impl2, impl3, impl4, impl5

RIGHT (vertical):
  RED→GREEN: test1→impl1
  RED→GREEN: test2→impl2
  RED→GREEN: test3→impl3
```

## Workflow

### 1. Planning

Before writing any code:

- Confirm what interface changes are needed
- Confirm which behaviors to test (prioritize critical paths)
- Identify opportunities for deep modules (small interface, deep implementation)
- Design for testability (accept deps, return results)
- List behaviors to test (not implementation steps)

You can't test everything. Focus on critical paths and complex logic.

### 2. Red

Write ONE failing test. Include at least one adversarial test per feature (oversized input, malformed data, boundary conditions). These catch silent truncation and overflow that happy-path tests miss.

```rust
#[test]
fn encode_rejects_oversized_extra_data() {
    let cell = WireCell { extra: vec![0u8; 70_000], ..Default::default() };
    assert!(cell.encode().is_err());
}
```

### 3. Green

Write minimal code to pass the test. Run `cargo clippy` immediately after — fix lint issues inline, not in a later pass.

```bash
cargo test -p <crate> <test_name>
cargo clippy -p <crate> --all-targets -- -D warnings
```

If clippy flags something, fix it now. Don't move to the next test with warnings.

### 4. Repeat

For each remaining behavior: RED (one failing test) → GREEN (minimal code + clippy).

Rules:

- One test at a time
- Only enough code to pass the current test
- Don't anticipate future tests

### 5. Refactor

After all tests pass, look for refactor candidates (see [refactoring.md](refactoring.md)):

- Extract duplication
- Deepen modules
- Run tests after each refactor step

Never refactor while RED. Get to GREEN first.

## Rust Patterns

### Common clippy-pedantic issues to handle during GREEN

- `doc_markdown`: backtick `CamelCase` type names in doc comments
- `cast_possible_truncation`: use `try_into()` instead of `as u16` on `.len()`
- `must_use_candidate`: add `#[must_use]` to pure functions returning values
- `missing_panics_doc`: add `# Panics` section if using `.expect()`/`.unwrap()`
- `checked_conversions`: use `u16::try_from(x).is_ok()` instead of `x <= u16::MAX as usize`

### Test commands

```bash
# Run one test
cargo test -p oakterm-terminal handler::tests::print_ascii

# Run all tests in a crate
cargo test -p oakterm-terminal

# Run with output
cargo test -p oakterm-terminal -- --nocapture

# Full workspace
mise run test
```

## Checklist Per Cycle

```text
[ ] Test describes behavior, not implementation
[ ] Test uses public interface only
[ ] Test would survive internal refactor
[ ] At least one adversarial test per feature
[ ] Code is minimal for this test
[ ] cargo clippy clean after GREEN
[ ] No speculative features added
```
