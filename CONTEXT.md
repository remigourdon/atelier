# Context

Atelier organises git worktrees into zellij sessions, on top of worktrunk (`wt`).

- **Workspace** — a named zellij session that owns items. One is the default workspace (`default_workspace`, `default` unless configured): it always exists and cannot be removed.
- **Repo** — a registered git repository (its main worktree path), with an optional alias and a default workspace.
- **Item** — something atelier can open in a tab: a worktree or a carnet. Each item belongs to exactly one workspace.
- **Worktree** — a git worktree of a registered repo, created and removed through worktrunk.
- **Main worktree** — a repo's primary checkout. Always listed.
- **Carnet** — an investigation folder `<root>/YYYY-MM-DD-<name>` that is its own git repo, with only its main worktree; together the carnets are a searchable history of investigations. The folder itself is the record: the front matter of its README holds its `group`, the `issues` it links, whether it is `closed`, and a one-line `summary`. Without front matter it has no group and links nothing, whatever its folder is named. An item of its own kind, never a registered repo: worktrees made of it by hand are not tracked. Optional.
- **Closed carnet** — a carnet whose investigation is over: out of the Work panel, still listed and searchable with every other carnet.
- **Group** — a free-form label that helps a person see what they are working on, trimmed and uppercased wherever it comes in (`LOGIN REWRITE`). Optional, independent of workspaces, and able to span them. A group never carries issue keys.
- **Tab** — the zellij tab opened for an item, recorded as session, tab id and anchor pane id.
- **Anchor pane** — the pane named `editor` in the worktree layout; atelier finds a tab's pane by this name.
- **Review** — an open GitHub pull request or GitLab merge request, either to review or authored by me. It links the issues its closing references and the issue keys in its title, branch and body name, found when it is listed, never stored; its worktree is the recorded worktree on its branch in its project's registered repo, and its group is that worktree's.
- **CI** — worktrunk's status of a worktree's branch, kept as three facts: its checks (passed, running, failed, or unavailable when they could not be fetched), whether its review conflicts with its base, and the review's decision (changes requested, waiting for approval, approved, or draft); stale when the local head is not the one checked. A review with no checks still has CI. A default branch has its own workflow's checks and no review.
- **Severity** — how badly a worktree needs attention: its most severe fact. Broken: its checks failed, its review conflicts, its checkout has conflicts or merging into the default branch would conflict. Needs you: it is behind or diverged from the remote, its checkout is unusual, it shares no history with the default branch, changes were requested or its checks are unavailable. Waiting: its review waits for approval. Fine otherwise; stale or draft checks count for nothing. The TUI tints a worktree's name with it; a finished worktree is dimmed instead.
- **Issue** — a tracker record (GitHub or Jira) normalised to `state`, `labels` and `blocked`.
- **Issue key** — an issue's identifier: `ABC-5` on Jira; on GitHub always `owner/repo#12`, shown as `repo#12` when no other configured repo shares the name. Every item links issues through its own ordered list of issue keys.
- **State** — an issue's normalised progress: `todo`, `in_progress` or `done`.
- **Section** — an ordered rule that places issues in an Issues sub-tab; first match wins.
- **Linked work** — every item, worktree or carnet, that links an issue's key, in any group.
- **Finished worktree** — a worktree other than a main one whose work is merged, as of the last fetch: worktrunk reports its branch integrated into the default branch, or its branch's upstream is gone. Integrated wins when both hold. The TUI says merged and remote branch deleted.
- **Finish plan** — the toggleable lines `f` shows for a group, a workspace or an issue (the whole groups of its linked work, and its linked items in no group alone): remove each finished worktree, close each open carnet of the group, pull each main worktree, and why the rest stays.
- **Command log** — the TUI's record of every external command atelier ran and its result.
