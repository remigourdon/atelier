//! Panel 2's Work list: the selected workspace's worktrees and open carnets, in groups.

use std::path::PathBuf;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, kind, pair, paths};
use crate::tui::app::{
    Action, Effect, Job, Kind, List, MenuEntry, Modal, Model, Removal, Submit, Work, WorkKind,
};
use crate::tui::update::{confirm, note, run, update, workspace_menu};
use crate::tui::view::{Palette, icon};

pub struct WorkList;

/// The end of the key of a workspace's `Carnets` group, which no group name can produce.
const CARNETS_KEY: &str = "\0\0carnets";

/// A row of the Work panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A group header; `members` index `Snapshot::work`. The `Carnets` group, of ungrouped
    /// carnets, has an empty `name`.
    Group {
        key: String,
        name: String,
        members: Vec<usize>,
        folded: bool,
    },
    Item(usize),
}

impl Model {
    /// Whether a group row is folded; the `Carnets` group starts folded.
    pub fn is_folded(&self, key: &str) -> bool {
        self.folded.contains(key) != key.ends_with(CARNETS_KEY)
    }

    pub fn set_folded(&mut self, key: &str, folded: bool) {
        if folded == key.ends_with(CARNETS_KEY) {
            self.folded.remove(key);
        } else {
            self.folded.insert(key.to_owned());
        }
    }

    /// Panel 2's rows: named groups, foldable, then ungrouped worktrees, then the ungrouped
    /// carnets in a `Carnets` group. Worktrees come before carnets, which are newest first.
    pub fn work_rows(&self) -> Vec<Row> {
        let Some(workspace) = self.workspace() else {
            return Vec::new();
        };
        let work = &self.snapshot.work;
        let filtering = !self.filter(List::Work).is_empty();
        let mut members: Vec<usize> = (0..work.len())
            .filter(|&index| {
                let work = &work[index];
                work.workspace == workspace
                    && self.matches(
                        List::Work,
                        &[&work.title(), &work.group, &work.path.to_string_lossy()],
                    )
            })
            .collect();
        // Named groups, then ungrouped worktrees, then ungrouped carnets.
        let section = |work: &Work| {
            (
                work.group.is_empty(),
                work.in_carnets_group(),
                work.group.clone(),
            )
        };
        members.sort_by(|&a, &b| {
            let (a, b) = (&work[a], &work[b]);
            section(a)
                .cmp(&section(b))
                .then_with(|| match (&a.kind, &b.kind) {
                    (
                        WorkKind::Worktree {
                            repo_name: x,
                            tree: tx,
                            ..
                        },
                        WorkKind::Worktree {
                            repo_name: y,
                            tree: ty,
                            ..
                        },
                    ) => (x, !tx.main, a.branch()).cmp(&(y, !ty.main, b.branch())),
                    (WorkKind::Worktree { .. }, WorkKind::Carnet { .. }) => {
                        std::cmp::Ordering::Less
                    }
                    (WorkKind::Carnet { .. }, WorkKind::Worktree { .. }) => {
                        std::cmp::Ordering::Greater
                    }
                    (WorkKind::Carnet { .. }, WorkKind::Carnet { .. }) => {
                        b.path.file_name().cmp(&a.path.file_name())
                    }
                })
        });
        let mut lines = Vec::new();
        let mut index = 0;
        while index < members.len() {
            let first = &work[members[index]];
            let carnets = first.in_carnets_group();
            let end = members[index..]
                .iter()
                .position(|&other| section(&work[other]) != section(first))
                .map_or(members.len(), |offset| index + offset);
            let slice = &members[index..end];
            if first.group.is_empty() && !carnets {
                lines.extend(slice.iter().map(|&member| Row::Item(member)));
            } else {
                let key = if carnets {
                    format!("{workspace}{CARNETS_KEY}")
                } else {
                    format!("{workspace}\0{}", first.group)
                };
                let folded = !filtering && self.is_folded(&key);
                lines.push(Row::Group {
                    key,
                    name: first.group.clone(),
                    members: slice.to_vec(),
                    folded,
                });
                if !folded {
                    lines.extend(slice.iter().map(|&member| Row::Item(member)));
                }
            }
            index = end;
        }
        lines
    }

    pub fn work_row(&self) -> Option<Row> {
        self.work_rows().into_iter().nth(self.index(List::Work))
    }

    /// The selected worktree, or every worktree of the selected group.
    pub fn targets(&self) -> Vec<&Work> {
        match self.work_row() {
            Some(Row::Item(index)) => vec![&self.snapshot.work[index]],
            Some(Row::Group { members, .. }) => members
                .iter()
                .map(|&index| &self.snapshot.work[index])
                .collect(),
            None => Vec::new(),
        }
    }
}

/// The item selected in the Work list, when it is active and on an item row.
pub fn selected(model: &Model) -> Option<&Work> {
    match (model.active(), model.work_row()?) {
        (List::Work, Row::Item(index)) => Some(&model.snapshot.work[index]),
        _ => None,
    }
}

/// How a group row is named; the `Carnets` group has no group name.
pub fn group_name(name: &str) -> &str {
    if name.is_empty() { "Carnets" } else { name }
}

impl ListKind for WorkList {
    fn kind(&self) -> Kind {
        Kind::Work
    }

    fn title<'a>(&self, _model: &'a Model, _list: List) -> &'a str {
        "Work"
    }

    fn len(&self, model: &Model, _list: List) -> usize {
        model.work_rows().len()
    }

    /// A group's key, which holds a NUL no path does, or an item's path.
    fn ids(&self, model: &Model, _list: List) -> Vec<String> {
        (model.work_rows().into_iter())
            .map(|row| match row {
                Row::Group { key, .. } => key,
                Row::Item(index) => model.snapshot.work[index].path().display().to_string(),
            })
            .collect()
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, _list: List) -> Vec<Line<'a>> {
        let dim = Style::new().fg(palette.dim);
        model
            .work_rows()
            .into_iter()
            .map(|line| match line {
                Row::Group {
                    name,
                    members,
                    folded,
                    ..
                } => {
                    let open = members
                        .iter()
                        .filter(|&&index| model.snapshot.work[index].tab)
                        .count();
                    Line::from(vec![
                        Span::styled(
                            format!(
                                "{} {}",
                                if folded {
                                    palette.glyphs.folded
                                } else {
                                    palette.glyphs.unfolded
                                },
                                group_name(&name)
                            ),
                            Style::new().fg(palette.info).bold(),
                        ),
                        Span::styled(format!(" {} · {open} open", members.len()), dim),
                    ])
                }
                Row::Item(index) => {
                    let work = &model.snapshot.work[index];
                    let glyphs = &palette.glyphs;
                    let indent = if work.group.is_empty() && !work.in_carnets_group() {
                        ""
                    } else {
                        "  "
                    };
                    let marker = if model.pulling.contains(work.path()) {
                        let frame = glyphs.spinner[model.frame % glyphs.spinner.len()];
                        Span::styled(format!("{frame} "), Style::new().fg(palette.info))
                    } else if work.tab {
                        Span::styled(format!("{} ", glyphs.open), Style::new().fg(palette.ok))
                    } else {
                        Span::styled(format!("{} ", glyphs.closed), dim)
                    };
                    let mut spans = vec![Span::raw(indent), marker];
                    let glyph = if work.is_carnet() {
                        glyphs.carnet
                    } else {
                        glyphs.worktree
                    };
                    spans.extend(icon(glyph, dim));
                    spans.push(Span::raw(work.title()));
                    let tree = work.tree();
                    if let Some(tree) = tree.filter(|tree| !tree.symbols.is_empty()) {
                        let status = if tree.dirty {
                            Style::new().fg(palette.warn)
                        } else {
                            dim
                        };
                        spans.push(Span::styled(format!(" {}", tree.symbols), status));
                    }
                    if let Some((_, behind)) =
                        (tree.and_then(|tree| tree.upstream)).filter(|&(_, behind)| behind > 0)
                    {
                        spans.push(Span::styled(
                            format!(" ↓{behind}"),
                            Style::new().fg(palette.warn),
                        ));
                    }
                    Line::from(spans)
                }
            })
            .collect()
    }

    fn detail(&self, model: &Model, _list: List) -> Vec<(String, String)> {
        match model.work_row() {
            Some(Row::Group { name, members, .. }) => {
                let mut pairs = vec![pair("Group", group_name(&name).to_owned())];
                pairs.extend(members.iter().map(|&index| {
                    let work = &model.snapshot.work[index];
                    pair(kind(work), work.title())
                }));
                pairs
            }
            Some(Row::Item(index)) => {
                let work = &model.snapshot.work[index];
                let (repo_name, tree) = match &work.kind {
                    WorkKind::Worktree {
                        repo_name, tree, ..
                    } => (repo_name, tree),
                    WorkKind::Carnet {
                        tickets, summary, ..
                    } => {
                        return vec![
                            pair("Carnet", work.title()),
                            pair("Path", work.path.display().to_string()),
                            pair("Workspace", work.workspace.clone()),
                            pair("Tickets", tickets.join(", ")),
                            pair("Summary", summary.clone()),
                            pair("Tab", if work.tab { "open" } else { "closed" }.into()),
                        ];
                    }
                };
                vec![
                    pair("Repo", repo_name.clone()),
                    pair("Branch", work.branch()),
                    pair("Path", tree.path.display().to_string()),
                    pair("Workspace", work.workspace.clone()),
                    pair("Group", work.group.clone()),
                    pair("Tab", if work.tab { "open" } else { "closed" }.into()),
                    pair(
                        "Status",
                        if tree.dirty {
                            format!("dirty +{} -{}", tree.diff.0, tree.diff.1)
                        } else {
                            "clean".into()
                        },
                    ),
                    pair(
                        "Upstream",
                        tree.upstream
                            .map(|(ahead, behind)| format!("↑{ahead} ↓{behind}"))
                            .unwrap_or_else(|| "none".into()),
                    ),
                    pair(
                        "Commit",
                        format!(
                            "{} {} ({})",
                            tree.short_sha, tree.subject, tree.committed_at
                        ),
                    ),
                ]
            }
            None => Vec::new(),
        }
    }

    fn activate(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let paths = paths(&model.targets());
        run_on(model, paths, Job::Open)
    }

    /// Folds or unfolds a group. Not the row's `folded`: a filter shows every group unfolded.
    fn enter(&self, model: &mut Model, _list: List) -> bool {
        let Some(Row::Group { key, .. }) = model.work_row() else {
            return false;
        };
        model.set_folded(&key, !model.is_folded(&key));
        true
    }

    fn create(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(workspace) = model.workspace().map(str::to_owned) else {
            return Vec::new();
        };
        let group = match model.work_row() {
            Some(Row::Group { name, .. }) => name,
            Some(Row::Item(index)) => model.snapshot.work[index].group.clone(),
            None => String::new(),
        };
        let ask = |repo: PathBuf, name: String| Action::Ask {
            title: format!("New worktree of {name}: branch"),
            initial: String::new(),
            then: Submit::Branch {
                repo,
                workspace: workspace.clone(),
                group: group.clone(),
            },
        };
        // The selected worktree's repo, else every repo.
        let selected = (model.targets().first()).and_then(|work| match &work.kind {
            WorkKind::Worktree {
                repo, repo_name, ..
            } => Some((repo.clone(), repo_name.clone())),
            WorkKind::Carnet { .. } => None,
        });
        let mut entries: Vec<MenuEntry> = match selected {
            Some((repo, name)) if !model.carnets => return update(model, ask(repo, name)),
            Some((repo, name)) => vec![MenuEntry {
                key: "1".into(),
                label: format!("worktree of {name}"),
                action: ask(repo, name),
            }],
            None => (model.snapshot.repos.iter().enumerate())
                .map(|(index, repo)| MenuEntry {
                    key: (index + 1).to_string(),
                    label: if model.carnets {
                        format!("worktree of {}", repo.name())
                    } else {
                        repo.name()
                    },
                    action: ask(repo.path.clone(), repo.name()),
                })
                .collect(),
        };
        if model.carnets {
            entries.push(MenuEntry {
                key: "c".into(),
                label: "carnet".into(),
                action: Action::Ask {
                    title: "New carnet: name".into(),
                    initial: String::new(),
                    then: Submit::Carnet { workspace, group },
                },
            });
        }
        if entries.is_empty() {
            return note(
                model,
                "no repos yet: register one with `atelier add <path>`",
            );
        }
        let title = if model.carnets {
            "New"
        } else {
            "New worktree in"
        };
        model.modal = Some(Modal::Menu {
            title: title.into(),
            entries,
            selected: 0,
        });
        Vec::new()
    }

    fn edit(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let targets = model.targets();
        let Some(first) = targets.first() else {
            return Vec::new();
        };
        let action = Action::Ask {
            title: format!("Group of {} item(s)", targets.len()),
            initial: first.group.clone(),
            then: Submit::Group(paths(&targets)),
        };
        update(model, action)
    }

    fn move_to(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let targets = model.targets();
        let Some(first) = targets.first() else {
            return Vec::new();
        };
        let current = first.workspace.clone();
        let paths = paths(&targets);
        workspace_menu(
            model,
            format!("Move {} item(s) to", paths.len()),
            &current,
            |workspace| Job::Move {
                paths: paths.clone(),
                workspace,
            },
        )
    }

    fn remove(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let targets = model.targets();
        let removable: Vec<(&Work, Removal)> = (targets.into_iter())
            .filter_map(|work| Some((work, Removal::of(work)?)))
            .collect();
        if removable.is_empty() {
            return note(model, "main worktrees and carnets are never removed");
        }
        let mut lines = vec!["Remove these?".to_owned()];
        for (work, removal) in &removable {
            let note = if removal.force {
                "  (uncommitted changes will be lost)"
            } else {
                ""
            };
            lines.push(format!("  {}{note}", work.title()));
        }
        let removals = removable.into_iter().map(|(_, removal)| removal).collect();
        confirm(model, "Remove".into(), lines, Job::Remove(removals))
    }

    /// Closes the open tabs of the selection.
    fn close(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let open: Vec<_> = (model.targets().into_iter())
            .filter(|work| work.tab)
            .collect();
        run_on(model, paths(&open), Job::Close)
    }

    fn close_carnet(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let carnets: Vec<_> = (model.targets().into_iter())
            .filter(|work| work.is_carnet())
            .collect();
        run_on(model, paths(&carnets), Job::CloseCarnet)
    }

    fn pull(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let trees: Vec<_> = (model.targets().into_iter())
            .filter(|work| work.tree().is_some())
            .collect();
        run_on(model, paths(&trees), Job::Pull)
    }

    /// The item's path, or the group's name.
    fn copy_path(&self, model: &Model, _list: List) -> Option<String> {
        match model.work_row()? {
            Row::Item(index) => Some(model.snapshot.work[index].path().display().to_string()),
            Row::Group { name, .. } => Some(name).filter(|name| !name.is_empty()),
        }
    }

    fn branch(&self, model: &Model, _list: List) -> Option<String> {
        match model.work_row()? {
            Row::Item(index) => (model.snapshot.work[index].tree())?.branch.clone(),
            Row::Group { .. } => None,
        }
    }

    /// The forge page of the worktree's branch.
    fn url(&self, model: &Model, _list: List) -> Option<String> {
        match model.work_row()? {
            Row::Item(index) => {
                let work = &model.snapshot.work[index];
                let forge = model.snapshot.forges.get(work.repo()?)?;
                Some(match &work.tree()?.branch {
                    Some(branch) => forge.branch_url(branch),
                    None => forge.url.clone(),
                })
            }
            Row::Group { .. } => None,
        }
    }
}

/// Runs a job on some paths, unless there are none.
fn run_on(
    model: &mut Model,
    paths: Vec<PathBuf>,
    job: impl FnOnce(Vec<PathBuf>) -> Job,
) -> Vec<Effect> {
    if paths.is_empty() {
        Vec::new()
    } else {
        vec![run(model, job(paths))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::update::tests::{model, with_carnets, work};

    fn titles(model: &Model) -> Vec<String> {
        (model.work_rows().into_iter())
            .map(|row| match row {
                Row::Group { name, .. } => format!("[{}]", group_name(&name)),
                Row::Item(index) => model.snapshot.work[index].title(),
            })
            .collect()
    }

    /// The default workspace's carnets, the ABC-1 worktrees, and two more: one ungrouped and
    /// one in group XYZ-2.
    fn grouped() -> Model {
        let mut model = with_carnets(model());
        model.snapshot.work.extend([
            work("web", "XYZ-2-menu", "XYZ-2", "default"),
            work("api", "fix", "", "default"),
        ]);
        model
    }

    #[test]
    fn named_groups_come_first_then_ungrouped_worktrees_then_carnets() {
        assert_eq!(
            titles(&grouped()),
            [
                "[ABC-1]",
                "api:ABC-1-login",
                "web:ABC-1-form",
                "2026-10-01-ABC-1-logs",
                "[XYZ-2]",
                "web:XYZ-2-menu",
                "api:main",
                "api:fix",
                "[Carnets]",
            ]
        );
    }

    #[test]
    fn a_repos_main_worktree_comes_first() {
        let mut model = grouped();
        model.snapshot.work.push(work("api", "aaa", "", "default"));
        let titles = titles(&model);
        let at = |title: &str| titles.iter().position(|other| other == title).unwrap();
        assert!(at("api:main") < at("api:aaa"));
        assert!(at("api:aaa") < at("api:fix"));
    }

    #[test]
    fn the_carnets_group_is_folded_by_default() {
        let model = grouped();
        let rows = model.work_rows();
        let Some(Row::Group {
            name,
            folded,
            members,
            ..
        }) = rows.last()
        else {
            panic!("no Carnets group");
        };
        assert_eq!((name.as_str(), *folded, members.len()), ("", true, 2));
    }

    #[test]
    fn carnets_are_newest_first() {
        let mut model = grouped();
        let Some(Row::Group { key, .. }) = model.work_rows().pop() else {
            panic!("no Carnets group");
        };
        model.set_folded(&key, false);
        assert_eq!(
            titles(&model)[8..],
            ["[Carnets]", "2026-10-02-ideas", "2026-09-20-old"]
        );
    }
}
