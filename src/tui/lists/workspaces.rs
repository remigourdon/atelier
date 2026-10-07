//! Panel 1's Workspaces list.

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, ListRow, Target, pair, plan, subtle, work};
use crate::finish::Scope;
use crate::tui::app::{Action, Effect, Job, Kind, List, Model, Submit};
use crate::tui::update::{confirm, switch_workspace, update};
use crate::tui::view::{Palette, icon};

pub struct Workspaces;

impl Model {
    pub fn workspaces(&self) -> Vec<&String> {
        self.snapshot
            .workspaces
            .iter()
            .filter(|name| self.matches(List::Workspaces, &[name]))
            .collect()
    }

    /// The workspace whose work panel 2 shows: the one selected in panel 1.
    pub fn workspace(&self) -> Option<&str> {
        let names = self.workspaces();
        names
            .get(self.index(List::Workspaces))
            .or(names.first())
            .map(|name| name.as_str())
            .or(self.snapshot.here.as_deref())
    }
}

impl ListKind for Workspaces {
    fn kind(&self) -> Kind {
        Kind::Workspaces
    }

    fn title<'a>(&self, _model: &'a Model, _list: List) -> &'a str {
        "Workspaces"
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, _list: List) -> Vec<ListRow<'a>> {
        let dim = Style::new().fg(palette.dim);
        model
            .workspaces()
            .into_iter()
            .map(|name| {
                let open = model
                    .snapshot
                    .work
                    .iter()
                    .filter(|work| work.workspace == *name && work.tab)
                    .count();
                let mut spans: Vec<Span> =
                    icon(palette.glyphs.workspace, dim).into_iter().collect();
                spans.push(Span::raw(name.as_str()));
                if model.snapshot.here.as_ref() == Some(name) {
                    spans.push(Span::styled(" (here)", Style::new().fg(palette.accent)));
                }
                if open > 0 {
                    spans.push(Span::styled(format!(" {open} open"), dim));
                }
                ListRow {
                    id: name.clone(),
                    line: Line::from(spans),
                    target: Target::Workspace(Cow::Borrowed(name)),
                    path: Some(name.clone()),
                    branch: None,
                    url: None,
                }
            })
            .collect()
    }

    fn detail(
        &self,
        model: &Model,
        palette: &Palette,
        target: &Target,
    ) -> Vec<(String, Line<'static>)> {
        let Target::Workspace(name) = target else {
            return Vec::new();
        };
        let name = name.as_ref();
        let work: Vec<_> = model
            .snapshot
            .work
            .iter()
            .filter(|work| work.workspace == name && work::shown(work))
            .collect();
        let carnets = work.iter().filter(|work| work.is_carnet()).count();
        let repos: Vec<String> = model
            .snapshot
            .repos
            .iter()
            .filter(|repo| repo.default_workspace == name)
            .map(|repo| repo.name())
            .collect();
        let mut pairs = vec![
            pair("Workspace", name.to_owned()),
            pair(
                "Session",
                if model.snapshot.here.as_deref() == Some(name) {
                    Span::styled("current", Style::new().fg(palette.accent))
                } else {
                    subtle("other", palette)
                },
            ),
            pair("Worktrees", (work.len() - carnets).to_string()),
        ];
        if model.carnets {
            pairs.push(pair("Carnets", carnets.to_string()));
        }
        pairs.extend([
            pair(
                "Open tabs",
                work.iter().filter(|work| work.tab).count().to_string(),
            ),
            pair("Default for", repos.join(", ")),
        ]);
        pairs
    }

    fn activate(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(name) = selected.and_then(Target::into_workspace) else {
            return Vec::new();
        };
        if model.snapshot.here.is_some() {
            switch_workspace(model, name)
        } else {
            vec![Effect::Attach(name)]
        }
    }

    fn create(&self, model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        update(model, Action::ask("New workspace", "", Submit::Workspace))
    }

    fn remove(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(name) = selected.and_then(Target::into_workspace) else {
            return Vec::new();
        };
        let lines = vec![format!("Remove the workspace {name}?")];
        confirm(
            model,
            "Remove workspace".into(),
            lines,
            Job::RemoveWorkspace(name),
        )
    }

    /// A sweep of the workspace.
    fn finish(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        match selected.and_then(Target::into_workspace) {
            Some(name) => plan(model, Scope::Workspace(name)),
            None => Vec::new(),
        }
    }
}
