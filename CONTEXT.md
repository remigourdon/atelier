# Context

Atelier organises git worktrees into zellij sessions, on top of worktrunk (`wt`).

- **Workspace** — a named zellij session that owns items. One is the default workspace (`default_workspace`, `default` unless configured): it always exists and cannot be removed.
- **Repo** — a registered git repository (its main worktree path), with an optional alias and a default workspace.
- **Item** — something atelier can open in a tab: a worktree or a carnet. Each item belongs to exactly one workspace.
- **Worktree** — a git worktree of a registered repo, created and removed through worktrunk.
- **Main worktree** — a repo's primary checkout. Always listed.
- **Carnet** — an investigation folder `<root>/YYYY-MM-DD-<name>` that is its own git repo, with only its main worktree; together the carnets are a searchable history of investigations. The folder itself is the record: the front matter of its README names its tickets, whether it is closed, and a one-line summary. An item of its own kind, never a registered repo: worktrees made of it by hand are not tracked. Its group is its first ticket. Optional.
- **Closed carnet** — a carnet whose investigation is over: out of the Work panel, still listed and searchable with every other carnet.
- **Group** — a label shared by items about the same ticket, derived from a ticket key (`ABC-123`) in the branch or name, or a carnet's first ticket, or set by hand.
- **Tab** — the zellij tab opened for an item, recorded as session, tab id and anchor pane id.
- **Anchor pane** — the pane named `editor` in the worktree layout; atelier finds a tab's pane by this name.
- **Review** — an open GitHub pull request or GitLab merge request, either to review or authored by me.
- **CI** — worktrunk's status of a worktree's branch: its checks (passed, running, failed), merge conflicts, or its review's decision (changes requested, approval pending); stale when the local head is not the one checked. A default branch has its own workflow's checks and no review.
- **Issue** — a tracker item (GitHub or Jira) normalised to `state`, `labels` and `blocked`.
- **State** — an issue's normalised progress: `todo`, `in_progress` or `done`.
- **Section** — an ordered rule that places issues in an Issues sub-tab; first match wins.
- **Linked work** — the worktrees whose group is an issue's key (`ABC-123`, or `repo#12` on GitHub, with the owner when two configured repos share a name), and the carnets that list that key among their tickets.
- **Finished worktree** — a worktree other than a main one whose work is merged, as of the last fetch: worktrunk reports its branch integrated into the default branch, or its branch's upstream is gone. Integrated wins when both hold.
- **Finish plan** — the toggleable lines `f` shows for a group, a workspace or an issue's linked work: remove each finished worktree, close the group's carnet, pull each main worktree, and why the rest stays.
- **Command log** — the TUI's record of every external command atelier ran and its result.
