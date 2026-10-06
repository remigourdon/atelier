//! Panel 4's sections, one list of issues each.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ListKind, close_tabs, kind, pair, paths, plan, subtle, tab_mark, tab_word, tag_style};
use crate::finish::Scope;
use crate::issues::{Issue, State};
use crate::state::Repo;
use crate::tui::app::{
    Action, Cmd, Effect, Feed, Job, Kind, List, MenuEntry, Modal, Model, Source, Submit, Work,
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

    /// An issue's linked work: the worktrees and the carnets, closed ones too, that link its
    /// key, in any group.
    pub fn issue_work(&self, issue: &Issue) -> Vec<&Work> {
        let worktrees =
            (self.snapshot.work.iter()).filter(|work| !work.is_carnet() && work.links(&issue.key));
        let carnets = (self.snapshot.carnets.iter()).filter(|carnet| carnet.links(&issue.key));
        worktrees.chain(carnets).collect()
    }

    /// An issue's key as shown.
    pub fn issue_label(&self, issue: &Issue) -> String {
        self.tracker_config.display_key(&issue.key)
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
    let label = model.issue_label(&issue);
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
                title: format!("New worktree of {} for {label}: branch", repo.name()),
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
        title: format!("New worktree for {label} in"),
        entries,
        selected: 0,
    });
    Vec::new()
}

/// An issue's state: in progress or done stand out, to do does not.
fn state_style(state: State, palette: &Palette) -> Style {
    match state {
        State::Todo => Style::new(),
        State::InProgress => Style::new().fg(palette.info),
        State::Done => Style::new().fg(palette.ok),
    }
}

fn blocked_style(palette: &Palette) -> Style {
    Style::new().fg(palette.error)
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
                let marker = if work.is_empty() {
                    Span::raw("  ")
                } else {
                    tab_mark(work.iter().any(|work| work.tab), palette)
                };
                let mut spans = vec![marker];
                spans.extend(icon(glyphs.issue, dim));
                spans.push(Span::styled(format!("{} ", model.issue_label(issue)), dim));
                if issue.state != State::Todo {
                    let label = format!("{} ", issue.state.label().to_lowercase());
                    spans.push(Span::styled(label, state_style(issue.state, palette)));
                }
                spans.push(Span::raw(issue.title.as_str()));
                if issue.blocked {
                    spans.push(Span::styled(" blocked", blocked_style(palette)));
                }
                for label in &issue.labels {
                    spans.push(Span::styled(format!(" {label}"), tag_style(palette)));
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
        let Some(issue) = model.issue() else {
            return Vec::new();
        };
        let mut pairs = vec![
            pair("Issue", model.issue_label(issue)),
            pair("Title", issue.title.clone()),
            pair(
                "State",
                Span::styled(issue.state.label(), state_style(issue.state, palette)),
            ),
            pair("Status", issue.status.clone()),
            pair(
                "Blocked",
                if issue.blocked {
                    Span::styled("yes", blocked_style(palette))
                } else {
                    Span::raw("no")
                },
            ),
            pair(
                "Labels",
                Span::styled(issue.labels.join(", "), tag_style(palette)),
            ),
            pair("Assignees", issue.assignees.join(", ")),
            pair("Updated", subtle(issue.updated_at.clone(), palette)),
            pair("URL", issue.url.clone().unwrap_or_default()),
            pair("Project", issue.project.clone()),
        ];
        for (key, value) in [("Type", &issue.kind), ("Priority", &issue.priority)] {
            pairs.extend(value.clone().map(|value| pair(key, value)));
        }
        let work = model.issue_work(issue);
        if work.is_empty() {
            let none = subtle("none: Space or n creates one", palette);
            pairs.push(pair("Worktree", none));
        }
        pairs.extend(work.into_iter().map(|work| {
            let line = vec![
                tab_mark(work.tab, palette),
                Span::raw(format!("{} · {} · tab ", work.title(), work.workspace)),
                tab_word(work.tab, palette),
            ];
            pair(kind(work), line)
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

    fn command(&self, model: &mut Model, _list: List, cmd: Cmd) -> Vec<Effect> {
        if cmd != Cmd::Close {
            return Vec::new();
        }
        let paths = model
            .issue()
            .map(|issue| {
                model
                    .issue_work(issue)
                    .into_iter()
                    .filter(|work| work.tab)
                    .map(|work| work.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        close_tabs(model, paths)
    }

    fn create(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        match model.issue().cloned() {
            Some(issue) => ask_start(model, issue),
            None => Vec::new(),
        }
    }

    /// The whole groups of the issue's linked work, and its linked items in no group alone.
    fn finish(&self, model: &mut Model, _list: List) -> Vec<Effect> {
        let Some(issue) = model.issue() else {
            return Vec::new();
        };
        let scope = Scope::Issue {
            key: issue.key.clone(),
            label: model.issue_label(issue),
            state: issue.state.label().to_lowercase(),
        };
        plan(model, scope)
    }

    fn url(&self, model: &Model, _list: List) -> Option<String> {
        model.issue()?.url.clone()
    }
}
