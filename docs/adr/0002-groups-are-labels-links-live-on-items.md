# Groups are labels; issue links live on items

One string, `items.group_key`, did three jobs: it grouped items, it was the foreign key to an issue, and it was the label shown. So a group typed by hand (`slow pages`) could never link an issue, linking was asymmetric (a carnet linked through any of its tickets, a worktree only through its one group, and finish closed a carnet only when its first ticket was the group), carnets disagreed with themselves (`carnet new` dropped a group that was not a key, `e` wrote it into `tickets`), and a GitHub key changed from `repo#12` to `owner/repo#12` once a second configured repo shared the name.

Groups help a person see what they are working on; issue links must be precise. These are two jobs, so they get two fields. A **group** is a free-form label, trimmed and uppercased wherever it comes in, optional, and independent of workspaces. Every item, worktree or carnet, **links** issues through its own ordered list of **issue keys**. A worktree keeps both in sqlite; a carnet keeps them in its README's front matter (`group`, `issues`), per [ADR 0001](0001-carnet-folder-is-the-record.md). An issue's linked work is every item linking its key, in any group. GitHub keys are always `owner/repo#12`, shortened to `repo#12` for display only.

Groups never carry keys. A new worktree for an issue or a review joins the one group among the items linking the same keys; items in no group are unsorted rather than a choice against a group, so they do not count. Finishing an issue covers the whole groups of its linked work, and its linked items in no group alone. Nothing converts the old state.

## Considered Options

- **The group is the issue key (the old model).** One field, no linking to edit, but a hand-typed group links nothing, an item can follow only one issue, and the label shown is whatever the key is.
- **Keys on the group.** A group would carry the issues it is about, and its members would link them through it. Fewer lists to edit, but an ungrouped item could link nothing, a group spanning several issues would link all of them from every member, and moving an item between groups would silently change what it links.
- **Keys on both.** The group's keys plus the item's own. Most expressive, but two places to look for why an item links an issue, and two places to edit when it should not.
