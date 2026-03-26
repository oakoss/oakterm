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

```markdown
---
spec: NNNN
title: Short Spec Title
status: draft
date: YYYY-MM-DD
adrs: [NNNN]
tags: [area tags]
---

# NNNN. Short Spec Title

## Overview

One paragraph: what this spec defines and why it exists.

## Contract

The formal definition. Depending on the spec type:

- **API specs**: function signatures, input/output types, error types, invariants
- **Protocol specs**: wire format, message types, sequencing, error handling
- **Data format specs**: schema, validation rules, migration path

## Behavior

Expected behavior for normal and error cases. Edge cases called out explicitly.

## Constraints

Performance budgets, memory limits, security requirements.

## References

- [ADR](../adrs/NNNN-title.md) — decision that led to this spec
- [Idea Doc](../../ideas/NN-topic.md) — original exploration
```

## Index

(none yet)
