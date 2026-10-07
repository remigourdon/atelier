//! The selectable lists: one module per kind of list, each owning its rows, its detail and
//! what each key does on its selection.

pub mod carnets;
mod issues;
mod repos;
mod reviews;
pub mod work;
mod workspaces;

pub(crate) use issues::review_style;

use std::borrow::Cow;
use std::path::PathBuf;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::app::{
    Action, Cmd, Effect, Job, Kind, List, MenuEntry, Modal, Model, Submit, Work, WorkKind,
};
use super::update::{run, workspace_menu};
use super::view::Palette;
use crate::finish::Scope;
use crate::issues::Issue;
use crate::issues::TrackerConfig;
use crate::links::{Group, group_text};
use crate::reviews::Review;
use crate::state::Repo;

/// A list's row, resolved once per update or frame: its identity, which keeps the selection
/// across a refresh, its line, and what it offers.
pub struct ListRow<'a> {
    pub id: String,
    pub line: Line<'a>,
    /// What the list's verbs act on and its detail describes.
    pub target: Target<'a>,
    /// What `y` copies.
    pub path: Option<String>,
    pub branch: Option<String>,
    /// What `o` opens.
    pub url: Option<String>,
}

/// What a row stands for. Rows borrow it from the model; the verbs take it owned, to change the
/// model.
#[derive(Debug, Clone)]
pub enum Target<'a> {
    Workspace(Cow<'a, str>),
    Repo(Cow<'a, Repo>),
    Review(Cow<'a, Review>),
    Issue(Cow<'a, Issue>),
    Item(Cow<'a, Work>),
    /// A Work group header; `key` is its fold's.
    Group {
        key: String,
        group: Group,
        members: Vec<Cow<'a, Work>>,
    },
}

impl Target<'_> {
    pub fn into_owned(self) -> Target<'static> {
        match self {
            Target::Workspace(name) => Target::Workspace(Cow::Owned(name.into_owned())),
            Target::Repo(repo) => Target::Repo(Cow::Owned(repo.into_owned())),
            Target::Review(review) => Target::Review(Cow::Owned(review.into_owned())),
            Target::Issue(issue) => Target::Issue(Cow::Owned(issue.into_owned())),
            Target::Item(work) => Target::Item(Cow::Owned(work.into_owned())),
            Target::Group {
                key,
                group,
                members,
            } => Target::Group {
                key,
                group,
                members: (members.into_iter())
                    .map(|work| Cow::Owned(work.into_owned()))
                    .collect(),
            },
        }
    }

    /// The item whose README and commits the main view shows.
    pub fn item(&self) -> Option<&Work> {
        match self {
            Target::Item(work) => Some(work),
            _ => None,
        }
    }

    /// The items the row covers: its item, or its group's members.
    pub fn items(&self) -> Vec<&Work> {
        match self {
            Target::Item(work) => vec![work],
            Target::Group { members, .. } => members.iter().map(|work| work.as_ref()).collect(),
            _ => Vec::new(),
        }
    }
}

/// The selected row of a list's `rows`; `None` when it is empty.
pub fn selected<'r, 'a>(
    model: &Model,
    list: List,
    rows: &'r [ListRow<'a>],
) -> Option<&'r ListRow<'a>> {
    rows.get(model.index(list).min(rows.len().saturating_sub(1)))
}

/// A list's selected row, resolved for an update.
pub fn selection(model: &Model, list: List) -> Option<ListRow<'_>> {
    let mut rows = model.rows(list);
    let index = model.index(list).min(rows.len().checked_sub(1)?);
    Some(rows.swap_remove(index))
}

/// A kind of list. Its verbs receive the selected row's target, resolved once; operations a list
/// doesn't support do nothing.
pub trait ListKind: Sync {
    /// What the keymap calls it.
    fn kind(&self) -> Kind;
    /// Takes the `List`, so the issue sections share one implementation.
    fn title<'a>(&self, model: &'a Model, list: List) -> &'a str;
    /// The rows, in order; the `List` says which section or which reviews.
    fn rows<'a>(&self, model: &'a Model, palette: &Palette, list: List) -> Vec<ListRow<'a>>;
    /// The key/value detail of the selection, atop the main view, coloured as its row is.
    fn detail(
        &self,
        model: &Model,
        palette: &Palette,
        target: &Target,
    ) -> Vec<(String, Line<'static>)>;
    /// What an empty list says.
    fn empty(&self, model: &Model) -> &'static str {
        if model.loaded {
            "nothing here"
        } else {
            "loading…"
        }
    }
    fn activate(&self, _model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        Vec::new()
    }
    /// `Enter` on the selection; `false` when it focuses the main view instead.
    fn enter(&self, _model: &mut Model, _selected: Option<Target<'static>>) -> bool {
        false
    }
    fn create(&self, _model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        Vec::new()
    }
    fn edit(&self, _model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        Vec::new()
    }
    fn move_to(&self, _model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        Vec::new()
    }
    fn remove(&self, _model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        Vec::new()
    }
    /// `f`: fetches, then shows the finish plan of what the selection covers.
    fn finish(&self, _model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        Vec::new()
    }
    /// `Esc` on the list, once its filter is clear; `false` when it does nothing.
    fn back(&self, _model: &mut Model) -> bool {
        false
    }
    /// A command only some lists act on, such as `x`, `c` or `p`; the others ignore it.
    fn command(
        &self,
        _model: &mut Model,
        _selected: Option<Target<'static>>,
        _cmd: Cmd,
    ) -> Vec<Effect> {
        Vec::new()
    }
}

impl Model {
    /// A list's rows, resolved for an update, which shows none of their lines.
    pub fn rows(&self, list: List) -> Vec<ListRow<'_>> {
        of(list).rows(self, &Palette::new(self.icons), list)
    }
}

/// The behaviour of a list.
pub fn of(list: List) -> &'static dyn ListKind {
    match list {
        List::Workspaces => &workspaces::Workspaces,
        List::Repos => &repos::Repos,
        List::Work => &work::WorkList,
        List::Carnets => &carnets::Carnets,
        List::ToReview | List::Mine => &reviews::Reviews,
        List::Section(_) => &issues::Issues,
    }
}

/// Fetches the repos `scope` may touch, then builds its plan.
fn plan(model: &mut Model, scope: Scope) -> Vec<Effect> {
    let repos = scope.repos(&model.snapshot);
    vec![run(model, Job::Plan { scope, repos })]
}

/// `e` on an item: a menu to move it to a group, or out of any, to edit its issue keys,
/// comma-separated, prefilled as stored, or a carnet's summary.
fn edit_links(model: &mut Model, work: &Work) -> Vec<Effect> {
    let group = Action::Ask {
        title: format!("Group of {}", work.title()),
        initial: group_text(work.group()).to_owned(),
        then: Submit::Group(vec![work.path.clone()]),
        groups: model.linked().groups(),
    };
    let issue_keys = Action::ask(
        format!("Issue keys of {}", work.title()),
        work.links.issue_keys.join(", "),
        Submit::IssueKeys(work.path.clone()),
    );
    let entry = |key: &str, label: &str, action| MenuEntry {
        key: key.into(),
        label: label.into(),
        action,
    };
    let mut entries = vec![
        entry("g", "group", group),
        entry("i", "issue keys", issue_keys),
    ];
    if let WorkKind::Carnet { summary, .. } = &work.kind {
        let summary = Action::ask(
            format!("Summary of {}", work.title()),
            summary.clone(),
            Submit::Summary(work.path.clone()),
        );
        entries.push(entry("s", "summary", summary));
    }
    model.modal = Some(Modal::menu(format!("Edit {}", work.title()), entries));
    Vec::new()
}

/// `m` on items: a menu, titled `title`, of the other workspaces to move `paths` to, from
/// `current`.
fn move_menu(model: &mut Model, title: String, paths: &[PathBuf], current: &str) -> Vec<Effect> {
    workspace_menu(model, title, current, |workspace| Job::Move {
        paths: paths.to_vec(),
        workspace,
    })
}

fn paths(works: &[&Work]) -> Vec<PathBuf> {
    works.iter().map(|work| work.path().clone()).collect()
}

/// Closes the given tabs, doing nothing for an empty selection.
fn close_tabs(model: &mut Model, paths: Vec<PathBuf>) -> Vec<Effect> {
    if paths.is_empty() {
        Vec::new()
    } else {
        vec![run(model, Job::Close(paths))]
    }
}

/// What the main view calls an item.
fn kind(work: &Work) -> &'static str {
    if work.is_carnet() {
        "Carnet"
    } else {
        "Worktree"
    }
}

fn pair(key: &str, value: impl Into<Line<'static>>) -> (String, Line<'static>) {
    (key.to_owned(), value.into())
}

/// Secondary text, such as a placeholder or a timestamp, in the style guide's subtle colour.
pub(super) fn subtle(text: impl Into<Cow<'static, str>>, palette: &Palette) -> Span<'static> {
    Span::styled(text, Style::new().fg(palette.dim))
}

/// An issue's labels.
fn tag_style(palette: &Palette) -> Style {
    Style::new().fg(palette.info)
}

/// A group, never coloured as an issue key, which can look the same in uppercase.
pub fn group_style(palette: &Palette) -> Style {
    Style::new().fg(palette.group)
}

/// An issue key.
pub fn key_style(palette: &Palette) -> Style {
    Style::new().fg(palette.issue_key)
}

/// Dims a finished or closed item's row after its `lead` spans: its tab dot or spinner keeps its
/// colour.
fn dim_after(spans: &mut [Span], lead: usize, palette: &Palette) {
    let dim = Style::new().fg(palette.dim);
    spans[lead..].iter_mut().for_each(|span| span.style = dim);
}

/// A workspace's name, wherever an item shows the one it is in.
pub fn workspace_style(palette: &Palette) -> Style {
    Style::new().fg(palette.workspace)
}

/// A workspace's name, in its colour.
fn workspace_span(workspace: &str, palette: &Palette) -> Span<'static> {
    Span::styled(workspace.to_owned(), workspace_style(palette))
}

/// An optional group, as the detail shows it.
fn group_span(group: Option<&Group>, palette: &Palette) -> Span<'static> {
    Span::styled(group_text(group).to_owned(), group_style(palette))
}

/// An item's issue keys as shown, joined by `separator`.
fn issue_keys(
    work: &Work,
    tracker: &TrackerConfig,
    separator: &str,
    palette: &Palette,
) -> Span<'static> {
    let keys = work.links.issue_keys.display(tracker, separator);
    Span::styled(keys, key_style(palette))
}

/// The mark of an item whose tab is open or closed, and its style.
pub(crate) fn tab(open: bool, palette: &Palette) -> (&'static str, Style) {
    if open {
        (palette.glyphs.open, Style::new().fg(palette.ok))
    } else {
        (palette.glyphs.closed, Style::new().fg(palette.dim))
    }
}

/// A row's tab mark.
fn tab_mark(open: bool, palette: &Palette) -> Span<'static> {
    let (glyph, style) = tab(open, palette);
    Span::styled(format!("{glyph} "), style)
}

/// `open` or `closed`, coloured as the tab mark.
fn tab_word(open: bool, palette: &Palette) -> Span<'static> {
    let (_, style) = tab(open, palette);
    Span::styled(if open { "open" } else { "closed" }, style)
}

/// An item in a detail: `repo:branch · workspace · tab open`.
fn work_line(work: &Work, palette: &Palette) -> Line<'static> {
    Line::from(vec![
        tab_mark(work.tab, palette),
        Span::raw(format!("{} · ", work.title())),
        workspace_span(&work.workspace, palette),
        Span::raw(" · tab "),
        tab_word(work.tab, palette),
    ])
}

/// The detail's tab state, marked as its row is.
fn tab_detail(open: bool, palette: &Palette) -> Line<'static> {
    Line::from(vec![tab_mark(open, palette), tab_word(open, palette)])
}
