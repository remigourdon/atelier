//! Panel 2's Carnets list: every carnet in every workspace, closed ones included.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{
    ListKind, close_tabs, edit_links, group_span, group_style, issue_keys, move_menu, pair, subtle,
    tab_detail, tab_mark,
};
use crate::carnet::dated;
use crate::issues::TrackerConfig;
use crate::links::group_text;
use crate::tui::app::{Action, Cmd, Effect, Job, Kind, List, Model, Submit, Work, WorkKind};
use crate::tui::update::{run, update};
use crate::tui::view::{Palette, icon};

pub struct Carnets;

/// A search inside the carnets, which narrows the list to the carnets with hits.
#[derive(Debug, Clone, PartialEq)]
pub struct Search {
    pub text: String,
    /// Each carnet's hit lines, `<file>:<line>:<text>`.
    pub hits: BTreeMap<PathBuf, Vec<String>>,
}

impl Model {
    /// The Carnets rows, newest first: those the search found, matching the filter on their
    /// folder name, group, issue keys and summary.
    pub fn carnet_rows(&self) -> Vec<&Work> {
        (self.snapshot.carnets.iter())
            .filter(|work| {
                (self.search.as_ref()).is_none_or(|search| search.hits.contains_key(&work.path))
            })
            .filter(|work| {
                let (group, keys) = (group_text(work.group()), work.links.issue_keys.join(" "));
                self.matches(
                    List::Carnets,
                    &[&work.title(), group, &keys, work.summary()],
                )
            })
            .collect()
    }

    pub fn carnet(&self) -> Option<&Work> {
        self.carnet_rows().get(self.index(List::Carnets)).copied()
    }

    /// The selected carnet's search hits, while the Carnets list is active.
    pub fn carnet_hits(&self) -> Option<&Vec<String>> {
        if self.active() != List::Carnets {
            return None;
        }
        self.search.as_ref()?.hits.get(&self.carnet()?.path)
    }
}

/// Where a carnet stands, as the colour of its glyph shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Open,
    /// Open, in a workspace other than the selected one.
    Elsewhere,
    Closed,
}

impl Standing {
    /// `work`'s standing, `elsewhere` when it is in a workspace other than the selected one.
    pub fn of(work: &Work, elsewhere: bool) -> Self {
        if work.closed() {
            Self::Closed
        } else if elsewhere {
            Self::Elsewhere
        } else {
            Self::Open
        }
    }

    /// Its glyph's colour.
    pub fn style(self, palette: &Palette) -> Style {
        match self {
            Self::Open => Style::new().fg(palette.info),
            Self::Elsewhere => Style::new().fg(palette.warn),
            Self::Closed => Style::new().fg(palette.dim),
        }
    }
}

/// A carnet's folder name split into its date and the rest, which a scan ensures it has.
fn folder(work: &Work) -> (String, String) {
    let folder = work.title();
    match dated(&folder) {
        Some((date, name)) => (date.to_owned(), name.to_owned()),
        None => (folder.clone(), String::new()),
    }
}

/// A carnet's row after `lead`: its glyph coloured by its `standing`, its date, its summary (else
/// its folder name after the date, as a placeholder), its group unless `grouped` says a header
/// shows it, and its issue keys; dimmed after `lead` when it is closed.
pub fn row(
    work: &Work,
    lead: Vec<Span<'static>>,
    standing: Standing,
    grouped: bool,
    tracker: &TrackerConfig,
    palette: &Palette,
) -> Line<'static> {
    let dim = Style::new().fg(palette.dim);
    let kept = lead.len();
    let mut spans = lead;
    spans.extend(icon(palette.glyphs.carnet, standing.style(palette)));
    let (date, name) = folder(work);
    spans.push(Span::styled(date, dim));
    spans.push(match work.summary() {
        "" => Span::styled(format!(" {name}"), dim.add_modifier(Modifier::ITALIC)),
        summary => Span::raw(format!(" {summary}")),
    });
    if let Some(group) = work.group().filter(|_| !grouped) {
        spans.push(Span::styled(format!(" {group}"), group_style(palette)));
    }
    if !work.links.issue_keys.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(issue_keys(work, tracker, ",", palette));
    }
    // The lead, such as the tab dot, keeps its colour.
    if standing == Standing::Closed {
        spans[kept..].iter_mut().for_each(|span| span.style = dim);
    }
    Line::from(spans)
}

/// A carnet's detail, in either list.
pub fn detail(
    work: &Work,
    tracker: &TrackerConfig,
    palette: &Palette,
) -> Vec<(String, Line<'static>)> {
    let (closed, summary) = match &work.kind {
        WorkKind::Carnet {
            closed, summary, ..
        } => (*closed, summary.clone()),
        WorkKind::Worktree { .. } => return Vec::new(),
    };
    let mut pairs = vec![
        pair("Summary", summary),
        pair("Path", subtle(work.path.display().to_string(), palette)),
        pair("Workspace", work.workspace.clone()),
        pair("Group", group_span(work.group(), palette)),
        pair("Issue keys", issue_keys(work, tracker, ", ", palette)),
        pair("Tab", tab_detail(work.tab, palette)),
    ];
    if closed {
        pairs.push(pair("Closed", subtle("yes", palette)));
    }
    pairs
}

impl ListKind for Carnets {
    fn kind(&self) -> Kind {
        Kind::Carnets
    }

    fn title<'a>(&self, _model: &'a Model, _list: List) -> &'a str {
        "Carnets"
    }

    fn len(&self, model: &Model, _list: List) -> usize {
        model.carnet_rows().len()
    }

    fn ids(&self, model: &Model, _list: List) -> Vec<String> {
        (model.carnet_rows().into_iter())
            .map(|work| work.path.display().to_string())
            .collect()
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, _list: List) -> Vec<Line<'a>> {
        let selected = model.workspace();
        (model.carnet_rows().into_iter())
            .map(|work| {
                let elsewhere = selected.is_some_and(|name| name != work.workspace);
                let standing = Standing::of(work, elsewhere);
                let lead = vec![tab_mark(work.tab, palette)];
                row(work, lead, standing, false, &model.tracker_config, palette)
            })
            .collect()
    }

    fn detail(
        &self,
        model: &Model,
        palette: &Palette,
        _list: List,
    ) -> Vec<(String, Line<'static>)> {
        (model.carnet())
            .map(|work| detail(work, &model.tracker_config, palette))
            .unwrap_or_default()
    }

    fn empty(&self, model: &Model, _list: List) -> &'static str {
        match &model.search {
            Some(_) => "no carnet matches: Esc clears the search",
            None if model.loaded => "no carnets yet: n in Work makes one",
            None => "loading…",
        }
    }

    fn item<'a>(&self, model: &'a Model, _list: List) -> Option<&'a Work> {
        model.carnet()
    }

    /// Edits the carnet's group or issue keys.
    fn edit(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        match model.carnet().cloned() {
            Some(work) => edit_links(model, &work),
            None => Vec::new(),
        }
    }

    /// Moves the carnet, closed or not, to another workspace.
    fn move_to(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(work) = model.carnet() else {
            return Vec::new();
        };
        let (title, paths) = (format!("Move {} to", work.title()), [work.path.clone()]);
        let current = work.workspace.clone();
        move_menu(model, title, &paths, &current)
    }

    /// Opens the carnet's tab; a closed carnet stays closed.
    fn activate(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        match model.carnet() {
            Some(work) => {
                let job = Job::Open(vec![work.path.clone()]);
                vec![run(model, job)]
            }
            None => Vec::new(),
        }
    }

    /// `x` closes the tab, `c` toggles the carnet lifecycle, and `s` searches every carnet.
    fn command(&self, model: &mut Model, _list: List, cmd: Cmd) -> Vec<Effect> {
        match cmd {
            Cmd::Close => {
                let paths = model
                    .carnet()
                    .filter(|work| work.tab)
                    .map(|work| work.path.clone())
                    .into_iter()
                    .collect();
                close_tabs(model, paths)
            }
            Cmd::ToggleCarnet => {
                let Some(work) = model.carnet() else {
                    return Vec::new();
                };
                let paths = vec![work.path.clone()];
                let job = if work.closed() {
                    Job::ReopenCarnet(paths)
                } else {
                    Job::CloseCarnet(paths)
                };
                vec![run(model, job)]
            }
            Cmd::Search => {
                let initial = (model.search.as_ref())
                    .map(|search| search.text.clone())
                    .unwrap_or_default();
                let action = Action::ask("Search inside carnets", initial, Submit::Search);
                update(model, action)
            }
            _ => Vec::new(),
        }
    }

    /// Clears the search.
    fn back(&self, model: &mut Model, _list: List) -> bool {
        model.search.take().is_some()
    }

    fn copy_path(&self, model: &Model, _list: List) -> Option<String> {
        model.carnet().map(|work| work.path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Icons;
    use crate::tui::app::Panel;
    use crate::tui::lists::{key_style, tab};
    use crate::tui::update::tests::{model, with_carnets};

    fn titles(model: &Model) -> Vec<String> {
        model
            .carnet_rows()
            .iter()
            .map(|work| work.title())
            .collect()
    }

    #[test]
    fn every_carnet_newest_first_across_workspaces() {
        let mut model = with_carnets(model());
        assert_eq!(
            titles(&model),
            [
                "2026-10-02-ideas",
                "2026-10-01-ABC-1-logs",
                "2026-09-20-old",
                "2026-08-01-done"
            ]
        );
        assert_eq!(model.tabs(Panel::Work), [List::Work, List::Carnets]);
        model.carnets = false;
        assert_eq!(model.tabs(Panel::Work), [List::Work]);
    }

    #[test]
    fn a_row_dims_its_date_and_colours_its_glyph_by_standing() {
        let model = with_carnets(model());
        let palette = Palette::new(catppuccin::PALETTE.mocha, Icons::Unicode);
        let tracker = &model.tracker_config;
        let mut work = model.snapshot.carnets[1].clone();
        if let WorkKind::Carnet { summary, .. } = &mut work.kind {
            *summary = "Login fails".into();
        }
        let shown = |line: &Line| -> Vec<(String, Style)> {
            (line.spans.iter())
                .map(|span| (span.content.to_string(), span.style))
                .collect()
        };
        let (dim, info) = (Style::new().fg(palette.dim), Style::new().fg(palette.info));
        assert_eq!(
            shown(&row(
                &work,
                Vec::new(),
                Standing::Open,
                false,
                tracker,
                &palette
            )),
            [
                ("✎ ".into(), info),
                ("2026-10-01".into(), dim),
                (" Login fails".into(), Style::new()),
                (" ABC-1".into(), group_style(&palette)),
                (" ".into(), Style::new()),
                ("ABC-1".into(), key_style(&palette)),
            ]
        );
        let standing = Standing::of(&work, true);
        let line = row(&work, Vec::new(), standing, true, tracker, &palette);
        assert_eq!(line.spans[0].style, standing.style(&palette));
        assert_eq!(standing, Standing::Elsewhere);
        assert!(
            !line.spans.iter().any(|span| span.content == " ABC-1"),
            "no group under its header"
        );

        let unsummarised = &model.snapshot.carnets[0];
        let line = row(
            unsummarised,
            Vec::new(),
            Standing::Open,
            false,
            tracker,
            &palette,
        );
        let name = (line.spans.iter()).find(|span| span.content == " ideas");
        assert!(
            name.is_some_and(|span| span.style.add_modifier.contains(Modifier::ITALIC)),
            "the folder name stands in"
        );

        let closed = model.snapshot.carnets.last().unwrap();
        let standing = Standing::of(closed, true);
        assert_eq!(standing, Standing::Closed, "closed wins over elsewhere");
        let lead = vec![tab_mark(false, &palette)];
        let line = row(closed, lead, standing, false, tracker, &palette);
        assert_eq!(
            line.spans[0].style,
            tab(false, &palette).1,
            "the tab dot kept"
        );
        assert!(
            line.spans[1..]
                .iter()
                .all(|span| span.style.fg == Some(palette.dim)),
            "the rest dimmed"
        );
    }
}
