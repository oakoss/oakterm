# Good and Bad Tests

## Good Tests

Test through real interfaces, not mocks of internal parts.

```rust
#[test]
fn print_ascii_writes_at_cursor() {
    let mut grid = test_grid(80, 24);
    process_bytes(&mut grid, b"hello");
    assert_row_text(&grid, 0, "hello");
    assert_cursor_at(&grid, 0, 5);
}
```

Characteristics:

- Tests behavior callers care about
- Uses public API only
- Survives internal refactors
- Describes WHAT, not HOW
- One logical assertion per test

## Bad Tests

Coupled to internal structure.

```rust
// BAD: tests internal field values instead of observable behavior
#[test]
fn print_sets_cell_codepoint() {
    let mut grid = test_grid(80, 24);
    process_bytes(&mut grid, b"a");
    assert_eq!(grid.lines[0].cells[0].codepoint, 'a');
    assert_eq!(grid.lines[0].cells[0].fg, Color::Default);
    assert_eq!(grid.lines[0].cells[0].bg, Color::Default);
    // Testing every field = testing implementation
}

// GOOD: tests the observable outcome
#[test]
fn print_writes_character() {
    let mut grid = test_grid(80, 24);
    process_bytes(&mut grid, b"a");
    assert_row_text(&grid, 0, "a");
}
```

## Adversarial Tests

Every feature needs at least one test for bad input.

```rust
#[test]
fn encode_rejects_oversized_payload() {
    let big = vec![0u8; MAX_PAYLOAD as usize + 1];
    assert!(Frame::new(0x01, 1, big).is_err());
}

#[test]
fn decode_rejects_bad_magic() {
    let mut data = Frame::new(0x01, 1, vec![]).unwrap().encode_to_vec();
    data[0] = 0xFF;
    assert!(Frame::decode_from_slice(&data).is_err());
}
```
