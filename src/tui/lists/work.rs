//! Panel 2's Work list: the selected workspace's worktrees and open carnets, and closed
//! carnets with their tab open, in groups.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::{
    ListKind, carnets, dim_after, edit_links, group_span, group_style, issue_keys, kind, move_menu,
    pair, paths, plan, subtle, tab_detail, tab_mark, workspace_span,
};
use crate::finish::{self, Scope, Signal};
use crate::links::{Group, group_text};
use crate::tui::app::{
    Action, Cmd, Draft, DraftStep, Effect, Job, Kind, List, MenuEntry, Modal, Model, Removal,
    Submit, Work, WorkKind,
};
use crate::tui::marks::{self, Mark, Part, Severity, Symbol, Tone};
use crate::tui::update::{confirm, note, run, update};
use crate::tui::view::{Palette, icon};
use crate::worktrunk::{Ci, CiReview, Forge, Worktree};

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
                    let lead = spans.len();
                    spans.extend(icon(glyphs.worktree, dim));
                    spans.push(Span::styled(work.title(), severity(work).style(palette)));
                    if finish::signal(work).is_some() {
                        dim_after(&mut spans, lead, palette);
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
                let pulling = model.schedule.is_pulling(work.path());
                let forge = model.snapshot.forges.get(repo);
                let mut pairs = vec![
                    pair("Repo", repo_name.clone()),
                    pair("Branch", work.branch()),
                    pair("Path", tree.path.display().to_string()),
                    pair("Workspace", workspace_span(&work.workspace, palette)),
                    pair("Group", group_span(work.group(), palette)),
                    pair(
                        "Issue keys",
                        issue_keys(work, &model.tracker_config, ", ", palette),
                    ),
                ];
                pairs.extend(tree_detail(tree, work.tab, pulling, forge, palette));
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

/// worktrunk's status symbols, such as `!?↑`, each in its own colour.
pub(crate) fn colored_symbols(tree: &Worktree, palette: &Palette) -> Vec<Span<'static>> {
    (tree.symbols.chars())
        .map(|mark| {
            Span::styled(
                mark.to_string(),
                Style::new().fg(Tone::of(mark).color(palette)),
            )
        })
        .collect()
}

/// One line of the detail: a fact's spans.
type Fact = Vec<Span<'static>>;

/// A mark in its colour, then what it means.
fn explain(mark: impl Into<String>, color: Color, words: impl Into<String>) -> Fact {
    vec![
        Span::styled(mark.into(), Style::new().fg(color)),
        Span::raw(format!(" {}", words.into())),
    ]
}

/// A fact's mark in its colour, then `words`.
fn explain_mark(mark: Mark, words: impl Into<String>, palette: &Palette) -> Fact {
    let glyph = (mark.glyph)(&palette.glyphs);
    explain(glyph, mark.tone.color(palette), words)
}

/// A status symbol in its colour, then `words`.
fn explain_symbol(symbol: &Symbol, words: impl Into<String>, palette: &Palette) -> Fact {
    explain(symbol.mark, symbol.tone.color(palette), words)
}

/// `n` of `thing`, plural past one.
fn count(n: u64, thing: &str) -> String {
    if n == 1 {
        format!("1 {thing}")
    } else {
        format!("{n} {thing}s")
    }
}

/// What a finished worktree's mark means.
pub(crate) fn finished_label(signal: Signal) -> &'static str {
    match signal {
        Signal::Integrated => "merged into the default branch",
        Signal::Gone => "its remote branch was deleted",
    }
}

/// A worktree's state in the detail, one fact a line, each mark with its meaning in words, under
/// the key of its section, which shows on its first line only: its tab, changes, checkout, the
/// default branch, the remote, checks, review, decision, merge and whether it is finished.
fn tree_detail(
    tree: &Worktree,
    tab: bool,
    pulling: bool,
    forge: Option<&Forge>,
    palette: &Palette,
) -> Vec<(String, Line<'static>)> {
    let ci = tree.ci.as_ref();
    let review = ci.and_then(|ci| Some((ci, ci.review.as_ref()?)));
    let sections: [(&str, Vec<Fact>); 10] = [
        ("Tab", vec![tab_fact(tab, pulling, palette)]),
        (Part::Changes.section(), change_facts(tree, palette)),
        (Part::Checkout.section(), checkout_facts(tree, palette)),
        (Part::Default.section(), default_branch_facts(tree, palette)),
        (Part::Remote.section(), vec![remote_fact(tree, palette)]),
        (
            "Checks",
            Vec::from_iter(ci.and_then(|ci| checks_fact(ci, palette))),
        ),
        (
            "Review",
            Vec::from_iter(review.map(|(ci, review)| review_fact(ci, review, forge, palette))),
        ),
        (
            "Decision",
            Vec::from_iter(review.and_then(|(_, review)| decision_fact(review, palette))),
        ),
        (
            "Merge",
            Vec::from_iter(review.map(|(ci, _)| merge_fact(tree, ci, palette))),
        ),
        ("Finished", Vec::from_iter(finished_fact(tree, palette))),
    ];
    (sections.into_iter())
        .flat_map(|(key, facts)| {
            (facts.into_iter().enumerate())
                .map(move |(index, fact)| pair(if index == 0 { key } else { "" }, fact))
        })
        .collect()
}

fn tab_fact(tab: bool, pulling: bool, palette: &Palette) -> Fact {
    if pulling {
        explain(palette.glyphs.spinner[0], palette.info, "pulling")
    } else {
        tab_detail(tab, palette).spans
    }
}

/// worktrunk's `+ ! ?` one a line, then the lines changed; `clean` without any.
fn change_facts(tree: &Worktree, palette: &Palette) -> Vec<Fact> {
    let mut facts: Vec<Fact> = (marks::symbols_in(tree, Part::Changes))
        .map(|symbol| explain(symbol.mark, palette.text, symbol.help))
        .collect();
    if facts.is_empty() && !tree.dirty {
        return vec![vec![subtle("clean", palette)]];
    }
    let (added, deleted) = tree.diff;
    facts.push(vec![Span::raw(format!("+{added} −{deleted} lines"))]);
    facts
}

fn checkout_facts(tree: &Worktree, palette: &Palette) -> Vec<Fact> {
    (marks::symbols_in(tree, Part::Checkout))
        .map(|symbol| explain_symbol(symbol, symbol.help, palette))
        .collect()
}

/// Against the default branch, with the commits ahead, and the branch's name where a line
/// compares with it.
fn default_branch_facts(tree: &Worktree, palette: &Palette) -> Vec<Fact> {
    (marks::symbols_in(tree, Part::Default))
        .map(|symbol| {
            let help = symbol.help;
            let words = match (symbol.mark, tree.ahead_of_default) {
                ("↑", Some(n)) => format!("{help} by {}", count(n, "commit")),
                ("↕", Some(n)) => format!("{help}, {} ahead", count(n, "commit")),
                _ => help.into(),
            };
            let mut fact = explain_symbol(symbol, words, palette);
            let compares = !matches!(symbol.mark, "^" | "∅");
            if let Some(branch) = tree.default_branch.as_ref().filter(|_| compares) {
                fact.push(subtle(format!(" ({branch})"), palette));
            }
            fact
        })
        .collect()
}

/// Against the remote, with the commits to push and pull; `no upstream` without one.
fn remote_fact(tree: &Worktree, palette: &Palette) -> Fact {
    let Some((ahead, behind)) = tree.upstream else {
        return vec![subtle("no upstream", palette)];
    };
    let (mark, counts) = match (ahead, behind) {
        (0, 0) => ("|", None),
        (ahead, 0) => ("⇡", Some(ahead.to_string())),
        (0, behind) => ("⇣", Some(behind.to_string())),
        (ahead, behind) => ("⇅", Some(format!("{ahead} to push, {behind} to pull"))),
    };
    let symbol = marks::find(mark);
    let help = symbol.help;
    let words = counts.map_or(help.into(), |counts| format!("{help} ({counts})"));
    explain_symbol(symbol, words, palette)
}

/// `None` without checks.
fn checks_fact(ci: &Ci, palette: &Palette) -> Option<Fact> {
    let mut fact = vec![
        marks::checks_span(ci, palette)?,
        Span::raw(format!(" {}", marks::checks(ci.checks?).words)),
    ];
    if ci.branch_workflow {
        fact.push(subtle(" (branch workflow)", palette));
    }
    if ci.stale {
        fact.push(subtle(" · stale: local commits not pushed", palette));
    }
    Some(fact)
}

fn review_fact(ci: &Ci, review: &CiReview, forge: Option<&Forge>, palette: &Palette) -> Fact {
    let mut fact = vec![Span::raw(review_reference(review, forge))];
    if ci.draft() {
        fact.push(subtle(" draft", palette));
    }
    fact
}

/// `None` without a decision, or for a draft's, which the review's line says.
fn decision_fact(review: &CiReview, palette: &Palette) -> Option<Fact> {
    let mark = marks::decision(review.decision?)?;
    Some(explain_mark(mark, mark.words, palette))
}

fn merge_fact(tree: &Worktree, ci: &Ci, palette: &Palette) -> Fact {
    if !ci.conflicts {
        return vec![Span::raw("mergeable")];
    }
    let base = (tree.default_branch.as_deref()).unwrap_or("the default branch");
    let mark = marks::CONFLICTS;
    explain_mark(mark, format!("{} with {base}", mark.words), palette)
}

/// Why it is finished; `None` while it is not.
fn finished_fact(tree: &Worktree, palette: &Palette) -> Option<Fact> {
    let signal = finish::tree_signal(tree)?;
    Some(vec![
        finished_mark(signal, palette),
        Span::raw(format!(" {}", finished_label(signal))),
        subtle(" (f to finish)", palette),
    ])
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

    /// The detail as text, a line per fact: its key, or `-` under the one above.
    fn facts(tree: &Worktree, tab: bool, pulling: bool) -> Vec<String> {
        let palette = Palette::new(crate::config::Icons::Unicode);
        (tree_detail(tree, tab, pulling, None, &palette).into_iter())
            .map(|(key, line)| {
                let key = if key.is_empty() { "-".into() } else { key };
                format!("{key}: {line}")
            })
            .collect()
    }

    #[test]
    fn the_detail_spells_out_every_mark() {
        use crate::worktrunk::{Checks, CiReview, CiState, Decision};
        let tree = Worktree {
            dirty: true,
            diff: (48, 12),
            symbols: "+!?↻↑⇅".into(),
            ahead_of_default: Some(3),
            default_branch: Some("main".into()),
            upstream: Some((1, 2)),
            ci: Some(Ci {
                state: Some(CiState::Conflicts),
                checks: Some(Checks::Passed),
                conflicts: true,
                stale: true,
                branch_workflow: false,
                review: Some(CiReview {
                    number: Some(464),
                    url: None,
                    decision: Some(Decision::Pending),
                }),
            }),
            ..Worktree::default()
        };
        assert_eq!(
            facts(&tree, true, false),
            [
                "Tab: ◉ open",
                "Changes: + staged changes",
                "-: ! unstaged changes",
                "-: ? untracked files",
                "-: +48 −12 lines",
                "Checkout: ↻ rebase, merge or other operation in progress",
                "Default branch: ↑ ahead by 3 commits (main)",
                "Remote: ⇅ diverged (1 to push, 2 to pull)",
                "Checks: ◆ passed · stale: local commits not pushed",
                "Review: #464",
                "Decision: ◇ waiting for approval",
                "Merge: ✗ conflicts with main",
            ]
        );
        let clean = Worktree {
            integrated: true,
            ..Worktree::default()
        };
        assert_eq!(
            facts(&clean, false, true),
            [
                "Tab: ◐ pulling",
                "Changes: clean",
                "Remote: no upstream",
                "Finished: ⊂ merged into the default branch (f to finish)",
            ]
        );
        let unchecked = Worktree {
            ci: Some(Ci {
                state: None,
                checks: None,
                conflicts: false,
                stale: false,
                branch_workflow: false,
                review: Some(CiReview {
                    number: Some(7),
                    url: None,
                    decision: Some(Decision::Draft),
                }),
            }),
            ..clean
        };
        assert_eq!(
            facts(&unchecked, false, false)[3..],
            [
                "Review: #7 draft",
                "Merge: mergeable",
                "Finished: ⊂ merged into the default branch (f to finish)"
            ],
            "a review with no checks still shows, with no Checks line"
        );
        let main = Worktree {
            symbols: "^".into(),
            default_branch: Some("main".into()),
            ..Worktree::default()
        };
        assert!(
            facts(&main, false, false).contains(&"Default branch: ^ is the main worktree".into()),
            "no branch named where nothing is compared with it"
        );
    }
}
