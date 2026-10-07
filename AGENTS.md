# AGENTS.md

## Git conventions

- **Branches** — named after the change: `phase-0-scaffold`, `fix-tab-naming`. Agent-name prefixes (`claude/…`) are rejected.
- **Commits** — the whole message of a commit you author is one imperative subject line: `Add sqlite migrations`. Nothing follows it: no body, no `feat:`/`fix:`/`chore:` prefix, no `Co-Authored-By` or `Claude-Session` trailer. A squash merge on `main` keeps GitHub's default body listing the PR's commits.
- **Pull requests** — the description states what changed and why, and ends there: no attribution or "generated with" footer.
