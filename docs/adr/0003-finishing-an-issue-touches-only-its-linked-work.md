# Finishing an issue touches only its linked work

[ADR 0002](0002-groups-are-labels-links-live-on-items.md) had finishing an issue cover the whole groups of its linked work. That rule outlived the model it came from: when the group was the issue key, a group was one issue's work, but now that every item links its own issue keys and a group can span several issues, finishing one issue removed finished worktrees and closed carnets that belonged to another. So an issue's finish plan covers exactly its linked work: the items that link its key, in any group or none. A carnet that also links another issue keeps its close line unchecked, and a closed carnet has nothing left to finish. Finishing a group or sweeping a workspace still covers the whole group.

## Considered Options

- **Whole groups of the issue's linked work (ADR 0002).** One `f` cleans up everything around an issue, but it reaches work no link ties to the issue, and whether a closed carnet widened the plan depended on whether its tab was open.
