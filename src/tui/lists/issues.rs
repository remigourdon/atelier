//! Panel 4's sections, one list of issues each.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, kind, pair, paths};
use crate::issues::{Issue, State};
use crate::state::Repo;
use crate::tui::app::{
    Action, Effect, Feed, Job, Kind, List, MenuEntry, Modal, Model, Source, Submit, Work,
};
use crate::tui::update::{note, run};
use crate::tui::view::{Palette, icon};

pub struct Issues;

impl Model {
    /// A section's issues, narrowed by its filter.
    pub fn issues(&self, list: List) -> Vec<&Issue> {
        let List::Section(index) = list else {
            return Vec::new();
        };
        self.issues
            .iter()
            .filter(|issue| {
                self.tracker_config.section(issue) == Some(index)
                    && self.matches(
                        list,
                        &[
                            &issue.key,
                            &issue.title,
                            &issue.project,
                            &issue.labels.join(" "),
                            &issue.assignees.join(" "),
                        ],
                    )
            })
            .collect()
    }

    /// The selected issue, when a section is active.
    pub fn issue(&self) -> Option<&Issue> {
        let list = self.active();
        self.issues(list).get(self.index(list)).copied()
    }

    /// An issue's linked work: the worktrees in its group and the carnets, closed ones too,
    /// that list its key among their tickets.
    pub fn issue_work(&self, issue: &Issue) -> Vec<&Work> {
        let worktrees =
            (self.snapshot.work.iter()).filter(|work| !work.is_carnet() && work.group == issue.key);
        let carnets =
            (self.snapshot.carnets.iter()).filter(|carnet| carnet.tickets().contains(&issue.key));
        worktrees.chain(carnets).collect()
    }
}

/// Asks which repo an issue's worktree goes in, then for its branch. The menu suggests the
/// repos of its linked work, then its own repo on GitHub, but never picks one: a tracker-only
/// repo holds issues whose work happens elsewhere. It goes to the workspace of the linked work
/// in that repo, else of any linked work, else the repo's default one.
fn ask_start(model: &mut Model, issue: Issue) -> Vec<Effect> {
    let linked = model.issue_work(&issue);
    let workspace = |repo: &Repo| {
        let work = (linked.iter().find(|work| work.repo() == Some(&repo.path))).or(linked.first());
        work.map_or(repo.default_workspace.clone(), |work| {
            work.workspace.clone()
        })
    };
    let own = (issue.project_url.as_deref()).and_then(|url| model.project_repo(url));
    let mut repos: Vec<&Repo> = model.snapshot.repos.iter().collect();
    repos.sort_by_key(|repo| {
        let suggested = (linked.iter()).any(|work| work.repo() == Some(&repo.path));
        let own = own.is_some_and(|own| own.path == repo.path);
        (!suggested, !own)
    });
    let entries: Vec<MenuEntry> = (repos.into_iter().enumerate())
        .map(|(index, repo)| MenuEntry {
            key: (index + 1).to_string(),
            label: repo.name(),
            action: Action::Ask {
                title: format!("New worktree of {} for {}: branch", repo.name(), issue.key),
                initial: issue.branch(),
                then: Submit::Start {
                    repo: repo.path.clone(),
                    workspace: workspace(repo),
                    issue: Box::new(issue.clone()),
                },
            },
        })
        .collect();
    if entries.is_empty() {
        return note(
            model,
            "no repos yet: register one with `atelier add <path>`",
        );
    }
    model.modal = Some(Modal::Menu {
        title: format!("New worktree for {} in", issue.key),
        entries,
        selected: 0,
    });
    Vec::new()
}

impl ListKind for Issues {
    fn kind(&self) -> Kind {
        Kind::Issues
    }

    fn title<'a>(&self, model: &'a Model, list: List) -> &'a str {
        match list {
            List::Section(index) => model.tracker_config.title(index),
            _ => "",
        }
    }

    fn len(&self, model: &Model, list: List) -> usize {
        model.issues(list).len()
    }

    /// An issue's key, which unlike its URL is never empty.
    fn ids(&self, model: &Model, list: List) -> Vec<String> {
        (model.issues(list).iter())
            .map(|issue| issue.key.clone())
            .collect()
    }

    fn rows<'a>(&self, model: &'a Model, palette: &Palette, list: List) -> Vec<Line<'a>> {
        let dim = Style::new().fg(palette.dim);
        model
            .issues(list)
            .into_iter()
            .map(|issue| {
                let glyphs = &palette.glyphs;
                let work = model.issue_work(issue);
                let marker = if work.iter().any(|work| work.tab) {
                    Span::styled(format!("{} ", glyphs.open), Style::new().fg(palette.ok))
                } else if !work.is_empty() {
                    Span::styled(format!("{} ", glyphs.closed), dim)
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![marker];
                spans.extend(icon(glyphs.issue, dim));
                spans.push(Span::styled(format!("{} ", issue.key), dim));
                let state = match issue.state {
                    State::Todo => None,
                    State::InProgress => Some(palette.info),
                    State::Done => Some(palette.ok),
                };
                if let Some(color) = state {
                    let label = format!("{} ", issue.state.label().to_lowercase());
                    spans.push(Span::styled(label, Style::new().fg(color)));
                }
                spans.push(Span::raw(issue.title.as_str()));
                if issue.blocked {
                    spans.push(Span::styled(" blocked", Style::new().fg(palette.error)));
                }
                for label in &issue.labels {
                    spans.push(Span::styled(
                        format!(" {label}"),
                        Style::new().fg(palette.info),
                    ));
                }
                Line::from(spans)
            })
            .collect()
    }

    fn detail(&self, model: &Model, _list: List) -> Vec<(String, String)> {
        let Some(issue) = model.issue() else {
            return Vec::new();
        };
        let mut pairs = vec![
            pair("Issue", issue.key.clone()),
            pair("Title", issue.title.clone()),
            pair("State", issue.state.label().into()),
            pair("Status", issue.status.clone()),
            pair("Blocked", if issue.blocked { "yes" } else { "no" }.into()),
            pair("Labels", issue.labels.join(", ")),
            pair("Assignees", issue.assignees.join(", ")),
            pair("Updated", issue.updated_at.clone()),
            pair("URL", issue.url.clone().unwrap_or_default()),
            pair("Project", issue.project.clone()),
        ];
        for (key, value) in [("Type", &issue.kind), ("Priority", &issue.priority)] {
            pairs.extend(value.clone().map(|value| pair(key, value)));
        }
        let work = model.issue_work(issue);
        if work.is_empty() {
            pairs.push(pair("Worktree", "none: Space or n creates one".into()));
        }
        pairs.extend(work.into_iter().map(|work| {
            let tab = if work.tab { "open" } else { "closed" };
            pair(
                kind(work),
                format!("{} · {} · tab {tab}", work.title(), work.workspace),
            )
        }));
        pairs
    }

    fn empty(&self, model: &Model, _list: List) -> &'static str {
        if (model.schedule.loading()).any(|source| matches!(source, Source::Feed(Feed::Issues(_))))
        {
            "loading…"
        } else if model.tracker_config.scopes().is_empty() {
            "no [tracker] configured"
        } else {
            "nothing here"
        }
    }

    /// Opens the issue's linked work, else asks to start a worktree for it.
    fn activate(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(issue) = model.issue().cloned() else {
            return Vec::new();
        };
        let paths = paths(&model.issue_work(&issue));
        if paths.is_empty() {
            ask_start(model, issue)
        } else {
            vec![run(model, Job::Open(paths))]
        }
    }

    fn create(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        match model.issue().cloned() {
            Some(issue) => ask_start(model, issue),
            None => Vec::new(),
        }
    }

    fn url(&self, model: &Model, _list: List) -> Option<String> {
        model.issue()?.url.clone()
    }
}
