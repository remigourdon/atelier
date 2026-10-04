# Context

Atelier organises git worktrees into zellij sessions, on top of worktrunk (`wt`).

- **Workspace** — a named zellij session that owns items. One is the default workspace (`default_workspace`, `default` unless configured): it always exists and cannot be removed.
- **Repo** — a registered git repository (its main worktree path), with an optional alias and a default workspace.
- **Item** — something atelier can open in a tab: a worktree or a carnet. Each item belongs to exactly one workspace.
- **Worktree** — a git worktree of a registered repo, created and removed through worktrunk.
- **Main worktree** — a repo's primary checkout. Always listed.
- **Carnet** — an investigation folder `<root>/YYYY-MM-DD-<name>` that is its own git repo. Optional.
- **Group** — a label shared by items about the same ticket, derived from a ticket key (`ABC-123`) in the branch or name, or set by hand.
- **Tab** — the zellij tab opened for an item, recorded as session, tab id and anchor pane id.
- **Anchor pane** — the pane named `editor` in the worktree layout; atelier finds a tab's pane by this name.
- **Review** — an open GitHub pull request or GitLab merge request, either to review or authored by me.
- **Issue** — a tracker item (GitHub or Jira) normalised to `state`, `labels` and `blocked`.
- **State** — an issue's normalised progress: `todo`, `in_progress` or `done`.
- **Section** — an ordered rule that places issues in an Issues sub-tab; first match wins.
- **Command log** — the TUI's record of every external command atelier ran and its result.
