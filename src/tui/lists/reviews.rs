//! Panel 3's review lists: To review and Mine.

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{
    ListKind, ListRow, Target, close_tabs, group_style, key_style, pair, subtle, tab_mark,
    work_line,
};

use crate::reviews::{Review, Role};
use crate::state::Repo;
use crate::tui::app::{Cmd, Effect, Feed, Kind, List, Model, Pending, Source};
use crate::tui::update::{join_linked_group, note};
use crate::tui::view::{Palette, icon};

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

    /// The registered repo's name for a review's project, else the project's path.
    pub fn review_project(&self, review: &Review) -> String {
        (self.linked().project_repo(&review.project_url))
            .map_or_else(|| review.project.clone(), Repo::name)
    }

    /// How a review is listed: its repo's name, else its project, and its number.
    pub fn review_label(&self, review: &Review) -> String {
        format!(
            "{}{}",
            self.review_project(review),
            review.provider.reference(review.number)
        )
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

    /// Each row's identity is its review's URL.
    fn rows<'a>(&self, model: &'a Model, palette: &Palette, list: List) -> Vec<ListRow<'a>> {
        let dim = Style::new().fg(palette.dim);
        let linked = model.linked();
        model
            .reviews(list)
            .into_iter()
            .map(|review| {
                let glyphs = &palette.glyphs;
                let marker = match linked.review_worktree(review) {
                    Some(work) => tab_mark(work.tab, palette),
                    None => Span::raw("  "),
                };
                let mut spans = vec![marker];
                spans.extend(icon(glyphs.review, dim));
                spans.push(Span::styled(
                    format!("{} ", model.review_label(review)),
                    dim,
                ));
                spans.push(Span::raw(review.title.as_str()));
                if review.draft {
                    spans.push(Span::raw(" "));
                    spans.push(status(review, palette));
                }
                if let Some(group) = linked.review_group(review) {
                    spans.push(Span::styled(format!(" {group}"), group_style(palette)));
                }
                if list == List::ToReview {
                    spans.push(Span::styled(format!(" @{}", review.author), dim));
                }
                ListRow {
                    id: review.url.clone(),
                    line: Line::from(spans),
                    target: Target::Review(Cow::Borrowed(review)),
                    path: None,
                    branch: Some(review.branch.clone()),
                    url: Some(review.url.clone()),
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
        let Target::Review(review) = target else {
            return Vec::new();
        };
        let linked = model.linked();
        let repo = match linked.project_repo(&review.project_url) {
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
                "Issue keys",
                Span::styled(
                    review.issue_keys.display(&model.tracker_config, ", "),
                    key_style(palette),
                ),
            ),
            pair(
                "Worktree",
                linked.review_worktree(review).map_or(
                    subtle("none: Space checks it out", palette).into(),
                    |work| work_line(work, palette),
                ),
            ),
        ]
    }

    fn empty(&self, model: &Model) -> &'static str {
        if (model.schedule.loading()).any(|source| matches!(source, Source::Feed(Feed::Reviews(_))))
        {
            "loading…"
        } else {
            "nothing here"
        }
    }

    fn activate(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(review) = selected.and_then(Target::into_review) else {
            return Vec::new();
        };
        let Some(repo) = model.linked().project_repo(&review.project_url) else {
            let message = format!(
                "{} is not registered: add a clone with `atelier add <path>`",
                review.project
            );
            return note(model, &message);
        };
        let pending = Pending::Checkout {
            repo: repo.path.clone(),
            workspace: repo.default_workspace.clone(),
            review: Box::new(review),
        };
        join_linked_group(model, pending)
    }

    fn command(
        &self,
        model: &mut Model,
        selected: Option<Target<'static>>,
        cmd: Cmd,
    ) -> Vec<Effect> {
        if cmd != Cmd::Close {
            return Vec::new();
        }
        let paths = selected
            .and_then(Target::into_review)
            .and_then(|review| model.linked().review_worktree(&review))
            .filter(|work| work.tab)
            .map(|work| work.path.clone())
            .into_iter()
            .collect();
        close_tabs(model, paths)
    }
}
