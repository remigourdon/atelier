//! Panel 2's Work list: the selected workspace's worktrees and open carnets, and closed
//! carnets with their tab open, in groups.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{
    ListKind, carnets, edit_links, group_span, group_style, issue_keys, kind, move_menu, pair,
    paths, plan, subtle, tab_detail, tab_mark,
};
use crate::finish::{self, Scope, Signal};
use crate::links::{Group, group_text};
use crate::tui::app::{
    Action, Cmd, Draft, DraftStep, Effect, Job, Kind, List, MenuEntry, Modal, Model, Removal,
    Submit, Work, WorkKind,
};
use crate::tui::marks::{self, Severity, Tone};
use crate::tui::update::{confirm, note, run, update};
use crate::tui::view::{Palette, icon};
use crate::worktrunk::{Checks, Ci, CiReview, Decision, Forge, Worktree};

pub struct WorkList;

/// A row of the Work panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A group header; `members` index `Snapshot::work`.
    Group {
        key: String,
        group: Group,
        members: Vec<usize>,
        folded: bool,
    },
    Item(usize),
}

impl Model {
    pub fn is_folded(&self, key: &str) -> bool {
        self.folded.contains(key)
    }

    pub fn set_folded(&mut self, key: &str, folded: bool) {
        if folded {
            self.folded.insert(key.to_owned());
        } else {
            self.folded.remove(key);
        }
    }

    /// Every item in every workspace, closed carnets included; a carnet can be listed twice.
    fn every_item(&self) -> impl Iterator<Item = &Work> {
        (self.snapshot.work.iter()).chain(&self.snapshot.carnets)
    }

    /// Every group of every item in every workspace, carnets included, once each, sorted: what
    /// a group prompt completes to.
    pub fn groups(&self) -> Vec<Group> {
        let groups: BTreeSet<&Group> = self.every_item().filter_map(Work::group).collect();
        groups.into_iter().cloned().collect()
    }

    /// Every item in `group`, in every workspace, closed carnets included, once each.
    fn group_members(&self, group: &Group) -> Vec<PathBuf> {
        let paths: BTreeSet<&PathBuf> = (self.every_item())
            .filter(|work| work.group() == Some(group))
            .map(|work| &work.path)
            .collect();
        paths.into_iter().cloned().collect()
    }

    /// Panel 2's rows: named groups, foldable, then the items in no group. Worktrees come
    /// before carnets, which are newest first.
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
                        &[
                            &work.title(),
                            group_text(work.group()),
                            &work.path.to_string_lossy(),
                            work.summary(),
                        ],
                    )
            })
            .collect();
        // Named groups, then the items in no group.
        let section = |work: &Work| (work.group().is_none(), work.group().cloned());
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
            let end = members[index..]
                .iter()
                .position(|&other| section(&work[other]) != section(first))
                .map_or(members.len(), |offset| index + offset);
            let slice = &members[index..end];
            match first.group() {
                None => lines.extend(slice.iter().map(|&member| Row::Item(member))),
                Some(group) => {
                    let key = format!("{workspace}\0{group}");
                    let folded = !filtering && self.is_folded(&key);
                    lines.push(Row::Group {
                        key,
                        group: group.clone(),
                        members: slice.to_vec(),
                        folded,
                    });
                    if !folded {
                        lines.extend(slice.iter().map(|&member| Row::Item(member)));
                    }
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
                    group,
                    members,
                    folded,
                    ..
                } => {
                    let open = members
                        .iter()
                        .filter(|&&index| model.snapshot.work[index].tab)
                        .count();
                    let glyph = if folded {
                        palette.glyphs.folded
                    } else {
                        palette.glyphs.unfolded
                    };
                    // Folded, its name takes its members' worst tint.
                    let worst = (members.iter().filter(|_| folded))
                        .map(|&index| severity(&model.snapshot.work[index]))
                        .max()
                        .and_then(|severity| severity.color(palette));
                    let name = worst.map_or(group_style(palette), |color| Style::new().fg(color));
                    Line::from(vec![
                        Span::styled(format!("{glyph} "), group_style(palette).bold()),
                        Span::styled(group.to_string(), name.bold()),
                        Span::styled(format!(" {} · {open} open", members.len()), dim),
                    ])
                }
                Row::Item(index) => {
                    let work = &model.snapshot.work[index];
                    let glyphs = &palette.glyphs;
                    let indent = if work.group().is_none() { "" } else { "  " };
                    let marker = if model.schedule.is_pulling(work.path()) {
                        let frame = glyphs.spinner[model.frame % glyphs.spinner.len()];
                        Span::styled(format!("{frame} "), Style::new().fg(palette.info))
                    } else {
                        tab_mark(work.tab, palette)
                    };
                    let mut spans = vec![Span::raw(indent), marker];
                    if work.is_carnet() {
                        let tracker = &model.tracker_config;
                        return carnets::row(work, spans, None, true, tracker, palette);
                    }
                    spans.extend(icon(glyphs.worktree, dim));
                    spans.push(Span::styled(work.title(), tint(severity(work), palette)));
                    // Finished: dimmed, all but the tab dot or spinner.
                    if finish::signal(work).is_some() {
                        spans[2..].iter_mut().for_each(|span| span.style = dim);
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
            Some(Row::Group { group, members, .. }) => {
                let mut pairs = vec![pair("Group", group_span(Some(&group), palette))];
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
                    WorkKind::Carnet { .. } => {
                        return carnets::detail(work, &model.tracker_config, palette);
                    }
                };
                let status_text = if tree.dirty {
                    format!("dirty +{} -{}", tree.diff.0, tree.diff.1)
                } else {
                    "clean".into()
                };
                let mut status = Vec::new();
                if !tree.symbols.is_empty() {
                    status.extend(symbols(tree, palette));
                    status.push(Span::raw(" "));
                }
                status.push(Span::raw(status_text));
                let upstream = match tree.upstream {
                    // Counts of zero recede.
                    Some((ahead, behind)) => {
                        let count = |arrow, count| {
                            let text = format!("{arrow}{count}");
                            if count > 0 {
                                Span::raw(text)
                            } else {
                                subtle(text, palette)
                            }
                        };
                        Line::from(vec![count("↑", ahead), Span::raw(" "), count("↓", behind)])
                    }
                    None => subtle("none", palette).into(),
                };
                let mut pairs = vec![
                    pair("Repo", repo_name.clone()),
                    pair("Branch", work.branch()),
                    pair("Path", tree.path.display().to_string()),
                    pair("Workspace", work.workspace.clone()),
                    pair("Group", group_span(work.group(), palette)),
                    pair(
                        "Issue keys",
                        issue_keys(work, &model.tracker_config, ", ", palette),
                    ),
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
            Some(Row::Group { group, .. }) => Some(group),
            Some(Row::Item(index)) => model.snapshot.work[index].group().cloned(),
            None => None,
        };
        let ask = |repo: PathBuf, name: String| {
            Action::ask(
                format!("New worktree of {name}: branch"),
                "",
                Submit::Branch {
                    repo,
                    workspace: workspace.clone(),
                    group: group.clone(),
                },
            )
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
                action: Action::ask(
                    "New carnet: summary",
                    "",
                    Submit::Carnet {
                        draft: Draft {
                            workspace,
                            group,
                            ..Draft::default()
                        },
                        step: DraftStep::Summary,
                    },
                ),
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
        model.modal = Some(Modal::menu(title, entries));
        Vec::new()
    }

    /// Edits the selected item's group or issue keys, or renames the selected group in every
    /// workspace.
    fn edit(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        match model.work_row() {
            Some(Row::Item(index)) => {
                let work = model.snapshot.work[index].clone();
                edit_links(model, &work)
            }
            Some(Row::Group { group, .. }) => {
                let action = Action::Ask {
                    title: format!("Rename group {group}"),
                    initial: group.to_string(),
                    then: Submit::Group(model.group_members(&group)),
                    groups: model.groups(),
                };
                update(model, action)
            }
            None => Vec::new(),
        }
    }

    fn move_to(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let targets = model.targets();
        let Some(first) = targets.first() else {
            return Vec::new();
        };
        let current = first.workspace.clone();
        let paths = paths(&targets);
        let title = format!("Move {} item(s) to", paths.len());
        move_menu(model, title, &paths, &current)
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
        let mut groups: Vec<Group> = Vec::new();
        let mut items = Vec::new();
        for work in targets {
            match work.group() {
                None => items.push(work.path.clone()),
                Some(group) if !groups.contains(group) => groups.push(group.clone()),
                Some(_) => {}
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
            Row::Group { group, .. } => Some(group.to_string()),
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

/// A Work item's severity: a finished worktree's row is dimmed instead, and a carnet has none.
pub(crate) fn severity(work: &Work) -> Severity {
    match work.tree() {
        Some(tree) if finish::signal(work).is_none() => Severity::of(tree),
        _ => Severity::Fine,
    }
}

/// A name in its severity's colour.
pub(crate) fn tint(severity: Severity, palette: &Palette) -> Style {
    severity
        .color(palette)
        .map_or(Style::new(), |color| Style::new().fg(color))
}

/// worktrunk's status symbols, such as `!?↑`, each in its own colour.
pub(crate) fn symbols(tree: &Worktree, palette: &Palette) -> Vec<Span<'static>> {
    (tree.symbols.chars())
        .map(|mark| {
            let tone = marks::lookup(mark).map_or(Tone::Quiet, |symbol| symbol.tone);
            Span::styled(mark.to_string(), Style::new().fg(tone.color(palette)))
        })
        .collect()
}

/// A check status's glyph and colour.
pub(crate) fn checks_mark(checks: Checks, palette: &Palette) -> (&'static str, Color) {
    let glyphs = &palette.glyphs;
    match checks {
        Checks::Passed => (glyphs.passed, palette.ok),
        Checks::Running => (glyphs.running, palette.info),
        Checks::Failed => (glyphs.failed, palette.error),
        Checks::Unavailable => (glyphs.unavailable, palette.warn),
    }
}

/// What a check status is called.
pub(crate) fn checks_label(checks: Checks) -> &'static str {
    match checks {
        Checks::Passed => "passed",
        Checks::Running => "running",
        Checks::Failed => "failed",
        Checks::Unavailable => "unavailable",
    }
}

/// The checks' mark, dimmed when stale or for a draft; `None` without checks.
pub(crate) fn checks_span(ci: &Ci, palette: &Palette) -> Option<Span<'static>> {
    let (glyph, color) = checks_mark(ci.checks?, palette);
    let style = Style::new().fg(color);
    let style = if ci.stale || ci.draft() {
        style.add_modifier(Modifier::DIM)
    } else {
        style
    };
    Some(Span::styled(glyph, style))
}

/// A review decision's glyph and colour, none for a draft's.
pub(crate) fn decision_mark(
    decision: Decision,
    palette: &Palette,
) -> Option<(&'static str, Color)> {
    let glyphs = &palette.glyphs;
    match decision {
        Decision::ChangesRequested => Some((glyphs.changes_requested, palette.changes_requested)),
        Decision::Pending => Some((glyphs.approval, palette.approval_pending)),
        Decision::Approved => Some((glyphs.passed, palette.ok)),
        Decision::Draft => None,
    }
}

/// What a review decision is called.
pub(crate) fn decision_label(decision: Decision) -> &'static str {
    match decision {
        Decision::Pending => "waiting for approval",
        decision => decision.label(),
    }
}

/// A review's merge conflicts: its glyph and colour.
pub(crate) fn conflicts_mark(palette: &Palette) -> (&'static str, Color) {
    (palette.glyphs.conflicts, palette.error)
}

/// The detail's CI: its checks' mark, what it means, and why it may be dimmed.
fn ci_detail(ci: &Ci, palette: &Palette) -> Line<'static> {
    let Some(checks) = ci.checks else {
        return subtle("none", palette).into();
    };
    let mut text = format!(" {}", checks_label(checks));
    if ci.branch_workflow {
        text.push_str(" (branch)");
    }
    if ci.stale {
        text.push_str(" · stale");
    }
    if ci.draft() {
        text.push_str(" · draft");
    }
    let mark = checks_span(ci, palette).unwrap_or_default();
    Line::from(vec![mark.clone(), Span::styled(text, mark.style)])
}

/// The detail's review: its reference, its decision and its conflicts, marked.
fn review_detail(
    ci: &Ci,
    review: &CiReview,
    forge: Option<&Forge>,
    palette: &Palette,
) -> Line<'static> {
    let mut spans = vec![Span::raw(review_reference(review, forge))];
    if let Some((glyph, color)) = review.decision.and_then(|d| decision_mark(d, palette)) {
        let label = decision_label(review.decision.unwrap_or(Decision::Draft));
        spans.push(Span::styled(
            format!(" {glyph} {label}"),
            Style::new().fg(color),
        ));
    }
    if ci.conflicts {
        let (glyph, color) = conflicts_mark(palette);
        spans.push(Span::styled(
            format!(" {glyph} conflicts"),
            Style::new().fg(color),
        ));
    }
    Line::from(spans)
}

/// How the forge refers to a review: `#12`, `!12` on GitLab, `open` without a number.
pub(crate) fn review_reference(review: &CiReview, forge: Option<&Forge>) -> String {
    match (review.number, forge) {
        (Some(number), Some(forge)) => forge.review_reference(number),
        (Some(number), None) => format!("#{number}"),
        (None, _) => "open".into(),
    }
}

/// A finished worktree's mark: why it is finished.
pub(crate) fn finished_mark(signal: Signal, palette: &Palette) -> Span<'static> {
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
                Row::Group { group, .. } => format!("[{group}]"),
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
    fn named_groups_come_first_then_ungrouped_worktrees_then_carnets_newest_first() {
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
                "2026-10-02-ideas",
                "2026-09-20-old",
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
    fn groups_start_unfolded_and_fold_on_their_own() {
        let mut model = grouped();
        let Some(Row::Group { key, folded, .. }) = model.work_rows().into_iter().next() else {
            panic!("no group");
        };
        assert!(!folded);
        model.set_folded(&key, true);
        assert_eq!(
            titles(&model)[..3],
            ["[ABC-1]", "[XYZ-2]", "web:XYZ-2-menu"]
        );
        model.set_folded(&key, false);
        assert_eq!(titles(&model).len(), 10);
    }
}
