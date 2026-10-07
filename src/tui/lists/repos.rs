//! Panel 1's Repos list.

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, ListRow, Target, pair, workspace_span};
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
}

/// The selected repo.
fn repo(selected: Option<Target>) -> Option<Repo> {
    match selected? {
        Target::Repo(repo) => Some(repo.into_owned()),
        _ => None,
    }
}

impl ListKind for Repos {
    fn kind(&self) -> Kind {
        Kind::Repos
    }

    fn title<'a>(&self, _model: &'a Model, _list: List) -> &'a str {
        "Repos"
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, _list: List) -> Vec<ListRow<'a>> {
        let dim = Style::new().fg(palette.dim);
        model
            .repos()
            .into_iter()
            .map(|repo| {
                let mut spans: Vec<Span> = icon(palette.glyphs.repo, dim).into_iter().collect();
                spans.push(Span::raw(repo.name()));
                spans.push(Span::styled(format!(" → {}", repo.default_workspace), dim));
                let path = repo.path.display().to_string();
                ListRow {
                    id: path.clone(),
                    line: Line::from(spans),
                    target: Target::Repo(Cow::Borrowed(repo)),
                    path: Some(path),
                    branch: None,
                    // The repo's forge page.
                    url: (model.snapshot.forges.get(&repo.path)).map(|forge| forge.url.clone()),
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
        let Target::Repo(repo) = target else {
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
            pair(
                "Workspace",
                workspace_span(&repo.default_workspace, palette),
            ),
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

    fn create(&self, model: &mut Model, _selected: Option<Target<'static>>) -> Vec<Effect> {
        note(model, "register repos with `atelier add <path>`")
    }

    fn edit(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(repo) = repo(selected) else {
            return Vec::new();
        };
        let action = Action::ask(
            format!("Alias of {} (empty clears it)", repo.path.display()),
            repo.alias.clone().unwrap_or_default(),
            Submit::Alias(repo.path.clone()),
        );
        update(model, action)
    }

    fn move_to(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(repo) = repo(selected) else {
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

    fn remove(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(repo) = repo(selected) else {
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
}
