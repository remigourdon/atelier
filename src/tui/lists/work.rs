//! Panel 2's Work list: the selected workspace's worktrees and open carnets, in groups.

use std::path::PathBuf;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{ListKind, carnets, kind, pair, paths, plan, subtle, tab_detail, tab_mark};
use crate::finish::{self, Scope, Signal};
use crate::tui::app::{
    Action, Cmd, Effect, Job, Kind, List, MenuEntry, Modal, Model, Removal, Submit, Work, WorkKind,
};
use crate::tui::update::{confirm, note, run, update, workspace_menu};
use crate::tui::view::{Palette, icon};
use crate::worktrunk::{Ci, CiReview, CiState, Decision, Forge, Worktree};

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
                    let marker = if model.schedule.is_pulling(work.path()) {
                        let frame = glyphs.spinner[model.frame % glyphs.spinner.len()];
                        Span::styled(format!("{frame} "), Style::new().fg(palette.info))
                    } else {
                        tab_mark(work.tab, palette)
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
                    if let Some(ci) = tree.and_then(|tree| tree.ci.as_ref()) {
                        spans.push(Span::raw(" "));
                        spans.push(ci_mark(ci, palette));
                    }
                    if let Some(tree) = tree.filter(|tree| !tree.symbols.is_empty()) {
                        spans.push(Span::raw(" "));
                        spans.push(symbols(tree, palette));
                    }
                    if let Some(behind) = tree.and_then(|tree| behind(tree, palette)) {
                        spans.push(Span::raw(" "));
                        spans.push(behind);
                    }
                    // Finished: dimmed, with why.
                    if let Some(signal) = finish::signal(work) {
                        spans.push(Span::raw(" "));
                        spans.push(finished_mark(signal, palette));
                        spans = spans.into_iter().map(|span| span.style(dim)).collect();
                    }
                    Line::from(spans)
                }
            })
            .collect()
    }

    fn detail(
        &self,
        model: &Model,
        palette: &Palette,
        _list: List,
    ) -> Vec<(String, Line<'static>)> {
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
                let (repo, repo_name, tree) = match &work.kind {
                    WorkKind::Worktree {
                        repo,
                        repo_name,
                        tree,
                    } => (repo, repo_name, tree),
                    WorkKind::Carnet { .. } => return carnets::detail(work, palette),
                };
                let status_text = if tree.dirty {
                    format!("dirty +{} -{}", tree.diff.0, tree.diff.1)
                } else {
                    "clean".into()
                };
                let mut status = Vec::new();
                if !tree.symbols.is_empty() {
                    status.extend([symbols(tree, palette), Span::raw(" ")]);
                }
                status.push(Span::styled(status_text, status_style(tree, palette)));
                let upstream = match tree.upstream {
                    // Counts of zero recede.
                    Some((ahead, _)) => {
                        let ahead = if ahead > 0 {
                            Span::raw(format!("↑{ahead}"))
                        } else {
                            subtle("↑0", palette)
                        };
                        let behind =
                            self::behind(tree, palette).unwrap_or_else(|| subtle("↓0", palette));
                        Line::from(vec![ahead, Span::raw(" "), behind])
                    }
                    None => subtle("none", palette).into(),
                };
                let mut pairs = vec![
                    pair("Repo", repo_name.clone()),
                    pair("Branch", work.branch()),
                    pair("Path", tree.path.display().to_string()),
                    pair("Workspace", work.workspace.clone()),
                    pair("Group", work.group.clone()),
                    pair("Tab", tab_detail(work.tab, palette)),
                    pair("Status", status),
                    pair("Upstream", upstream),
                    pair(
                        "Commit",
                        vec![
                            subtle(format!("{} ", tree.short_sha), palette),
                            Span::raw(tree.subject.clone()),
                            subtle(format!(" ({})", tree.committed_at), palette),
                        ],
                    ),
                ];
                if let Some(ci) = &tree.ci {
                    pairs.push(pair("CI", ci_detail(ci, palette)));
                    let forge = model.snapshot.forges.get(repo);
                    if let Some(review) = &ci.review {
                        pairs.push(pair("Review", review_detail(ci, review, forge, palette)));
                    }
                }
                if let Some(signal) = finish::signal(work) {
                    let why = format!(" {} (f to finish)", signal.label());
                    let line = vec![
                        finished_mark(signal, palette),
                        Span::styled(why, Style::new().fg(palette.dim)),
                    ];
                    pairs.push(pair("Finished", line));
                }
                pairs
            }
            None => Vec::new(),
        }
    }

    /// The item on the selected row.
    fn item<'a>(&self, model: &'a Model, _list: List) -> Option<&'a Work> {
        match model.work_row()? {
            Row::Item(index) => Some(&model.snapshot.work[index]),
            Row::Group { .. } => None,
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

    /// The selection's whole groups, and its items in no group on their own.
    fn finish(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let targets = model.targets();
        if targets.is_empty() {
            return Vec::new();
        }
        let mut groups: Vec<String> = Vec::new();
        let mut items = Vec::new();
        for work in targets {
            if work.group.is_empty() {
                items.push(work.path.clone());
            } else if !groups.contains(&work.group) {
                groups.push(work.group.clone());
            }
        }
        plan(model, Scope::Work { groups, items })
    }

    /// `x` closes the selection's open tabs, `c` its carnets, `p` pulls its worktrees.
    fn command(&self, model: &mut Model, _list: List, cmd: Cmd) -> Vec<Effect> {
        let targets = model.targets().into_iter();
        let (targets, job): (Vec<_>, fn(_) -> _) = match cmd {
            Cmd::Close => (targets.filter(|work| work.tab).collect(), Job::Close),
            Cmd::ToggleCarnet => (
                targets.filter(|work| work.is_carnet()).collect(),
                Job::CloseCarnet,
            ),
            Cmd::Pull => (
                targets.filter(|work| work.tree().is_some()).collect(),
                Job::Pull,
            ),
            _ => return Vec::new(),
        };
        let paths = paths(&targets);
        run_on(model, paths, job)
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

    /// The forge page of the worktree's review, else of its branch.
    fn url(&self, model: &Model, _list: List) -> Option<String> {
        match model.work_row()? {
            Row::Item(index) => {
                let work = &model.snapshot.work[index];
                let tree = work.tree()?;
                if let Some(url) = tree.ci.as_ref().and_then(Ci::review_url) {
                    return Some(url.to_owned());
                }
                let forge = model.snapshot.forges.get(work.repo()?)?;
                Some(match &tree.branch {
                    Some(branch) => forge.branch_url(branch),
                    None => forge.url.clone(),
                })
            }
            Row::Group { .. } => None,
        }
    }
}

/// worktrunk's status symbols, such as `!?↑`: warning when the tree is dirty.
fn symbols(tree: &Worktree, palette: &Palette) -> Span<'static> {
    Span::styled(tree.symbols.clone(), status_style(tree, palette))
}

fn status_style(tree: &Worktree, palette: &Palette) -> Style {
    Style::new().fg(if tree.dirty {
        palette.warn
    } else {
        palette.dim
    })
}

/// `↓N` when the branch is behind its upstream.
fn behind(tree: &Worktree, palette: &Palette) -> Option<Span<'static>> {
    let (_, behind) = tree.upstream.filter(|&(_, behind)| behind > 0)?;
    Some(Span::styled(
        format!("↓{behind}"),
        Style::new().fg(palette.warn),
    ))
}

/// A CI status's colour, as worktrunk's: dimmed when stale or for a draft.
fn ci_style(ci: &Ci, palette: &Palette) -> Style {
    let color = match ci.state {
        CiState::Passed => palette.ok,
        CiState::Running => palette.info,
        CiState::Failed => palette.error,
        CiState::Conflicts | CiState::Error => palette.warn,
        CiState::ChangesRequested => palette.changes_requested,
        CiState::ApprovalPending => palette.approval_pending,
    };
    let style = Style::new().fg(color);
    if ci.stale || ci.draft() {
        style.add_modifier(Modifier::DIM)
    } else {
        style
    }
}

/// A row's CI mark, its colour the status.
fn ci_mark(ci: &Ci, palette: &Palette) -> Span<'static> {
    let glyph = if ci.state == CiState::Error {
        palette.glyphs.ci_error
    } else {
        palette.glyphs.ci
    };
    Span::styled(glyph, ci_style(ci, palette))
}

/// The detail's CI: the row's mark, what it means, and why it may be dimmed.
fn ci_detail(ci: &Ci, palette: &Palette) -> Line<'static> {
    let mut text = format!(" {}", ci.state.label());
    if ci.branch_workflow {
        text.push_str(" (branch)");
    }
    if ci.stale {
        text.push_str(" · stale");
    }
    if ci.draft() {
        text.push_str(" · draft");
    }
    Line::from(vec![
        ci_mark(ci, palette),
        Span::styled(text, ci_style(ci, palette)),
    ])
}

/// The detail's review: its reference and decision, coloured as the CI mark shows it.
fn review_detail(
    ci: &Ci,
    review: &CiReview,
    forge: Option<&Forge>,
    palette: &Palette,
) -> Line<'static> {
    let reference = match (review.number, forge) {
        (Some(number), Some(forge)) => forge.review_reference(number),
        (Some(number), None) => format!("#{number}"),
        (None, _) => "open".into(),
    };
    let mut spans = vec![Span::raw(reference)];
    if let Some(decision) = review.decision {
        let style = match decision {
            Decision::ChangesRequested => Style::new().fg(palette.changes_requested),
            Decision::Pending => Style::new().fg(palette.approval_pending),
            Decision::Draft => Style::new().fg(palette.dim),
            // Approval leaves the CI's colour, as in worktrunk.
            Decision::Approved => ci_style(ci, palette),
        };
        spans.push(Span::styled(format!(" {}", decision.label()), style));
    }
    Line::from(spans)
}

/// A finished worktree's mark: why it is finished.
fn finished_mark(signal: Signal, palette: &Palette) -> Span<'static> {
    Span::styled(palette.glyphs.signal(signal), Style::new().fg(palette.dim))
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
