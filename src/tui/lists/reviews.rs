//! Panel 3's review lists: To review and Mine.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, close_tabs, pair, subtle, tab_mark};
use crate::reviews::{Review, Role};
use crate::state::Repo;
use crate::tui::app::{Cmd, Effect, Feed, Kind, List, Model, Pending, Source, Work};
use crate::tui::update::{join_linked_group, note};
use crate::tui::view::{Palette, icon};
use crate::worktrunk;

pub struct Reviews;

/// The reviews a list holds.
fn role(list: List) -> Option<Role> {
    match list {
        List::ToReview => Some(Role::ToReview),
        List::Mine => Some(Role::Mine),
        _ => None,
    }
}

impl Model {
    /// A list's reviews, narrowed by its filter.
    pub fn reviews(&self, list: List) -> Vec<&Review> {
        self.reviews
            .iter()
            .filter(|review| {
                Some(review.role) == role(list)
                    && self.matches(
                        list,
                        &[
                            &review.title,
                            &review.project,
                            &review.author,
                            &review.branch,
                            &review.provider.reference(review.number),
                        ],
                    )
            })
            .collect()
    }

    /// The selected review, when a review list is active.
    pub fn review(&self) -> Option<&Review> {
        let list = self.active();
        self.reviews(list).get(self.index(list)).copied()
    }

    /// The registered repo whose forge web page is `project_url`.
    pub fn project_repo(&self, project_url: &str) -> Option<&Repo> {
        let path = self.snapshot.forges.iter().find_map(|(path, forge)| {
            worktrunk::same_project(&forge.url, project_url).then_some(path)
        })?;
        self.snapshot.repos.iter().find(|repo| repo.path == *path)
    }

    /// The registered repo's name for a review's project, else the project's path.
    pub fn review_project(&self, review: &Review) -> String {
        self.project_repo(&review.project_url)
            .map_or_else(|| review.project.clone(), Repo::name)
    }

    /// The worktree that has a review's branch checked out.
    pub fn review_work(&self, review: &Review) -> Option<&Work> {
        let repo = self.project_repo(&review.project_url)?;
        self.snapshot.work.iter().find(|work| {
            work.repo() == Some(&repo.path)
                && (work.tree()).and_then(|tree| tree.branch.as_deref())
                    == Some(review.branch.as_str())
        })
    }
}

/// A review's status: a draft's warns.
fn status(review: &Review, palette: &Palette) -> Span<'static> {
    if review.draft {
        Span::styled("draft", Style::new().fg(palette.warn))
    } else {
        Span::raw("open")
    }
}

impl ListKind for Reviews {
    fn kind(&self) -> Kind {
        Kind::Reviews
    }

    fn title<'a>(&self, _model: &'a Model, list: List) -> &'a str {
        if list == List::ToReview {
            "To review"
        } else {
            "Mine"
        }
    }

    fn len(&self, model: &Model, list: List) -> usize {
        model.reviews(list).len()
    }

    /// A review's URL.
    fn ids(&self, model: &Model, list: List) -> Vec<String> {
        (model.reviews(list).iter())
            .map(|review| review.url.clone())
            .collect()
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, list: List) -> Vec<Line<'a>> {
        let dim = Style::new().fg(palette.dim);
        model
            .reviews(list)
            .into_iter()
            .map(|review| {
                let glyphs = &palette.glyphs;
                let marker = match model.review_work(review) {
                    Some(work) => tab_mark(work.tab, palette),
                    None => Span::raw("  "),
                };
                let mut spans = vec![marker];
                spans.extend(icon(glyphs.review, dim));
                spans.push(Span::styled(
                    format!(
                        "{}{} ",
                        model.review_project(review),
                        review.provider.reference(review.number)
                    ),
                    dim,
                ));
                spans.push(Span::raw(review.title.as_str()));
                if review.draft {
                    spans.push(Span::raw(" "));
                    spans.push(status(review, palette));
                }
                if list == List::ToReview {
                    spans.push(Span::styled(format!(" @{}", review.author), dim));
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
        let Some(review) = model.review() else {
            return Vec::new();
        };
        let repo = match model.project_repo(&review.project_url) {
            Some(repo) => repo.name(),
            None => format!("{} (not registered)", review.project),
        };
        vec![
            pair(
                "Review",
                format!(
                    "{}{}",
                    review.project,
                    review.provider.reference(review.number)
                ),
            ),
            pair("Title", review.title.clone()),
            pair("Author", review.author.clone()),
            pair("Branch", format!("{} → {}", review.branch, review.base)),
            pair("Status", status(review, palette)),
            pair("Updated", subtle(review.updated_at.clone(), palette)),
            pair("URL", review.url.clone()),
            pair("Repo", repo),
            pair(
                "Worktree",
                model.review_work(review).map_or(
                    subtle("none: Space checks it out", palette).into(),
                    |work| {
                        Line::from(vec![
                            tab_mark(work.tab, palette),
                            Span::raw(work.path().display().to_string()),
                        ])
                    },
                ),
            ),
        ]
    }

    fn empty(&self, model: &Model, _list: List) -> &'static str {
        if (model.schedule.loading()).any(|source| matches!(source, Source::Feed(Feed::Reviews(_))))
        {
            "loading…"
        } else {
            "nothing here"
        }
    }

    fn activate(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(review) = model.review() else {
            return Vec::new();
        };
        let Some(repo) = model.project_repo(&review.project_url) else {
            let message = format!(
                "{} is not registered: add a clone with `atelier add <path>`",
                review.project
            );
            return note(model, &message);
        };
        let pending = Pending::Checkout {
            repo: repo.path.clone(),
            workspace: repo.default_workspace.clone(),
            review: Box::new(review.clone()),
        };
        join_linked_group(model, pending)
    }

    fn command(&self, model: &mut Model, _list: List, cmd: Cmd) -> Vec<Effect> {
        if cmd != Cmd::Close {
            return Vec::new();
        }
        let paths = model
            .review()
            .and_then(|review| model.review_work(review))
            .filter(|work| work.tab)
            .map(|work| work.path.clone())
            .into_iter()
            .collect();
        close_tabs(model, paths)
    }

    fn branch(&self, model: &Model, _list: List) -> Option<String> {
        model.review().map(|review| review.branch.clone())
    }

    fn url(&self, model: &Model, _list: List) -> Option<String> {
        model.review().map(|review| review.url.clone())
    }
}
