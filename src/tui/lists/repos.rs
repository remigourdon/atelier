//! Panel 1's Repos list.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, pair};
use crate::state::Repo;
use crate::tui::app::{Action, Effect, Job, Kind, List, Model, Submit};
use crate::tui::update::{confirm, note, update, workspace_menu};
use crate::tui::view::{Palette, icon};

pub struct Repos;

impl Model {
    pub fn repos(&self) -> Vec<&Repo> {
        self.snapshot
            .repos
            .iter()
            .filter(|repo| self.matches(List::Repos, &[&repo.name(), &repo.path.to_string_lossy()]))
            .collect()
    }

    pub fn repo(&self) -> Option<&Repo> {
        self.repos().get(self.index(List::Repos)).copied()
    }
}

impl ListKind for Repos {
    fn kind(&self) -> Kind {
        Kind::Repos
    }

    fn title<'a>(&self, _model: &'a Model, _list: List) -> &'a str {
        "Repos"
    }

    fn len(&self, model: &Model, _list: List) -> usize {
        model.repos().len()
    }

    fn ids(&self, model: &Model, _list: List) -> Vec<String> {
        (model.repos().into_iter())
            .map(|repo| repo.path.display().to_string())
            .collect()
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, _list: List) -> Vec<Line<'a>> {
        let dim = Style::new().fg(palette.dim);
        model
            .repos()
            .into_iter()
            .map(|repo| {
                let mut spans: Vec<Span> = icon(palette.glyphs.repo, dim).into_iter().collect();
                spans.push(Span::raw(repo.name()));
                spans.push(Span::styled(format!(" → {}", repo.default_workspace), dim));
                Line::from(spans)
            })
            .collect()
    }

    fn detail(
        &self,
        model: &Model,
        _palette: &Palette,
        _list: List,
    ) -> Vec<(String, Line<'static>)> {
        let Some(repo) = model.repo() else {
            return Vec::new();
        };
        let count = model
            .snapshot
            .work
            .iter()
            .filter(|work| work.repo() == Some(&repo.path))
            .count();
        vec![
            pair("Repo", repo.name()),
            pair("Alias", repo.alias.clone().unwrap_or_default()),
            pair("Path", repo.path.display().to_string()),
            pair("Workspace", repo.default_workspace.clone()),
            pair("Worktrees", count.to_string()),
            pair(
                "Forge",
                model
                    .snapshot
                    .forges
                    .get(&repo.path)
                    .map(|forge| forge.url.clone())
                    .unwrap_or_default(),
            ),
        ]
    }

    fn create(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        note(model, "register repos with `atelier add <path>`")
    }

    fn edit(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(repo) = model.repo() else {
            return Vec::new();
        };
        let action = Action::ask(
            format!("Alias of {} (empty clears it)", repo.path.display()),
            repo.alias.clone().unwrap_or_default(),
            Submit::Alias(repo.path.clone()),
        );
        update(model, action)
    }

    fn move_to(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(repo) = model.repo().cloned() else {
            return Vec::new();
        };
        workspace_menu(
            model,
            format!("Default workspace of {}", repo.name()),
            &repo.default_workspace,
            |workspace| Job::SetRepoWorkspace {
                repo: repo.path.clone(),
                workspace,
            },
        )
    }

    fn remove(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(repo) = model.repo().cloned() else {
            return Vec::new();
        };
        confirm(
            model,
            "Forget repo".into(),
            vec![
                format!("Forget {} and close its tabs?", repo.name()),
                "Its worktrees stay on disk.".into(),
            ],
            Job::Forget(repo.path),
        )
    }

    fn copy_path(&self, model: &Model, _list: List) -> Option<String> {
        model.repo().map(|repo| repo.path.display().to_string())
    }

    /// The repo's forge page.
    fn url(&self, model: &Model, _list: List) -> Option<String> {
        (model.snapshot.forges)
            .get(&model.repo()?.path)
            .map(|forge| forge.url.clone())
    }
}
