# Specifications

Formal contracts for implementation. A spec defines the exact interface, behavior, and constraints that code must satisfy. Trekker tasks reference specs, not idea docs.

## Format

```text
NNNN-short-title.md
```

Numbered sequentially. One spec per bounded concern (an API surface, a wire protocol, a data format).

## Status Lifecycle

```text
draft → review → accepted → implementing → complete
```

- **draft** — being written, not ready for review
- **review** — ready for feedback
- **accepted** — contract is final, implementation can begin
- **implementing** — active implementation in progress
- **complete** — implemented and tested

## Template

Copy [0000-template.md](0000-template.md) and renumber.

## Index

| Spec                                 | Title                        | Status | ADRs           | Tags |
| ------------------------------------ | ---------------------------- | ------ | -------------- | ---- |
| [0001](0001-daemon-wire-protocol.md) | Daemon Wire Protocol         | draft  | 0007           | core |
| [0002](0002-vt-parser.md)            | VT Parser & Terminal Handler | draft  | 0004,0008      | core |
| [0003](0003-screen-buffer.md)        | Screen Buffer                | draft  | 0006,0001,0009 | core |
| [0004](0004-scroll-buffer.md)        | Scroll Buffer & Archive      | draft  | 0006           | core |
