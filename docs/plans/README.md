# Plans

Plans are temporary execution artifacts.

## Folders

- `active/`: only currently actionable plans
- `archive/<year>/`: historical plans

## Operator Rules

- Never keep finished plans in `active/`.
- If a plan is replaced by a newer version, archive the old one immediately.
- If a plan drives permanent policy, extract that policy into `docs/decisions/`.
