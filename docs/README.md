# Documentation Index

This `docs/` tree is optimized for maintainer use: quick retrieval, low noise, and explicit lifecycle.

## Layout

- `docs/runbooks/`: repeatable operational procedures (deploy, rollback, emergency checks)
- `docs/decisions/`: durable architecture/product decisions (ADR-style notes)
- `docs/plans/active/`: plans currently in progress
- `docs/plans/archive/`: completed or superseded plans (grouped by year)
- `docs/perf/`: benchmark artifacts and performance snapshots

## Lifecycle Rules

1. Draft plans in `docs/plans/active/`.
2. When work is done or abandoned, move the plan to `docs/plans/archive/<year>/`.
3. Record long-term rationale in `docs/decisions/` instead of leaving it only in plan files.
4. Keep runbooks task-oriented and executable; avoid narrative-only docs.

## Naming

- Plans: `YYYY-MM-DD-<topic>.md`
- Decisions: `YYYY-MM-DD-<topic>-decision.md`
- Runbooks: `<topic>-runbook.md`
