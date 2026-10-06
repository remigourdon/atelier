//! Panel 2's Carnets list: every carnet in every workspace, closed ones included.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{
    ListKind, close_tabs, edit_links, group_span, group_style, issue_keys, pair, tab_detail,
    tab_mark,
};
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
                let summary = match &work.kind {
                    WorkKind::Carnet { summary, .. } => summary.as_str(),
                    WorkKind::Worktree { .. } => "",
                };
                let (group, keys) = (group_text(work.group()), work.links.issue_keys.join(" "));
                self.matches(List::Carnets, &[&work.title(), group, &keys, summary])
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

/// A closed carnet's mark.
fn closed_mark(palette: &Palette) -> Span<'static> {
    Span::styled("closed", Style::new().fg(palette.warn))
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
        pair("Carnet", work.title()),
        pair("Path", work.path.display().to_string()),
        pair("Workspace", work.workspace.clone()),
        pair("Group", group_span(work.group(), palette)),
        pair("Issue keys", issue_keys(work, tracker, ", ", palette)),
        pair("Summary", summary),
        pair("Tab", tab_detail(work.tab, palette)),
    ];
    if closed {
        pairs.push(pair("Closed", closed_mark(palette)));
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
        let dim = Style::new().fg(palette.dim);
        let glyphs = &palette.glyphs;
        (model.carnet_rows().into_iter())
            .map(|work| {
                let mut spans = vec![tab_mark(work.tab, palette)];
                spans.extend(icon(glyphs.carnet, dim));
                spans.push(Span::raw(work.title()));
                if let Some(group) = work.group() {
                    spans.push(Span::styled(format!(" {group}"), group_style(palette)));
                }
                if !work.links.issue_keys.is_empty() {
                    let keys = issue_keys(work, &model.tracker_config, ",", palette);
                    spans.push(Span::raw(" "));
                    spans.push(keys);
                }
                if work.closed() {
                    spans.push(Span::raw(" "));
                    spans.push(closed_mark(palette));
                }
                if let WorkKind::Carnet { summary, .. } = &work.kind
                    && !summary.is_empty()
                {
                    spans.push(Span::styled(format!(" · {summary}"), dim));
                }
                Line::from(spans)
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
                let action = Action::Ask {
                    title: "Search inside carnets".into(),
                    initial,
                    then: Submit::Search,
                    completions: Vec::new(),
                };
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
    use crate::tui::app::Panel;
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
}
