//! Panel 1's Workspaces list.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, pair};
use crate::tui::app::{Action, Effect, Job, Kind, List, Model, Submit};
use crate::tui::update::{confirm, run, update};
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

    fn len(&self, model: &Model, _list: List) -> usize {
        model.workspaces().len()
    }

    fn ids(&self, model: &Model, _list: List) -> Vec<String> {
        model.workspaces().into_iter().cloned().collect()
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, _list: List) -> Vec<Line<'a>> {
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
                Line::from(spans)
            })
            .collect()
    }

    fn detail(&self, model: &Model, _list: List) -> Vec<(String, String)> {
        let Some(name) = model.workspace() else {
            return Vec::new();
        };
        let work: Vec<_> = model
            .snapshot
            .work
            .iter()
            .filter(|work| work.workspace == name)
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
                    "current".into()
                } else {
                    "other".into()
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

    fn activate(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(name) = model.workspace().map(str::to_owned) else {
            return Vec::new();
        };
        if model.snapshot.here.is_some() {
            vec![run(model, Job::SwitchWorkspace(name))]
        } else {
            vec![Effect::Attach(name)]
        }
    }

    fn create(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        update(
            model,
            Action::Ask {
                title: "New workspace".into(),
                initial: String::new(),
                then: Submit::Workspace,
            },
        )
    }

    fn remove(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(name) = model.workspace().map(str::to_owned) else {
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

    fn copy_path(&self, model: &Model, _list: List) -> Option<String> {
        model.workspace().map(Into::into)
    }
}
