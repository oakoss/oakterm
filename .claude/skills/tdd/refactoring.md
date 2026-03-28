# Refactor Candidates

After TDD cycle, look for:

- **Duplication** — extract function/module
- **Long methods** — break into private helpers (keep tests on public interface)
- **Shallow modules** — combine or deepen
- **Feature envy** — move logic to where data lives
- **Existing code** the new code reveals as problematic

Never refactor while RED. Get to GREEN first.
