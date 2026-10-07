//! Panel 4's sections, one list of issues each.

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{
    ListKind, ListRow, Target, close_tabs, kind, pair, plan, subtle, tab_mark, tag_style, work_line,
};
use crate::finish::{Line as PlanLine, Plan, Scope};
use crate::issues::{Issue, State};
use crate::links::IssueKeys;
use crate::state::Repo;
use crate::tui::app::{
    Action, Cmd, Effect, Feed, IssueStep, Kind, List, MenuEntry, Modal, Model, Pending, Source,
    Submit,
};
use crate::tui::update::note;
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
                            issue.key.as_str(),
                            &issue.title,
                            &issue.project,
                            &issue.labels.join(" "),
                            &issue.assignees.join(" "),
                        ],
                    )
            })
            .collect()
    }

    /// An issue's key as shown.
    pub fn issue_label(&self, issue: &Issue) -> String {
        issue.key.display(&self.tracker_config)
    }
}

/// Asks which repo an issue's worktree goes in, then for its branch. The menu suggests the
/// repos of its linked work, then its own repo on GitHub, but never picks one: a tracker-only
/// repo holds issues whose work happens elsewhere. It goes to the workspace of the linked work
/// in that repo, else of any linked work, else the repo's default one.
fn ask_start(model: &mut Model, issue: Issue) -> Vec<Effect> {
    let view = model.linked();
    let linked = view.of_issue(&issue.key);
    let workspace = |repo: &Repo| {
        let work = (linked.iter().find(|work| work.repo() == Some(&repo.path))).or(linked.first());
        work.map_or(repo.default_workspace.clone(), |work| {
            work.workspace.clone()
        })
    };
    let own = (issue.project_url.as_deref()).and_then(|url| view.project_repo(url));
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
            action: Action::ask(
                format!("New worktree of {} for {label}: branch", repo.name()),
                issue.branch(),
                Submit::Start {
                    repo: repo.path.clone(),
                    workspace: workspace(repo),
                    issue: Box::new(issue.clone()),
                },
            ),
        })
        .collect();
    if entries.is_empty() {
        return note(
            model,
            "no repos yet: register one with `atelier add <path>`",
        );
    }
    model.modal = Some(Modal::menu(format!("New worktree for {label} in"), entries));
    Vec::new()
}

/// `Space`'s plan on an issue: open each linked item, checked, then check out each open review
/// linking it that no worktree has checked out yet, unchecked. A review of an unregistered
/// project can't be checked out, and says why.
fn issue_plan(model: &Model, issue: &Issue) -> Plan<IssueStep> {
    let linked = model.linked();
    let open = (linked.of_issue(&issue.key).into_iter()).map(|work| PlanLine::Step {
        step: IssueStep::Open(work.path.clone()),
        label: format!("open {}", work.title()),
        note: String::new(),
        checked: true,
    });
    let reviews = (linked
        .reviews_linking(&IssueKeys::from_iter([issue.key.clone()]))
        .into_iter())
    .filter(|review| linked.review_worktree(review).is_none())
    .map(|review| {
        let label = format!(
            "check out {} (@{})",
            model.review_label(review),
            review.author
        );
        match linked.project_repo(&review.project_url) {
            Some(repo) => PlanLine::Step {
                step: IssueStep::Checkout(Pending::Checkout {
                    repo: repo.path.clone(),
                    workspace: repo.default_workspace.clone(),
                    review: Box::new(review.clone()),
                }),
                label,
                note: String::new(),
                checked: false,
            },
            None => PlanLine::Info {
                label,
                note: "not registered".into(),
            },
        }
    });
    Plan {
        title: format!("Work on {}", model.issue_label(issue)),
        lines: open.chain(reviews).collect(),
    }
}

/// An issue's state: in progress or done stand out, to do does not.
fn state_style(state: State, palette: &Palette) -> Style {
    match state {
        State::Todo => Style::new(),
        State::InProgress => Style::new().fg(palette.info),
        State::Done => Style::new().fg(palette.ok),
    }
}

/// The mark of an issue an open review links, apart from its linked work's.
pub(crate) fn review_style(palette: &Palette) -> Style {
    Style::new().fg(palette.info)
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

    /// Each row's identity is its issue's key, which unlike its URL is never empty.
    fn rows<'a>(&self, model: &'a Model, palette: &Palette, list: List) -> Vec<ListRow<'a>> {
        let dim = Style::new().fg(palette.dim);
        let linked = model.linked();
        model
            .issues(list)
            .into_iter()
            .map(|issue| {
                let glyphs = &palette.glyphs;
                let work = linked.of_issue(&issue.key);
                // Linked work is marked as any tab is: open or not.
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
                ListRow {
                    id: issue.key.to_string(),
                    line: Line::from(spans),
                    target: Target::Issue(Cow::Borrowed(issue)),
                    path: None,
                    branch: None,
                    url: issue.url.clone(),
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
        let Target::Issue(issue) = target else {
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
        let linked = model.linked();
        let work = linked.of_issue(&issue.key);
        if work.is_empty() {
            let none = subtle("none: Space or n creates one", palette);
            pairs.push(pair("Worktree", none));
        }
        pairs.extend((work.into_iter()).map(|work| pair(kind(work), work_line(work, palette))));
        pairs.extend(
            linked
                .reviews_linking(&IssueKeys::from_iter([issue.key.clone()]))
                .into_iter()
                .map(|review| {
                    let checked_out = match linked.review_worktree(review) {
                        Some(_) => Span::styled("checked out", Style::new().fg(palette.ok)),
                        None => subtle("not checked out", palette),
                    };
                    let line = vec![
                        Span::styled(
                            format!("{} ", palette.glyphs.reviewed),
                            review_style(palette),
                        ),
                        Span::raw(format!(
                            "{} · @{} · ",
                            model.review_label(review),
                            review.author
                        )),
                        checked_out,
                    ];
                    pair("Review", line)
                }),
        );
        pairs
    }

    fn empty(&self, model: &Model) -> &'static str {
        if (model.schedule.loading()).any(|source| matches!(source, Source::Feed(Feed::Issues(_))))
        {
            "loading…"
        } else if model.tracker_config.scopes().is_empty() {
            "no [tracker] configured"
        } else {
            "nothing here"
        }
    }

    /// A plan to open the issue's linked work and check out its open reviews, else asks to
    /// start a worktree for it.
    fn activate(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(issue) = issue(selected) else {
            return Vec::new();
        };
        let plan = issue_plan(model, &issue);
        if plan.checkable().is_empty() {
            return ask_start(model, issue);
        }
        model.modal = Some(Modal::IssuePlan { plan, selected: 0 });
        Vec::new()
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
        let paths = issue(selected)
            .map(|issue| {
                model
                    .linked()
                    .of_issue(&issue.key)
                    .into_iter()
                    .filter(|work| work.tab)
                    .map(|work| work.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        close_tabs(model, paths)
    }

    fn create(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        match issue(selected) {
            Some(issue) => ask_start(model, issue),
            None => Vec::new(),
        }
    }

    /// The issue's linked work alone, never the rest of its groups.
    fn finish(&self, model: &mut Model, selected: Option<Target<'static>>) -> Vec<Effect> {
        let Some(issue) = issue(selected) else {
            return Vec::new();
        };
        let scope = Scope::Issue {
            key: issue.key.clone(),
            label: model.issue_label(&issue),
            state: issue.state.label().to_lowercase(),
        };
        plan(model, scope)
    }
}

/// The selected issue.
fn issue(selected: Option<Target>) -> Option<Issue> {
    match selected? {
        Target::Issue(issue) => Some(issue.into_owned()),
        _ => None,
    }
}
