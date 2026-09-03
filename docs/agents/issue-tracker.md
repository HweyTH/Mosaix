# Issue tracker: GitHub

Issues and specs for this repository live as GitHub issues in `HweyTH/Mosaix`. Use the `gh` CLI for all operations.

## Account

The required GitHub account is `HweyTH`, not the work account `ThaiGiaHuyDPU`.

Before any operation that changes GitHub state, verify the active identity:

`gh api user --jq .login`

The result must be `HweyTH`. If it is not, switch accounts:

`gh auth switch --hostname github.com --user HweyTH`

## Conventions

- Create: `gh issue create --title "..." --body "..."`
- Read: `gh issue view <number> --comments`
- List: `gh issue list --state open --json number,title,body,labels,comments`
- Comment: `gh issue comment <number> --body "..."`
- Add/remove labels: `gh issue edit <number> --add-label "..."` or `--remove-label "..."`
- Close: `gh issue close <number> --comment "..."`

Infer the repository from `git remote -v`.

## Pull requests as a triage surface

**PRs as a request surface: no.**

If changed to `yes`, external PRs run through the same labels and states as issues using the corresponding `gh pr` commands.

## When a skill says “publish to the issue tracker”

Create a GitHub issue.

## When a skill says “fetch the relevant ticket”

Run `gh issue view <number> --comments`.

## Wayfinding operations

- A map is an issue labelled `wayfinder:map`.
- Children are GitHub sub-issues, falling back to a task list and `Part of #<map>` when sub-issues are unavailable.
- Child labels use `wayfinder:<type>`, where the type is `research`, `prototype`, `grilling`, or `task`.
- Represent blockers with native GitHub issue dependencies. Fall back to a `Blocked by: #<n>` line when dependencies are unavailable.
- Claim a ticket with `gh issue edit <number> --add-assignee @me`.
- Resolve it by adding the answer as a comment, closing it, and adding its context pointer to the map.
