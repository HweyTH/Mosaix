# Workflow

- This repository is a Cargo workspace. Build it with `cargo build`.
- Run the workspace tests with `cargo test --workspace`.
- GitHub operations target `HweyTH/Mosaix`. Before mutating GitHub state, verify that `gh api user --jq .login` returns `HweyTH`.

## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues for `HweyTH/Mosaix`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the five canonical Hwey triage labels without aliases. See `docs/agents/triage-labels.md`.

### Domain docs

This is a single-context repository using root-level `CONTEXT.md` and ADRs under `docs/architecture-decisions/`. See `docs/agents/domain.md`.
