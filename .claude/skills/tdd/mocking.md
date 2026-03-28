# When to Mock

Mock at **system boundaries** only:

- External APIs
- File system (sometimes)
- Time/randomness
- Platform APIs (PTY, windowing)

Don't mock:

- Your own crates/modules
- Internal collaborators
- Anything you control

## Rust Patterns

Use trait objects or generics for dependency injection at boundaries:

```rust
// Testable: accepts any impl
fn process<S: TextShaper>(shaper: &S, run: &TextRun) -> Vec<ShapedGlyph> {
    shaper.shape(run)
}

// Hard to test: creates dependency internally
fn process(run: &TextRun) -> Vec<ShapedGlyph> {
    let shaper = HarfBuzzShaper::new();
    shaper.shape(run)
}
```

For testing, use test doubles in the same crate:

```rust
#[cfg(test)]
struct MockShaper;

#[cfg(test)]
impl TextShaper for MockShaper {
    fn shape(&self, _run: &TextRun) -> Vec<ShapedGlyph> { vec![] }
    // ...
}
```
