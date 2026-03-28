# Deep Modules

Small interface + lots of implementation.

```text
┌─────────────────────┐
│   Small Interface   │  ← Few methods, simple params
├─────────────────────┤
│                     │
│  Deep Implementation│  ← Complex logic hidden
│                     │
└─────────────────────┘
```

In OakTerm: Grid is a deep module. Complex internal state (cells, rows, dirty tracking, palette) behind a simple interface (new, touch_row, dirty_rows). The handler operates on Grid's API without knowing its internals.

When designing interfaces, ask:

- Can I reduce the number of methods?
- Can I simplify the parameters?
- Can I hide more complexity inside?
