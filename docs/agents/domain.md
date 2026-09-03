# Domain docs

Before investigating or changing the project, read:

- Root-level `CONTEXT.md`
- Architecture Decision Records under `docs/architecture-decisions/`

Missing files require no action.

## Layout

This is a **single-context** repository. Domain vocabulary lives in root-level `CONTEXT.md`; architectural decisions live under `docs/architecture-decisions/`.

## Format

Follow the installed `/domain-modeling` skill's `CONTEXT-FORMAT.md` and `ADR-FORMAT.md` as the format authority. Preserve this repository's established ADR directory and existing filename convention.

## Vocabulary and consistency

Use terminology exactly as `CONTEXT.md` defines it. Match that language when naming issues, proposals, tests, and implementation concepts.

## ADR contradictions

Explicitly surface conflicts with existing decisions instead of proceeding silently.
