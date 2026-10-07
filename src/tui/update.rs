//! `update(model, action) -> effects`: every state change, with no I/O.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use super::app::{
    Action, Binding, Cmd, Completion, Draft, DraftStep, Effect, Feed, Focus, IssueStep, Job,
    KEYMAP, List, MenuEntry, MenuPage, Modal, Model, On, Panel, Pending, Popup, PopupCmd, Row,
    Rows, Screen, Search, Snapshot, Source, Submit, Work, WorkKind, lookup, popup_lookup,
};
use super::lists;
use super::view::{Glyphs, Legend, areas, main_len, offset};
use super::widgets;
use crate::carnet::slug;
use crate::finish::Plan;
use crate::links::{Group, IssueKeys, Links, group_text};
use crate::process::Logged;
use crate::reviews::Provider;
use crate::worktrunk;

pub fn update(model: &mut Model, action: Action) -> Vec<Effect> {
    match action {
        Action::Key(key) => {
            model.schedule.input();
            key_press(model, key)
        }
        Action::Mouse(mouse) => {
            model.schedule.input();
            mouse_event(model, mouse)
        }
        Action::Resize(width, height) => {
            model.size = (width, height);
            Vec::new()
        }
        Action::Tick => {
            model.frame = model.frame.wrapping_add(1);
            let jobs = model.schedule.tick();
            start(model, jobs)
        }
        Action::Cmd(cmd) => command(model, cmd),
        Action::Run(job) => vec![run(model, job)],
        Action::Ask {
            title,
            initial,
            then,
            groups,
        } => {
            model.modal = Some(Modal::Prompt {
                title,
                input: Input::new(initial),
                then,
                completion: Completion::new(groups.iter().map(Group::to_string).collect()),
            });
            Vec::new()
        }
        Action::Copy(text) => {
            model.push_log([Logged {
                command: format!("copy {text}"),
                error: None,
            }]);
            vec![Effect::Copy(text)]
        }
        Action::Logged(log) => {
            model.push_log(log);
            Vec::new()
        }
        Action::Loaded {
            snapshot,
            full,
            log,
        } => {
            let mut effects = done(model, Source::Wt);
            model.push_log(log);
            match snapshot {
                Ok(snapshot) => {
                    let keep = Keep::of(model);
                    let old = std::mem::replace(&mut model.snapshot, snapshot);
                    if !full {
                        keep_ci(&mut model.snapshot, &old);
                    }
                    model.loaded = true;
                    keep.restore(model);
                    let relist = drop_stale(model, &old, full);
                    effects.extend(commits(model));
                    effects.extend(relist.map(|path| run(model, Job::Commits(path))));
                    effects.extend(fetch(model));
                }
                Err(error) => model.push_log([Logged {
                    command: "refresh".into(),
                    error: Some(error),
                }]),
            }
            effects
        }
        Action::Commits(path, lines) => {
            model.commits.insert(path, lines);
            done(model, Source::Git)
        }
        Action::Readme(readme) => {
            // A read for a carnet since left is dropped, so it does not replace the selected one's.
            if selected(model).is_some_and(|work| work.path == readme.path) {
                model.readme = Some(readme);
            }
            done(model, Source::Git)
        }
        Action::Searched { text, hits, log } => {
            let mut effects = done(model, Source::Run);
            model.push_log(log);
            if let Some(hits) = hits {
                let keep = Keep::of(model);
                model.search = Some(Search { text, hits });
                keep.restore(model);
                model.scroll = (0, 0);
                effects.extend(commits(model));
            }
            effects
        }
        Action::Planned { plan, log } => {
            let mut effects = done(model, Source::Run);
            model.push_log(log);
            match plan {
                Ok(plan) => {
                    let selected = plan.checkable().first().copied().unwrap_or(0);
                    model.modal = Some(Modal::Finish { plan, selected });
                }
                Err(error) => model.push_log([Logged {
                    command: "finish plan".into(),
                    error: Some(error),
                }]),
            }
            // Its fresh listing may show the marks anew.
            let jobs = model.schedule.changed();
            effects.extend(start(model, jobs));
            effects
        }
        Action::Linked {
            pending,
            group,
            log,
        } => {
            let mut effects = done(model, Source::Run);
            model.push_log(log);
            match group {
                Ok(Some(group)) => effects.push(run(model, pending.job(Some(group)))),
                Ok(None) => effects.extend(ask_group(model, pending)),
                // Not knowing the linked group is no reason to drop the worktree.
                Err(error) => {
                    model.push_log([Logged {
                        command: "linked group".into(),
                        error: Some(error),
                    }]);
                    effects.extend(ask_group(model, pending));
                }
            }
            effects
        }
        Action::Fetched { feed, rows, log } => {
            let effects = done(model, Source::Feed(feed));
            model.push_log(log);
            match rows {
                Ok(rows) => {
                    let keep = Keep::of(model);
                    match rows {
                        Rows::Reviews(reviews) => {
                            (model.reviews).retain(|review| Feed::Reviews(review.provider) != feed);
                            model.reviews.extend(reviews);
                            (model.reviews).sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                        }
                        Rows::Issues(issues) => {
                            (model.issues).retain(|issue| Feed::Issues(issue.tracker) != feed);
                            model.issues.extend(issues);
                            model.issues.sort_by_key(|issue| issue.tracker);
                        }
                    }
                    keep.restore(model);
                }
                Err(error) => model.push_log([Logged {
                    command: feed.what(),
                    error: Some(error),
                }]),
            }
            effects
        }
        Action::Finished { job, log, error } => {
            let mut effects = done(model, job.source());
            if let Job::Pull(paths) = &job {
                model.schedule.pulled(paths);
            }
            let logged_error = log.iter().rev().find_map(|entry| entry.error.clone());
            model.push_log(log);
            if let Some(error) = error
                && logged_error.as_ref() != Some(&error)
            {
                model.push_log([Logged {
                    command: "atelier".into(),
                    error: Some(error),
                }]);
            }
            if job.changes_items() {
                let jobs = model.schedule.changed();
                effects.extend(start(model, jobs));
            }
            effects
        }
    }
}

/// Starts a job, showing its loading indicator.
pub(super) fn run(model: &mut Model, job: Job) -> Effect {
    model.schedule.started(&job);
    Effect::Run(job)
}

/// Switches to another workspace's session, leaving this TUI as it should be found on
/// switching back: on its own session, listed first, with its work focused.
pub(super) fn switch_workspace(model: &mut Model, name: String) -> Vec<Effect> {
    let mut effects = vec![run(model, Job::SwitchWorkspace(name))];
    model.filters.remove(&List::Workspaces);
    model.selected.insert(List::Workspaces, 0);
    effects.extend(focus_panel(model, Panel::Work));
    effects
}

fn start(model: &mut Model, jobs: Vec<Job>) -> Vec<Effect> {
    jobs.into_iter().map(|job| run(model, job)).collect()
}

/// A job of `source` finished: starts the refresh it held back.
fn done(model: &mut Model, source: Source) -> Vec<Effect> {
    let jobs = model.schedule.finished(source);
    start(model, jobs)
}

/// Lists the due feeds: configured providers' reviews on the hosts of registered repos, and each
/// tracker's issues in its configured scopes. A provider with no host lists none.
fn fetch(model: &mut Model) -> Vec<Effect> {
    let mut keys = Vec::new();
    for provider in Provider::ALL {
        let mut hosts: Vec<String> = (model.snapshot.forges.values())
            .filter(|_| model.review_config.providers.contains(&provider))
            .filter(|forge| Provider::from_name(&forge.provider) == Some(provider))
            .filter_map(|forge| worktrunk::host(&forge.url).map(Into::into))
            .collect();
        hosts.sort();
        hosts.dedup();
        if hosts.is_empty() {
            model.reviews.retain(|review| review.provider != provider);
        }
        keys.push((Feed::Reviews(provider), hosts));
    }
    let scopes = model.tracker_config.scopes().into_iter();
    keys.extend(scopes.map(|(tracker, scopes)| (Feed::Issues(tracker), scopes)));
    let jobs = model.schedule.loaded(keys);
    start(model, jobs)
}

/// Each list's selection, kept by its row's identity across a refresh.
struct Keep(Vec<(List, String)>);

impl Keep {
    fn of(model: &Model) -> Self {
        let selected = (model.lists().into_iter())
            .filter_map(|list| {
                let id = lists::of(list)
                    .ids(model, list)
                    .into_iter()
                    .nth(model.index(list))?;
                Some((list, id))
            })
            .collect();
        Self(selected)
    }

    /// Selects each kept row where it is now. Lists restore in panel order, so Work's rows are
    /// those of the workspace already restored.
    fn restore(self, model: &mut Model) {
        for (list, id) in self.0 {
            let ids = lists::of(list).ids(model, list);
            if let Some(index) = ids.iter().position(|other| *other == id) {
                model.selected.insert(list, index);
            }
        }
        clamp_all(model);
    }
}

fn clamp_all(model: &mut Model) {
    for list in model.lists() {
        select(model, list, model.index(list));
    }
}

fn select(model: &mut Model, list: List, index: usize) {
    let index = index.min(model.len(list).saturating_sub(1));
    model.selected.insert(list, index);
}

/// Moves a list's selection, resetting what depends on it.
fn select_moved(model: &mut Model, list: List, index: usize) -> Vec<Effect> {
    let before = model.index(list);
    select(model, list, index);
    if model.index(list) == before {
        return Vec::new();
    }
    model.scroll = (0, 0);
    if list == List::Workspaces {
        model.selected.insert(List::Work, 0);
    }
    commits(model)
}

/// Drops the commits a refresh made stale: every one on a full refresh, else those of items
/// no longer listed, of worktrees whose head moved, and of carnets, whose head the snapshot
/// does not know. The selected carnet's stay on screen until listed again: returns its path.
fn drop_stale(model: &mut Model, old: &Snapshot, full: bool) -> Option<PathBuf> {
    if full {
        model.commits.clear();
        return None;
    }
    let heads: HashMap<&Path, &str> = (old.work.iter())
        .filter_map(|work| Some((work.path.as_path(), work.tree()?.short_sha.as_str())))
        .collect();
    let unmoved = |work: &Work| {
        (work.tree()).is_some_and(|tree| {
            heads.get(work.path.as_path()).copied() == Some(tree.short_sha.as_str())
        })
    };
    let mut kept: HashSet<PathBuf> = (model.snapshot.work.iter())
        .filter(|work| unmoved(work))
        .map(|work| work.path.clone())
        .collect();
    let relist = (selected(model))
        .filter(|work| work.is_carnet() && model.commits.contains_key(&work.path))
        .map(|work| work.path.clone());
    kept.extend(relist.clone());
    model.commits.retain(|path, _| kept.contains(path));
    relist
}

/// A fast refresh lists no CI: each worktree keeps what the last full refresh found.
fn keep_ci(snapshot: &mut Snapshot, old: &Snapshot) {
    let found: HashMap<&Path, &worktrunk::Ci> = (old.work.iter())
        .filter_map(|work| Some((work.path.as_path(), work.tree()?.ci.as_ref()?)))
        .collect();
    for work in &mut snapshot.work {
        if let Some(&ci) = found.get(work.path.as_path())
            && let WorkKind::Worktree { tree, .. } = &mut work.kind
        {
            tree.ci = Some(ci.clone());
        }
    }
}

/// The active list's selected item, else the Work list's.
fn selected(model: &Model) -> Option<&Work> {
    let list = model.active();
    (lists::of(list).item(model, list)).or_else(|| lists::of(List::Work).item(model, List::Work))
}

/// Fetches the selected item's recent commits unless they are loaded, and a carnet's README
/// unless the one loaded is its current one.
fn commits(model: &mut Model) -> Vec<Effect> {
    let Some(work) = selected(model) else {
        return Vec::new();
    };
    let path = work.path.clone();
    let stale_readme = match &work.kind {
        WorkKind::Carnet { readme: stamp, .. } => (model.readme.as_ref())
            .is_none_or(|readme| readme.path != path || readme.stamp != *stamp),
        WorkKind::Worktree { .. } => false,
    };
    let mut effects = Vec::new();
    if stale_readme {
        effects.push(run(model, Job::Readme(path.clone())));
    }
    if !model.commits.contains_key(&path) {
        effects.push(run(model, Job::Commits(path)));
    }
    effects
}

fn key_press(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return command(model, Cmd::Quit);
    }
    if let Some(modal) = model.modal.take() {
        let mut effects = modal_key(model, modal, key);
        if model.modal.is_none()
            && let Some(pending) = model.waiting.pop_front()
        {
            effects.extend(ask_group(model, pending));
        }
        return effects;
    }
    if let Some(list) = model.filtering {
        return filter_key(model, list, key);
    }
    match lookup(&key) {
        Some(cmd) => command(model, cmd),
        None => Vec::new(),
    }
}

fn filter_key(model: &mut Model, list: List, key: KeyEvent) -> Vec<Effect> {
    match popup_lookup(Popup::Filter, &key) {
        Some(PopupCmd::Accept) => model.filtering = None,
        Some(PopupCmd::Cancel) => {
            model.filtering = None;
            model.filters.remove(&list);
        }
        _ => {
            model
                .filters
                .entry(list)
                .or_default()
                .handle_event(&Event::Key(key));
        }
    }
    select_moved(model, list, 0);
    commits(model)
}

fn modal_key(model: &mut Model, modal: Modal, key: KeyEvent) -> Vec<Effect> {
    match modal {
        Modal::Prompt {
            title,
            mut input,
            then,
            mut completion,
        } => {
            match popup_lookup(Popup::Prompt, &key) {
                Some(PopupCmd::Accept) => return submit(model, then, input.value().trim()),
                Some(PopupCmd::Cancel) => return Vec::new(),
                Some(PopupCmd::Complete) => {
                    if let Some(text) = completion.next(input.value()) {
                        input = Input::new(text);
                    }
                }
                _ => {
                    completion.cycle = None;
                    input.handle_event(&Event::Key(key));
                }
            }
            model.modal = Some(Modal::Prompt {
                title,
                input,
                then,
                completion,
            });
            Vec::new()
        }
        Modal::Confirm { title, lines, job } => match popup_lookup(Popup::Confirm, &key) {
            Some(PopupCmd::Accept) => vec![run(model, job)],
            Some(PopupCmd::Cancel) => Vec::new(),
            _ => {
                model.modal = Some(Modal::Confirm { title, lines, job });
                Vec::new()
            }
        },
        Modal::Menu {
            title,
            entries,
            mut selected,
            legend,
            mut page,
            mut scroll,
        } => {
            let last = entries.len().saturating_sub(1);
            let lines = Legend::lines(&legend);
            let rows = widgets::menu_rows(&entries, &legend, screen(model)) as usize;
            let most = lines.saturating_sub(rows);
            match (page, popup_lookup(Popup::Menu, &key)) {
                (_, Some(PopupCmd::Cancel)) => return Vec::new(),
                (_, Some(PopupCmd::Page)) if !legend.is_empty() => {
                    page = match page {
                        MenuPage::Actions => MenuPage::Legend,
                        MenuPage::Legend => MenuPage::Actions,
                    };
                }
                (MenuPage::Legend, Some(PopupCmd::Down)) => scroll = (scroll + 1).min(most),
                (MenuPage::Legend, Some(PopupCmd::Up)) => scroll = scroll.saturating_sub(1),
                (MenuPage::Legend, Some(PopupCmd::Top)) => scroll = 0,
                (MenuPage::Legend, Some(PopupCmd::Bottom)) => scroll = most,
                (MenuPage::Legend, _) => {}
                (MenuPage::Actions, Some(PopupCmd::Accept)) => {
                    let action = entries[selected].action.clone();
                    return update(model, action);
                }
                (MenuPage::Actions, Some(PopupCmd::Down)) => selected = (selected + 1).min(last),
                (MenuPage::Actions, Some(PopupCmd::Up)) => selected = selected.saturating_sub(1),
                (MenuPage::Actions, Some(PopupCmd::Top)) => selected = 0,
                (MenuPage::Actions, Some(PopupCmd::Bottom)) => selected = last,
                (
                    MenuPage::Actions,
                    Some(PopupCmd::Toggle | PopupCmd::Complete | PopupCmd::Page) | None,
                ) => {
                    let shortcut = entries
                        .iter()
                        .find(|entry| match key.code {
                            KeyCode::Char(c) => entry.key.chars().eq([c]),
                            _ => false,
                        })
                        .map(|entry| entry.action.clone());
                    if let Some(action) = shortcut {
                        return update(model, action);
                    }
                }
            }
            model.modal = Some(Modal::Menu {
                title,
                entries,
                selected,
                legend,
                page,
                scroll,
            });
            Vec::new()
        }
        Modal::Finish { mut plan, selected } => match plan_key(&mut plan, selected, &key) {
            PlanKey::Run(steps) if steps.is_empty() => Vec::new(),
            PlanKey::Run(steps) => vec![run(model, Job::Finish(steps))],
            PlanKey::Cancel => Vec::new(),
            PlanKey::Keep(selected) => {
                model.modal = Some(Modal::Finish { plan, selected });
                Vec::new()
            }
        },
        Modal::IssuePlan { mut plan, selected } => match plan_key(&mut plan, selected, &key) {
            PlanKey::Run(steps) => run_issue_plan(model, steps),
            PlanKey::Cancel => Vec::new(),
            PlanKey::Keep(selected) => {
                model.modal = Some(Modal::IssuePlan { plan, selected });
                Vec::new()
            }
        },
    }
}

/// What a key does in a plan's popup.
enum PlanKey<S> {
    /// Runs the checked steps.
    Run(Vec<S>),
    /// Closes the popup, running nothing.
    Cancel,
    /// Keeps the popup open with this line selected.
    Keep(usize),
}

/// Moves through a plan's lines and toggles them, as a finish plan's keys do.
fn plan_key<S: Clone>(plan: &mut Plan<S>, selected: usize, key: &KeyEvent) -> PlanKey<S> {
    let last = plan.lines.len().saturating_sub(1);
    PlanKey::Keep(match popup_lookup(Popup::Finish, key) {
        Some(PopupCmd::Accept) => return PlanKey::Run(plan.checked()),
        Some(PopupCmd::Cancel) => return PlanKey::Cancel,
        Some(PopupCmd::Toggle) => {
            plan.toggle(selected);
            selected
        }
        Some(PopupCmd::Down) => (selected + 1).min(last),
        Some(PopupCmd::Up) => selected.saturating_sub(1),
        _ => selected,
    })
}

/// Opens the checked items' tabs, then checks out the checked reviews, each joining its linked
/// group or asking for one.
fn run_issue_plan(model: &mut Model, steps: Vec<IssueStep>) -> Vec<Effect> {
    let mut paths = Vec::new();
    let mut checkouts = Vec::new();
    for step in steps {
        match step {
            IssueStep::Open(path) => paths.push(path),
            IssueStep::Checkout(pending) => checkouts.push(pending),
        }
    }
    let mut effects = Vec::new();
    if !paths.is_empty() {
        effects.push(run(model, Job::Open(paths)));
    }
    for pending in checkouts {
        effects.extend(join_linked_group(model, pending));
    }
    effects
}

fn submit(model: &mut Model, then: Submit, value: &str) -> Vec<Effect> {
    let job = match then {
        _ if value.is_empty()
            && matches!(
                then,
                Submit::Branch { .. } | Submit::Start { .. } | Submit::Workspace
            ) =>
        {
            return Vec::new();
        }
        Submit::Branch {
            repo,
            workspace,
            group,
        } => Job::Create {
            repo,
            branch: value.into(),
            workspace,
            group,
        },
        Submit::Start {
            repo,
            workspace,
            issue,
        } => {
            let pending = Pending::Start {
                repo,
                branch: value.into(),
                workspace,
                issue,
            };
            return join_linked_group(model, pending);
        }
        Submit::Carnet { draft, step } => return draft_carnet(model, draft, step, value),
        Submit::Summary(path) => Job::SetSummary {
            path,
            summary: value.into(),
        },
        Submit::Group(paths) => Job::Regroup {
            paths,
            group: Group::parse(value),
        },
        Submit::IssueKeys(path) => Job::SetIssueKeys {
            path,
            issue_keys: IssueKeys::resolve(value.split(','), &model.tracker_config),
        },
        Submit::Join(pending) => pending.job(Group::parse(value)),
        Submit::Alias(repo) => Job::SetAlias {
            repo,
            alias: value.into(),
        },
        Submit::Workspace => Job::AddWorkspace(value.into()),
        Submit::Search if value.is_empty() => {
            model.search = None;
            return Vec::new();
        }
        Submit::Search => Job::SearchCarnets(value.into()),
    };
    vec![run(model, job)]
}

/// Makes a pending worktree in the one group linked to its issue keys, looked up first, else in
/// the group asked for. A worktree already there keeps its group, so nothing is asked.
pub(super) fn join_linked_group(model: &mut Model, pending: Pending) -> Vec<Effect> {
    let job = if pending.exists(model) {
        pending.job(None)
    } else {
        Job::LinkedGroup(pending)
    };
    vec![run(model, job)]
}

/// Asks for the group of a pending worktree whose linked work is not in exactly one group,
/// once no other popup is open.
fn ask_group(model: &mut Model, pending: Pending) -> Vec<Effect> {
    if model.modal.is_some() {
        model.waiting.push_back(pending);
        return Vec::new();
    }
    let action = Action::Ask {
        title: format!(
            "Group of the worktree for {} (empty: none)",
            pending.label(model)
        ),
        initial: String::new(),
        then: Submit::Join(pending),
        groups: model.groups(),
    };
    update(model, action)
}

pub(super) fn note(model: &mut Model, message: &str) -> Vec<Effect> {
    model.push_log([Logged {
        command: message.into(),
        error: None,
    }]);
    Vec::new()
}

/// A page of the active list.
fn page(model: &Model) -> usize {
    let areas = areas(model, screen(model));
    areas
        .panels
        .iter()
        .find(|(panel, _)| *panel == model.panel)
        .map_or(1, |(_, rect)| rect.height.saturating_sub(2).max(1) as usize)
}

fn main_height(model: &Model) -> u16 {
    let areas = areas(model, screen(model));
    areas
        .main
        .map_or(1, |rect| rect.height.saturating_sub(2).max(1))
}

fn focus_panel(model: &mut Model, panel: Panel) -> Vec<Effect> {
    model.panel = panel;
    model.focus = Focus::Panel(panel);
    model.scroll = (0, 0);
    commits(model)
}

/// Scrolls the main view, stopping when its last line reaches the bottom.
fn scroll(model: &mut Model, down: i32, right: i32) {
    let (y, x) = model.scroll;
    let end = main_len(model).saturating_sub(main_height(model) as usize) as i32;
    model.scroll = (
        (y as i32 + down).clamp(0, end.max(0)) as u16,
        (x as i32 + right).max(0) as u16,
    );
}

fn command(model: &mut Model, cmd: Cmd) -> Vec<Effect> {
    let list = model.active();
    let index = model.index(list);
    let in_main = model.focus == Focus::Main;
    match cmd {
        Cmd::Down if in_main => scroll(model, 1, 0),
        Cmd::Up if in_main => scroll(model, -1, 0),
        Cmd::Down => return select_moved(model, list, index + 1),
        Cmd::Up => return select_moved(model, list, index.saturating_sub(1)),
        Cmd::PageDown if in_main => scroll(model, main_height(model) as i32, 0),
        Cmd::PageUp if in_main => scroll(model, -(main_height(model) as i32), 0),
        Cmd::Bottom if in_main => scroll(model, i32::MAX / 2, 0),
        Cmd::PageDown => return select_moved(model, list, index + page(model)),
        Cmd::PageUp => return select_moved(model, list, index.saturating_sub(page(model))),
        Cmd::Top if in_main => model.scroll.0 = 0,
        Cmd::Top => return select_moved(model, list, 0),
        Cmd::Bottom => return select_moved(model, list, usize::MAX),
        Cmd::NextPanel | Cmd::PrevPanel => {
            let count = Panel::ALL.len();
            let at = Panel::ALL.iter().position(|&p| p == model.panel).unwrap();
            let next = match (cmd, in_main) {
                (_, true) => at,
                (Cmd::NextPanel, _) => (at + 1) % count,
                _ => (at + count - 1) % count,
            };
            return focus_panel(model, Panel::ALL[next]);
        }
        Cmd::Jump(number) => {
            if let Some(&panel) = Panel::ALL.get(number.wrapping_sub(1)) {
                return focus_panel(model, panel);
            }
        }
        Cmd::FocusMain => model.focus = Focus::Main,
        Cmd::ScrollDown => scroll(model, 1, 0),
        Cmd::ScrollUp => scroll(model, -1, 0),
        Cmd::ScrollPageDown => scroll(model, main_height(model) as i32, 0),
        Cmd::ScrollPageUp => scroll(model, -(main_height(model) as i32), 0),
        Cmd::ScrollLeft => scroll(model, 0, -4),
        Cmd::ScrollRight => scroll(model, 0, 4),
        Cmd::NextTab | Cmd::PrevTab => {
            let tabs = model.tabs(model.panel);
            let at = tabs.iter().position(|&tab| tab == list).unwrap_or(0);
            let next = if cmd == Cmd::NextTab {
                (at + 1) % tabs.len()
            } else {
                (at + tabs.len() - 1) % tabs.len()
            };
            model.sub.insert(model.panel, tabs[next]);
            model.scroll = (0, 0);
        }
        Cmd::Activate => return lists::of(list).activate(model, list),
        Cmd::Enter => {
            if !lists::of(list).enter(model, list) {
                model.focus = Focus::Main;
            }
        }
        Cmd::CollapseAll | Cmd::ExpandAll => {
            for row in model.work_rows() {
                if let Row::Group { key, .. } = row {
                    model.set_folded(&key, cmd == Cmd::CollapseAll);
                }
            }
            clamp_all(model);
        }
        Cmd::New => return lists::of(list).create(model, list),
        Cmd::Edit => return lists::of(list).edit(model, list),
        Cmd::Move => return lists::of(list).move_to(model, list),
        Cmd::Remove => return lists::of(list).remove(model, list),
        Cmd::Close | Cmd::ToggleCarnet | Cmd::Pull | Cmd::Search => {
            return lists::of(list).command(model, list, cmd);
        }
        Cmd::Finish => return lists::of(list).finish(model, list),
        Cmd::Tool if matches!(list, List::Work | List::Carnets) => {
            if let Some(work) = lists::of(list).item(model, list) {
                let branch = work.tree().map(|_| work.branch());
                let path = work.path.clone();
                return vec![Effect::Tool { path, branch }];
            }
        }
        Cmd::Tool => {}
        Cmd::Browse => {
            return match lists::of(list).url(model, list) {
                Some(url) => vec![run(model, Job::Browse(url))],
                None => note(model, "no forge URL for this selection"),
            };
        }
        Cmd::CopyMenu => {
            let mut entries = Vec::new();
            for (key, label, value) in [
                ("p", "path", lists::of(list).copy_path(model, list)),
                ("b", "branch", lists::of(list).branch(model, list)),
                ("u", "URL", lists::of(list).url(model, list)),
            ] {
                if let Some(value) = value {
                    entries.push(MenuEntry {
                        key: key.into(),
                        label: format!("{label}: {value}"),
                        action: Action::Copy(value),
                    });
                }
            }
            if !entries.is_empty() {
                model.modal = Some(Modal::menu("Copy", entries));
            }
        }
        Cmd::CopyPath => {
            let kind = lists::of(list);
            if let Some(path) = (kind.copy_path(model, list)).or_else(|| kind.url(model, list)) {
                return update(model, Action::Copy(path));
            }
        }
        Cmd::Filter if !in_main => model.filtering = Some(list),
        Cmd::Filter => {}
        Cmd::Refresh => {
            let jobs = model.schedule.refresh(true);
            return start(model, jobs);
        }
        Cmd::Menu => {
            let here = |binding: &&Binding| matches!(binding.on, On::Lists(lists) if lists.contains(&list.kind()));
            let global = |binding: &&Binding| binding.on == On::Global;
            let entries = (KEYMAP.iter().filter(here))
                .chain(KEYMAP.iter().filter(global))
                .map(|binding| MenuEntry {
                    key: binding.label.into(),
                    label: binding.help.into(),
                    action: Action::Cmd(binding.cmd),
                })
                .collect();
            model.modal = Some(Modal::Menu {
                title: "Actions".into(),
                entries,
                selected: 0,
                legend: Legend::of(list.kind(), &Glyphs::new(model.icons)),
                page: MenuPage::Actions,
                scroll: 0,
            });
        }
        Cmd::NextScreen => {
            model.screen = match model.screen {
                Screen::Normal => Screen::Half,
                _ => Screen::Full,
            }
        }
        Cmd::PrevScreen => {
            model.screen = match model.screen {
                Screen::Full => Screen::Half,
                _ => Screen::Normal,
            }
        }
        Cmd::ToggleLog => model.show_log = !model.show_log,
        Cmd::ExportLog => {
            let job = Job::ExportLog(model.log.clone());
            return vec![run(model, job)];
        }
        Cmd::Back => {
            if in_main {
                model.focus = Focus::Panel(model.panel);
            } else if model.filters.remove(&list).is_some() || lists::of(list).back(model, list) {
                select(model, list, 0);
                return commits(model);
            }
        }
        Cmd::Quit => {
            return vec![Effect::Quit];
        }
    }
    Vec::new()
}

/// The whole terminal, for laying out outside a draw.
fn screen(model: &Model) -> Rect {
    Rect::new(0, 0, model.size.0, model.size.1)
}

/// Takes `value` as a new carnet's `step`, then asks the next one, or makes the carnet once
/// its issue keys are in.
fn draft_carnet(model: &mut Model, mut draft: Draft, step: DraftStep, value: &str) -> Vec<Effect> {
    let (title, initial, groups, next) = match step {
        DraftStep::Summary => {
            draft.summary = value.into();
            // The summary's first words name the folder unless edited.
            let name = slug(value).split('-').take(5).collect::<Vec<_>>().join("-");
            ("New carnet: folder name", name, Vec::new(), DraftStep::Name)
        }
        // A name without a letter or a digit has no folder name: asked again, as typed.
        DraftStep::Name if slug(value).is_empty() => (
            "New carnet: folder name, with a letter or a digit",
            value.to_owned(),
            Vec::new(),
            DraftStep::Name,
        ),
        DraftStep::Name => {
            draft.name = value.into();
            let group = group_text(draft.group.as_ref()).to_owned();
            ("New carnet: group", group, model.groups(), DraftStep::Group)
        }
        DraftStep::Group => {
            draft.group = Group::parse(value);
            let next = DraftStep::IssueKeys;
            ("New carnet: issue keys", String::new(), Vec::new(), next)
        }
        DraftStep::IssueKeys => {
            let links = Links {
                group: draft.group,
                issue_keys: IssueKeys::resolve(value.split(','), &model.tracker_config),
            };
            let job = Job::NewCarnet {
                name: draft.name,
                workspace: draft.workspace,
                links,
                summary: draft.summary,
            };
            return vec![run(model, job)];
        }
    };
    let then = Submit::Carnet { draft, step: next };
    let action = Action::Ask {
        title: title.into(),
        initial,
        then,
        groups,
    };
    update(model, action)
}

/// A menu, titled `title`, of the workspaces other than `current`, each running `job` with its
/// name; a note when there is no other.
pub(super) fn workspace_menu(
    model: &mut Model,
    title: String,
    current: &str,
    job: impl Fn(String) -> Job,
) -> Vec<Effect> {
    let entries: Vec<MenuEntry> = model
        .snapshot
        .workspaces
        .iter()
        .filter(|name| *name != current)
        .enumerate()
        .map(|(index, name)| MenuEntry {
            key: (index + 1).to_string(),
            label: name.clone(),
            action: Action::Run(job(name.clone())),
        })
        .collect();
    if entries.is_empty() {
        return note(model, "no other workspace: create one with n in panel 1");
    }
    model.modal = Some(Modal::menu(title, entries));
    Vec::new()
}

pub(super) fn confirm(
    model: &mut Model,
    title: String,
    lines: Vec<String>,
    job: Job,
) -> Vec<Effect> {
    model.modal = Some(Modal::Confirm { title, lines, job });
    Vec::new()
}

fn mouse_event(model: &mut Model, mouse: MouseEvent) -> Vec<Effect> {
    if model.modal.is_some() {
        return Vec::new();
    }
    let areas = areas(model, screen(model));
    let at = Position::new(mouse.column, mouse.row);
    let in_main = areas.main.is_some_and(|rect| rect.contains(at));
    let panel = areas
        .panels
        .iter()
        .find(|(_, rect)| rect.contains(at))
        .copied();
    match mouse.kind {
        MouseEventKind::ScrollDown if in_main => scroll(model, 3, 0),
        MouseEventKind::ScrollUp if in_main => scroll(model, -3, 0),
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            if let Some((panel, _)) = panel {
                let list = model.list(panel);
                let index = model.index(list);
                let index = if mouse.kind == MouseEventKind::ScrollDown {
                    index + 1
                } else {
                    index.saturating_sub(1)
                };
                return select_moved(model, list, index);
            }
        }
        MouseEventKind::Down(MouseButton::Left) if in_main => model.focus = Focus::Main,
        MouseEventKind::Down(MouseButton::Left) => {
            let Some((panel, rect)) = panel else {
                return Vec::new();
            };
            let mut effects = focus_panel(model, panel);
            let list = model.list(panel);
            let inner_top = rect.y + 1;
            if rect.height >= 3 && mouse.row >= inner_top && mouse.row < rect.bottom() - 1 {
                let start = offset(model.index(list), rect.height - 2);
                let row = start + (mouse.row - inner_top) as usize;
                if row < model.len(list) {
                    effects.extend(select_moved(model, list, row));
                }
            }
            return effects;
        }
        _ => {}
    }
    Vec::new()
}

#[cfg(test)]
pub mod tests {
    use super::super::app::Readme;
    use super::*;
    use crate::carnet::Stamp;
    use crate::config::Icons;
    use crate::finish::{self, Scope, Step};
    use crate::git::Commit;
    use crate::links::tests::{group, key, keys};
    use crate::links::{Group, IssueKey, Links};
    use crate::reviews::Role;
    use crate::state::Repo;
    use crate::worktrunk::{Forge, Worktree};

    /// Links in `group` to the issue keys in `name`, as the default pattern finds them.
    fn linking(group: &str, name: &str) -> Links {
        let config = crate::config::Config::default();
        let found = crate::config::issue_keys(&config.issue_key_regex().unwrap(), name);
        Links {
            group: Group::parse(group),
            issue_keys: found.into_iter().map(IssueKey::listed).collect(),
        }
    }

    /// A worktree linking the issue keys in its branch.
    pub fn work(repo: &str, branch: &str, group: &str, workspace: &str) -> Work {
        let main = branch == "main";
        let path = if main {
            PathBuf::from(format!("/src/{repo}"))
        } else {
            PathBuf::from(format!("/src/{repo}.{branch}"))
        };
        Work {
            path: path.clone(),
            workspace: workspace.into(),
            links: linking(group, branch),
            tab: false,
            kind: WorkKind::Worktree {
                repo: PathBuf::from(format!("/src/{repo}")),
                repo_name: repo.into(),
                tree: Box::new(Worktree {
                    path,
                    branch: Some(branch.into()),
                    main,
                    on_default: main,
                    default_branch: Some("main".into()),
                    short_sha: "abc1234".into(),
                    subject: "Commit".into(),
                    ..Worktree::default()
                }),
            },
        }
    }

    /// A carnet in `group`, linking the issue keys in its name.
    pub fn carnet(name: &str, group: &str, workspace: &str) -> Work {
        Work {
            path: PathBuf::from(format!("/data/{name}")),
            workspace: workspace.into(),
            links: linking(group, name),
            tab: false,
            kind: WorkKind::Carnet {
                closed: false,
                summary: String::new(),
                readme: None,
            },
        }
    }

    /// With carnets enabled, one in the ABC-1 group and two ungrouped, open, and one closed in
    /// `side`.
    pub fn with_carnets(mut model: Model) -> Model {
        model.carnets = true;
        let open = [
            carnet("2026-10-01-ABC-1-logs", "ABC-1", "default"),
            carnet("2026-09-20-old", "", "default"),
            carnet("2026-10-02-ideas", "", "default"),
        ];
        let mut closed = carnet("2026-08-01-done", "", "side");
        if let WorkKind::Carnet { closed, .. } = &mut closed.kind {
            *closed = true;
        }
        model.snapshot.work.extend(open.clone());
        model.snapshot.carnets = open.into_iter().chain([closed]).collect();
        // Newest first, as a snapshot lists them.
        model.snapshot.carnets.sort_by(|a, b| b.path.cmp(&a.path));
        model
    }

    pub fn snapshot() -> Snapshot {
        let repo = |name: &str, workspace: &str| Repo {
            path: PathBuf::from(format!("/src/{name}")),
            alias: None,
            default_workspace: workspace.into(),
        };
        Snapshot {
            here: Some("default".into()),
            workspaces: vec!["default".into(), "side".into()],
            repos: vec![repo("api", "default"), repo("web", "side")],
            work: vec![
                work("api", "main", "", "default"),
                work("api", "ABC-1-login", "ABC-1", "default"),
                work("web", "ABC-1-form", "ABC-1", "default"),
                work("web", "main", "", "side"),
            ],
            carnets: Vec::new(),
            forges: [(
                PathBuf::from("/src/api"),
                Forge {
                    url: "https://forge/api".into(),
                    provider: "github".into(),
                },
            )]
            .into(),
        }
    }

    pub fn model() -> Model {
        let mut model = Model::new((120, 40));
        update(
            &mut model,
            Action::Loaded {
                snapshot: Ok(snapshot()),
                full: false,
                log: Vec::new(),
            },
        );
        model.schedule.finish_all();
        model
    }

    fn press(model: &mut Model, keys: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\x1b' => KeyCode::Esc,
                '\t' => KeyCode::Tab,
                '\x08' => KeyCode::Backspace,
                c => KeyCode::Char(c),
            };
            effects.extend(update(
                model,
                Action::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            ));
        }
        effects
    }

    fn jobs(effects: Vec<Effect>) -> Vec<Job> {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Run(Job::Commits(_)) => None,
                Effect::Run(job) => Some(job),
                _ => None,
            })
            .collect()
    }

    fn titles(model: &Model) -> Vec<String> {
        model
            .work_rows()
            .into_iter()
            .map(|line| match line {
                Row::Group { group, .. } => format!("[{group}]"),
                Row::Item(index) => model.snapshot.work[index].title(),
            })
            .collect()
    }

    #[test]
    fn work_lists_the_selected_workspace_grouped_then_ungrouped() {
        let mut model = model();
        assert_eq!(
            titles(&model),
            ["[ABC-1]", "api:ABC-1-login", "web:ABC-1-form", "api:main"]
        );
        press(&mut model, "1j");
        assert_eq!(titles(&model), ["web:main"]);
    }

    #[test]
    fn enter_folds_a_group_and_space_opens_all_of_it() {
        let mut model = model();
        press(&mut model, "\n");
        assert_eq!(titles(&model), ["[ABC-1]", "api:main"]);
        let jobs = jobs(press(&mut model, " "));
        assert_eq!(
            jobs,
            [Job::Open(vec![
                "/src/api.ABC-1-login".into(),
                "/src/web.ABC-1-form".into()
            ])]
        );
        press(&mut model, "=");
        assert_eq!(titles(&model).len(), 4);
        press(&mut model, "-");
        assert_eq!(titles(&model).len(), 2);
    }

    #[test]
    fn navigation_moves_and_clamps() {
        let mut model = model();
        press(&mut model, "jjjjjjj");
        assert_eq!(model.index(List::Work), 3);
        press(&mut model, "G");
        assert_eq!(model.index(List::Work), 3, "G does nothing");
        press(&mut model, "<");
        assert_eq!(model.index(List::Work), 0);
        press(&mut model, ">");
        assert_eq!(model.index(List::Work), 3);
        press(&mut model, "gg");
        assert_eq!(model.index(List::Work), 3, "gg no longer jumps to the top");
    }

    #[test]
    fn g_opens_the_tool_on_a_worktree_or_carnet_only() {
        let mut model = with_reviews(with_carnets(model()));
        assert!(press(&mut model, "g").is_empty(), "a group header");
        press(&mut model, "j");
        assert_eq!(
            press(&mut model, "g"),
            [Effect::Tool {
                path: "/src/api.ABC-1-login".into(),
                branch: Some("ABC-1-login".into()),
            }]
        );
        press(&mut model, "]");
        let effects = press(&mut model, "g");
        let [Effect::Tool { path, branch: None }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert!(
            path.starts_with("/data/"),
            "a carnet, its branch read when run"
        );
        press(&mut model, "3");
        assert!(press(&mut model, "g").is_empty(), "Reviews have no path");
    }

    #[test]
    fn panels_and_sub_tabs() {
        let mut model = tall_main();
        press(&mut model, "h");
        assert_eq!(model.focus, Focus::Panel(Panel::Workspaces));
        press(&mut model, "]");
        assert_eq!(model.active(), List::Repos);
        press(&mut model, "\t");
        assert_eq!(model.focus, Focus::Panel(Panel::Work));
        press(&mut model, "0");
        assert_eq!(model.focus, Focus::Main);
        press(&mut model, "j");
        assert_eq!(model.scroll, (1, 0));
        press(&mut model, "\x1b");
        assert_eq!(model.focus, Focus::Panel(Panel::Work));
    }

    #[test]
    fn selecting_an_item_fetches_its_commits_once() {
        let mut model = model();
        let effects = press(&mut model, "j");
        assert_eq!(
            effects,
            [Effect::Run(Job::Commits("/src/api.ABC-1-login".into()))]
        );
        update(
            &mut model,
            Action::Commits(
                "/src/api.ABC-1-login".into(),
                vec![Commit::fake("abc", "x")],
            ),
        );
        assert_eq!(
            press(&mut model, "j"),
            [Effect::Run(Job::Commits("/src/web.ABC-1-form".into()))]
        );
        assert!(press(&mut model, "k").is_empty(), "already loaded");
    }

    #[test]
    fn remove_confirms_and_skips_main_worktrees() {
        let mut model = model();
        model.snapshot.work[1].tree_mut().dirty = true;
        assert!(press(&mut model, "d").is_empty());
        let Some(Modal::Confirm { lines, .. }) = &model.modal else {
            panic!("no confirmation");
        };
        assert!(lines[1].contains("uncommitted"));
        let jobs = jobs(press(&mut model, "y"));
        let [Job::Remove(removals)] = jobs.as_slice() else {
            panic!("{jobs:?}");
        };
        assert_eq!(removals.len(), 2);
        assert!(removals[0].force && !removals[1].force);
        press(&mut model, ">");
        press(&mut model, "d");
        assert!(model.modal.is_none());
        assert!(model.log.last().unwrap().command.contains("main"));
    }

    fn plan_scope(effects: Vec<Effect>) -> (Scope, Vec<PathBuf>) {
        match &jobs(effects)[..] {
            [Job::Plan { scope, repos }] => (scope.clone(), repos.clone()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn f_plans_the_selections_groups_a_workspace_or_an_issues_linked_work() {
        let mut model = with_issues(model());
        let both = vec![PathBuf::from("/src/api"), PathBuf::from("/src/web")];
        let group = Scope::group("ABC-1");
        assert_eq!(
            plan_scope(press(&mut model, "jf")),
            (group, both.clone()),
            "one row selected, its whole group across repos"
        );
        let alone = Scope::Work {
            groups: Vec::new(),
            items: vec!["/src/api".into()],
        };
        assert_eq!(
            plan_scope(press(&mut model, ">f")),
            (alone, vec!["/src/api".into()]),
            "an item in no group, alone"
        );
        assert_eq!(
            plan_scope(press(&mut model, "1f")),
            (Scope::Workspace("default".into()), both.clone())
        );
        let (scope, repos) = plan_scope(press(&mut model, "4]jf"));
        assert_eq!(
            scope,
            Scope::Issue {
                key: key("ABC-1"),
                label: "ABC-1".into(),
                state: "to do".into()
            }
        );
        assert_eq!(repos, both);
        assert!(model.schedule.is_loading(Source::Run), "fetching shows");
        press(&mut model, "3");
        assert!(
            jobs(press(&mut model, "f")).is_empty(),
            "reviews have no plan"
        );
    }

    /// The ABC-1 plan: api:ABC-1-login integrated, web:ABC-1-form gone and dirty.
    fn planned(model: &mut Model) -> Vec<Effect> {
        model.snapshot.work[1].tree_mut().integrated = true;
        let form = model.snapshot.work[2].tree_mut();
        (form.gone, form.dirty) = (true, true);
        let plan = finish::plan(
            &model.snapshot,
            &Scope::group("ABC-1"),
            &[],
            &model.tracker_config,
        );
        let log = vec![Logged {
            command: "git -C /src/api fetch --prune".into(),
            error: None,
        }];
        update(
            model,
            Action::Planned {
                plan: Ok(plan),
                log,
            },
        )
    }

    fn finish_modal(model: &Model) -> (&finish::Plan, usize) {
        match &model.modal {
            Some(Modal::Finish { plan, selected }) => (plan, *selected),
            other => panic!("no finish plan: {other:?}"),
        }
    }

    #[test]
    fn the_finish_plan_opens_toggles_and_runs_its_checked_lines() {
        let mut model = model();
        let effects = planned(&mut model);
        assert_eq!(effects, [Effect::Run(Job::Refresh { full: false })]);
        assert_eq!(model.log.len(), 1, "the fetch is logged");
        let (plan, selected) = finish_modal(&model);
        assert_eq!(selected, 0);
        assert_eq!(plan.checked().len(), 1, "the dirty one is unchecked");
        let last = plan.lines.len() - 1;
        assert!(matches!(plan.lines[last], finish::Line::Info { .. }));
        press(&mut model, "jjjjjj");
        assert_eq!(
            finish_modal(&model).1,
            last,
            "j reaches the info lines, and stops at the last"
        );
        press(&mut model, " ");
        assert_eq!(
            finish_modal(&model).0.checked().len(),
            1,
            "an info line stays"
        );
        press(&mut model, "kkkkkk");
        press(&mut model, "j k ");
        assert_eq!(finish_modal(&model).1, 0);
        let jobs = jobs(press(&mut model, "\n"));
        let [Job::Finish(steps)] = &jobs[..] else {
            panic!("{jobs:?}");
        };
        let [Step::Remove { removal, .. }] = &steps[..] else {
            panic!("{steps:?}");
        };
        assert_eq!(removal.path, PathBuf::from("/src/web.ABC-1-form"));
        assert!(removal.force);
        assert!(model.modal.is_none());
    }

    #[test]
    fn the_finish_plan_cancels_and_runs_nothing_when_nothing_is_checked() {
        let mut model = model();
        planned(&mut model);
        assert!(jobs(press(&mut model, "\x1b")).is_empty());
        assert!(model.modal.is_none());
        planned(&mut model);
        press(&mut model, " ");
        assert!(jobs(press(&mut model, "\n")).is_empty());
        assert!(model.modal.is_none());
        update(
            &mut model,
            Action::Planned {
                plan: Err("no database".into()),
                log: Vec::new(),
            },
        );
        assert!(model.modal.is_none());
        assert_eq!(model.log.last().unwrap().command, "finish plan");
    }

    #[test]
    fn panel_one_adds_and_removes_workspaces_and_forgets_repos() {
        let mut model = model();
        press(&mut model, "1n");
        assert_eq!(
            jobs(press(&mut model, "w\n")),
            [Job::AddWorkspace("w".into())]
        );
        press(&mut model, "jd");
        assert_eq!(
            jobs(press(&mut model, "y")),
            [Job::RemoveWorkspace("side".into())]
        );
        press(&mut model, "]d");
        assert_eq!(
            jobs(press(&mut model, "y")),
            [Job::Forget("/src/api".into())]
        );
    }

    #[test]
    fn popup_hints_come_from_the_popup_keymap() {
        use crate::tui::app::{Popup, popup_hints};
        assert_eq!(
            popup_hints(Popup::Confirm),
            "Enter/y confirm · Esc/n cancel"
        );
        assert_eq!(popup_hints(Popup::Filter), "Enter keep · Esc clear");
        let mut model = model();
        press(&mut model, "?>");
        let Some(Modal::Menu {
            selected, entries, ..
        }) = &model.modal
        else {
            panic!();
        };
        assert_eq!(*selected, entries.len() - 1, "> goes to the last entry");
    }

    #[test]
    fn confirmation_can_be_cancelled() {
        let mut model = model();
        press(&mut model, "d");
        assert!(jobs(press(&mut model, "n")).is_empty());
        assert!(model.modal.is_none());
    }

    #[test]
    fn new_prompts_for_a_branch_in_the_selection_repo_and_group() {
        let mut model = model();
        press(&mut model, "n");
        let effects = press(&mut model, "fix\n");
        assert_eq!(
            jobs(effects),
            [Job::Create {
                repo: "/src/api".into(),
                branch: "fix".into(),
                workspace: "default".into(),
                group: Group::parse("ABC-1"),
            }]
        );
        assert!(model.schedule.is_loading(Source::Run));
    }

    #[test]
    fn new_in_an_empty_workspace_asks_for_the_repo() {
        let mut model = model();
        model.snapshot.work.clear();
        press(&mut model, "n");
        assert!(matches!(model.modal, Some(Modal::Menu { .. })));
        press(&mut model, "j\n");
        let effects = press(&mut model, "b\n");
        let [Job::Create { repo, .. }]: [Job; 1] = jobs(effects).try_into().unwrap() else {
            panic!();
        };
        assert_eq!(repo, PathBuf::from("/src/web"));
    }

    #[test]
    fn move_offers_the_other_workspaces() {
        let mut model = model();
        press(&mut model, ">");
        press(&mut model, "m");
        let effects = press(&mut model, "\n");
        assert_eq!(
            jobs(effects),
            [Job::Move {
                paths: vec!["/src/api".into()],
                workspace: "side".into()
            }]
        );
    }

    #[test]
    fn edit_regroups_and_renames_repos() {
        let mut model = model();
        press(&mut model, "j");
        press(&mut model, "eg");
        let Some(Modal::Prompt { input, .. }) = &model.modal else {
            panic!();
        };
        assert_eq!(input.value(), "ABC-1");
        press(&mut model, "\x1b");
        press(&mut model, "1]e");
        let effects = press(&mut model, "a\n");
        assert_eq!(
            jobs(effects),
            [Job::SetAlias {
                repo: "/src/api".into(),
                alias: "a".into()
            }]
        );
    }

    #[test]
    fn x_closes_carnet_tabs_without_changing_their_lifecycle() {
        let mut model = with_carnets(model());
        press(&mut model, "]>");
        assert!(jobs(press(&mut model, "x")).is_empty());
        model.snapshot.carnets.last_mut().unwrap().tab = true;
        assert_eq!(
            jobs(press(&mut model, "x")),
            [Job::Close(vec!["/data/2026-08-01-done".into()])]
        );
        assert!(model.carnet().unwrap().closed());
        assert!(model.modal.is_none());
        press(&mut model, "<");
        model.snapshot.carnets[0].tab = true;
        assert_eq!(
            jobs(press(&mut model, "x")),
            [Job::Close(vec!["/data/2026-10-02-ideas".into()])]
        );
        assert!(!model.carnet().unwrap().closed());
    }

    #[test]
    fn x_closes_review_tabs_from_both_review_lists() {
        let mut model = with_reviews(model());
        press(&mut model, "3");
        assert!(jobs(press(&mut model, "x")).is_empty());
        model.snapshot.work[1].tree_mut().branch = Some("change-2".into());
        assert!(jobs(press(&mut model, "x")).is_empty());
        model.snapshot.work[1].tab = true;
        assert_eq!(
            jobs(press(&mut model, "x")),
            [Job::Close(vec!["/src/api.ABC-1-login".into()])]
        );
        press(&mut model, "j");
        assert!(jobs(press(&mut model, "x")).is_empty(), "unregistered repo");
        press(&mut model, "]");
        let branch = model.review().unwrap().branch.clone();
        model.snapshot.work[1].tree_mut().branch = Some(branch);
        assert_eq!(
            jobs(press(&mut model, "x")),
            [Job::Close(vec!["/src/api.ABC-1-login".into()])]
        );
        assert!(model.modal.is_none());
    }

    #[test]
    fn x_closes_only_open_issue_tabs_across_workspaces_including_shared_carnets() {
        let mut model = with_issues(with_carnets(model()));
        press(&mut model, "4]");
        assert!(jobs(press(&mut model, "x")).is_empty(), "no linked work");
        press(&mut model, "j");
        assert_eq!(model.issue().unwrap().key, key("ABC-1"));
        assert!(jobs(press(&mut model, "x")).is_empty(), "no open tabs");
        model.snapshot.work[1].tab = true;
        model.snapshot.work[1].workspace = "side".into();
        let mut shared = carnet("shared", "ABC-1", "side");
        shared.tab = true;
        shared.links.issue_keys = keys(&["ABC-1", "api#4"]);
        if let WorkKind::Carnet { closed, .. } = &mut shared.kind {
            *closed = true;
        }
        model.snapshot.carnets.push(shared);
        assert_eq!(
            jobs(press(&mut model, "x")),
            [Job::Close(vec![
                "/src/api.ABC-1-login".into(),
                "/data/shared".into()
            ])]
        );
        assert!(model.snapshot.carnets.last().unwrap().closed());
        assert!(model.modal.is_none());
    }

    #[test]
    fn pull_and_close_act_on_the_selection() {
        let mut model = model();
        model.snapshot.work[2].tab = true;
        assert_eq!(
            jobs(press(&mut model, "x")),
            [Job::Close(vec!["/src/web.ABC-1-form".into()])]
        );
        press(&mut model, ">");
        assert_eq!(
            jobs(press(&mut model, "p")),
            [Job::Pull(vec!["/src/api".into()])]
        );
        assert_eq!(
            jobs(press(&mut model, "o")),
            [Job::Browse("https://forge/api/tree/main".into())]
        );
    }

    #[test]
    fn browse_opens_a_worktrees_review_over_its_branch() {
        let mut model = model();
        press(&mut model, ">");
        let tree = model.snapshot.work[0].tree_mut();
        assert_eq!(tree.branch.as_deref(), Some("main"));
        tree.ci = Some(worktrunk::Ci {
            state: Some(worktrunk::CiState::Running),
            checks: Some(worktrunk::Checks::Running),
            conflicts: false,
            stale: false,
            branch_workflow: false,
            review: Some(worktrunk::CiReview {
                number: Some(5),
                url: Some("https://forge/api/pull/5".into()),
                decision: None,
            }),
        });
        assert_eq!(
            jobs(press(&mut model, "o")),
            [Job::Browse("https://forge/api/pull/5".into())]
        );
    }

    #[test]
    fn startup_focuses_work_with_the_current_session_selected() {
        let model = model();
        assert_eq!(model.focus, Focus::Panel(Panel::Work));
        assert_eq!(model.index(List::Workspaces), 0);
        assert_eq!(model.workspace(), model.snapshot.here.as_deref());
    }

    #[test]
    fn space_on_a_workspace_switches_or_attaches() {
        let mut model = model();
        press(&mut model, "1j");
        assert_eq!(
            jobs(press(&mut model, " ")),
            [Job::SwitchWorkspace("side".into())]
        );
        assert_eq!(model.focus, Focus::Panel(Panel::Work));
        assert_eq!(model.workspace(), model.snapshot.here.as_deref());
        // Back onto "side" to attach to it.
        press(&mut model, "1j");
        model.snapshot.here = None;
        assert_eq!(press(&mut model, " "), [Effect::Attach("side".into())]);
    }

    #[test]
    fn filter_narrows_the_focused_list() {
        let mut model = model();
        press(&mut model, "/form");
        assert_eq!(titles(&model), ["[ABC-1]", "web:ABC-1-form"]);
        press(&mut model, "\n");
        assert_eq!(model.filtering, None);
        assert_eq!(model.filter(List::Work), "form");
        press(&mut model, "\x1b");
        assert_eq!(titles(&model).len(), 4);
    }

    #[test]
    fn the_work_filter_matches_a_carnets_summary() {
        let mut model = with_carnets(model());
        if let WorkKind::Carnet { summary, .. } = &mut model.snapshot.work[4].kind {
            *summary = "Token refresh".into();
        }
        press(&mut model, "/token");
        assert_eq!(titles(&model), ["[ABC-1]", "2026-10-01-ABC-1-logs"]);
    }

    #[test]
    fn enter_under_a_filter_toggles_a_groups_fold() {
        let mut model = model();
        press(&mut model, "\n");
        assert_eq!(titles(&model), ["[ABC-1]", "api:main"]);
        press(&mut model, "/form\n<\n\x1b");
        assert_eq!(titles(&model).len(), 4, "unfolded while filtered");
    }

    #[test]
    fn refresh_keeps_the_selection() {
        let mut model = model();
        press(&mut model, "jj");
        let mut snapshot = snapshot();
        snapshot
            .work
            .insert(0, work("api", "AAA-1", "AAA-1", "default"));
        update(
            &mut model,
            Action::Loaded {
                snapshot: Ok(snapshot),
                full: false,
                log: Vec::new(),
            },
        );
        assert_eq!(
            model.targets()[0].path(),
            &PathBuf::from("/src/web.ABC-1-form")
        );
    }

    #[test]
    fn fast_refreshes_keep_the_last_full_refreshs_ci() {
        let mut model = model();
        let ci = worktrunk::Ci {
            state: Some(worktrunk::CiState::Failed),
            checks: Some(worktrunk::Checks::Failed),
            conflicts: false,
            stale: false,
            branch_workflow: false,
            review: None,
        };
        let refresh = |model: &mut Model, ci: Option<&worktrunk::Ci>, full: bool| {
            let mut snapshot = snapshot();
            snapshot.work[1].tree_mut().ci = ci.cloned();
            update(
                model,
                Action::Loaded {
                    snapshot: Ok(snapshot),
                    full,
                    log: Vec::new(),
                },
            );
            model.snapshot.work[1].tree().unwrap().ci.clone()
        };
        assert_eq!(refresh(&mut model, Some(&ci), true), Some(ci.clone()));
        assert_eq!(refresh(&mut model, None, false), Some(ci), "kept");
        assert_eq!(refresh(&mut model, None, true), None, "cleared");
    }

    #[test]
    fn fast_refreshes_keep_commits_whose_head_did_not_move() {
        let mut model = with_carnets(model());
        let loaded = |model: &Model| -> Vec<String> {
            let mut paths: Vec<String> = (model.commits.keys())
                .map(|path| path.display().to_string())
                .collect();
            paths.sort();
            paths
        };
        let refresh = |model: &mut Model, snapshot: Snapshot, full: bool| {
            let log = Vec::new();
            let snapshot = Ok(snapshot);
            update(
                model,
                Action::Loaded {
                    snapshot,
                    full,
                    log,
                },
            );
        };
        let fill = |model: &mut Model| {
            for work in &model.snapshot.work {
                model.commits.insert(work.path.clone(), Vec::new());
            }
        };
        fill(&mut model);
        let mut snapshot = model.snapshot.clone();
        snapshot.work[1].tree_mut().short_sha = "def5678".into();
        snapshot
            .work
            .retain(|work| work.path != Path::new("/src/web"));
        for listed in [&mut snapshot.work, &mut snapshot.carnets] {
            listed.retain(|work| !work.path.ends_with("2026-09-20-old"));
        }
        refresh(&mut model, snapshot, false);
        assert_eq!(
            loaded(&model),
            ["/src/api", "/src/web.ABC-1-form"],
            "the moved head, the unlisted items and the unselected carnets are dropped"
        );
        fill(&mut model);
        let snapshot = model.snapshot.clone();
        refresh(&mut model, snapshot, true);
        assert!(loaded(&model).is_empty(), "a full refresh drops everything");
    }

    #[test]
    fn exporting_captures_the_log_without_refreshing_items() {
        let mut model = model();
        let entries = vec![Logged {
            command: "git pull".into(),
            error: Some("first line\nsecond line".into()),
        }];
        model.push_log(entries.clone());
        model.show_log = false;
        model.focus = Focus::Main;
        let job = Job::ExportLog(entries.clone());
        assert_eq!(press(&mut model, "E"), [Effect::Run(job.clone())]);
        model.push_log([Logged {
            command: "later command".into(),
            error: None,
        }]);
        let saved = Logged {
            command: "export command log to /state/atelier/logs/export.log".into(),
            error: None,
        };
        assert_eq!(
            update(
                &mut model,
                Action::Finished {
                    job,
                    log: vec![saved.clone()],
                    error: None
                }
            ),
            [],
            "exporting changes nothing to refresh"
        );
        assert_eq!(model.log.last(), Some(&saved));
        assert!(!model.schedule.loading().any(|_| true));
    }

    #[test]
    fn finished_jobs_log_and_refresh() {
        let mut model = model();
        press(&mut model, "p");
        let effects = update(
            &mut model,
            Action::Finished {
                job: Job::Pull(vec!["/src/api".into()]),
                log: vec![Logged {
                    command: "git pull".into(),
                    error: Some("boom".into()),
                }],
                error: Some("boom".into()),
            },
        );
        assert_eq!(effects, [Effect::Run(Job::Refresh { full: false })]);
        assert_eq!(model.log.len(), 1);
        assert_eq!(model.schedule.loading().collect::<Vec<_>>(), [Source::Wt]);
        press(&mut model, "x");
        let finished = |job| Action::Finished {
            job,
            log: Vec::new(),
            error: None,
        };
        let close = finished(Job::Close(vec!["/src/api".into()]));
        assert!(
            update(&mut model, close).is_empty(),
            "the refresh in flight holds the next one back"
        );
        assert_eq!(
            update(
                &mut model,
                finished(Job::Browse("https://forge/api".into()))
            ),
            [],
            "browsing changes nothing to refresh"
        );
        let effects = update(
            &mut model,
            Action::Loaded {
                snapshot: Ok(snapshot()),
                full: false,
                log: Vec::new(),
            },
        );
        assert_eq!(effects, [Effect::Run(Job::Refresh { full: false })]);
    }

    #[test]
    fn pulling_rows_spin_until_their_job_finishes() {
        let mut model = model();
        press(&mut model, " ");
        model.schedule.finish_all();
        assert!(!model.animating());
        let jobs = jobs(press(&mut model, "p"));
        let paths = vec![
            PathBuf::from("/src/api.ABC-1-login"),
            PathBuf::from("/src/web.ABC-1-form"),
        ];
        assert_eq!(jobs, [Job::Pull(paths.clone())]);
        assert!(paths.iter().all(|path| model.schedule.is_pulling(path)));
        assert!(model.animating());
        let frame = model.frame;
        update(&mut model, Action::Tick);
        assert_eq!(model.frame, frame + 1);
        update(
            &mut model,
            Action::Finished {
                job: Job::Pull(paths),
                log: Vec::new(),
                error: None,
            },
        );
        assert!(!model.animating());
    }

    #[test]
    fn control_c_quits_from_anywhere() {
        let ctrl_c = Action::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        for keys in ["n", "/", "?", "d"] {
            let mut model = model();
            press(&mut model, keys);
            assert_eq!(update(&mut model, ctrl_c.clone()), [Effect::Quit], "{keys}");
        }
    }

    /// A short terminal with a worktree selected whose commits overflow the main view.
    fn tall_main() -> Model {
        let mut model = model();
        model.size = (120, 12);
        press(&mut model, "j");
        let commits = (0..20)
            .map(|n| Commit::fake("abc1234", &format!("commit {n}")))
            .collect();
        update(
            &mut model,
            Action::Commits("/src/api.ABC-1-login".into(), commits),
        );
        model
    }

    #[test]
    fn list_keys_scroll_the_focused_main_view() {
        let mut model = tall_main();
        press(&mut model, "0>");
        let bottom = model.scroll.0;
        press(&mut model, ".J");
        assert_eq!(model.scroll.0, bottom, "stops at the end");
        assert_eq!(model.index(List::Work), 1);
        assert!(model.scroll.0 > 0);
        press(&mut model, ",<");
        assert_eq!(model.scroll.0, 0);
    }

    #[test]
    fn menu_entries_answer_to_their_key() {
        let mut model = model();
        model.snapshot.workspaces.push("third".into());
        press(&mut model, "m2");
        assert!(model.modal.is_none());
        press(&mut model, "?n");
        assert!(
            matches!(model.modal, Some(Modal::Prompt { .. })),
            "? then n asks for a branch"
        );
    }

    #[test]
    fn screen_modes_and_quit() {
        let mut model = model();
        press(&mut model, "++");
        assert_eq!(model.screen, Screen::Full);
        press(&mut model, "_");
        assert_eq!(model.screen, Screen::Half);
        press(&mut model, "@");
        assert!(!model.show_log);
        assert_eq!(press(&mut model, "q"), [Effect::Quit]);
    }

    fn menu_keys(model: &Model) -> Vec<String> {
        match &model.modal {
            Some(Modal::Menu { entries, .. }) => {
                entries.iter().map(|entry| entry.key.clone()).collect()
            }
            _ => panic!("no menu"),
        }
    }

    #[test]
    fn actions_menu_lists_the_focused_panels_actions_then_global_ones() {
        let mut model = model();
        press(&mut model, "1]?");
        let keys = menu_keys(&model);
        assert!(keys.contains(&"e".into()) && keys.contains(&"m".into()));
        assert!(!keys.contains(&"p".into()) && !keys.contains(&"x".into()));
        assert!(!keys.contains(&"j/↓".into()), "navigation stays out");
        let global = keys.iter().position(|key| key == "R").unwrap();
        assert!(keys.iter().position(|key| key == "e").unwrap() < global);
        press(&mut model, "\x1b2?");
        let keys = menu_keys(&model);
        assert!(keys.contains(&"p".into()) && keys.contains(&"Space".into()));
    }

    fn menu_legend(model: &Model) -> Vec<&'static str> {
        match &model.modal {
            Some(Modal::Menu { legend, .. }) => legend.iter().map(|legend| legend.help).collect(),
            _ => panic!("no menu"),
        }
    }

    fn menu_scroll(model: &Model) -> (MenuPage, usize, usize) {
        match &model.modal {
            Some(Modal::Menu {
                page,
                selected,
                scroll,
                ..
            }) => (*page, *selected, *scroll),
            _ => panic!("no menu"),
        }
    }

    #[test]
    fn actions_menu_explains_the_focused_panels_marks() {
        let mut model = model();
        press(&mut model, "?");
        let legend = menu_legend(&model);
        assert!(legend.contains(&"would conflict when merged") && legend.contains(&"folded group"));
        assert!(!legend.contains(&"an open review links it"));
        assert!(
            !legend.contains(&"worktree"),
            "no Unicode glyph for a worktree"
        );
        assert_eq!(legend.last(), Some(&"loading"), "global marks come last");
        press(&mut model, "\x1b4?");
        let legend = menu_legend(&model);
        assert!(legend.contains(&"an open review links it"));
        assert!(!legend.contains(&"would conflict when merged"));
        model.icons = Icons::Nerd;
        press(&mut model, "\x1b?");
        assert!(
            menu_legend(&model).contains(&"issue"),
            "Nerd Fonts have one"
        );
    }

    #[test]
    fn actions_menu_pages_between_its_actions_and_its_legend() {
        let mut model = model();
        model.size = (100, 20);
        press(&mut model, "?");
        let Some(Modal::Menu {
            entries, legend, ..
        }) = &model.modal
        else {
            panic!();
        };
        let last = entries.len() - 1;
        let most = Legend::lines(legend) - 16;
        use MenuPage::{Actions, Legend as Marks};
        press(&mut model, ">j");
        assert_eq!(
            menu_scroll(&model),
            (Actions, last, 0),
            "j stops at the last action"
        );
        press(&mut model, "\t");
        assert_eq!(
            menu_scroll(&model),
            (Marks, last, 0),
            "Tab shows the legend"
        );
        press(&mut model, "jj");
        assert_eq!(
            menu_scroll(&model),
            (Marks, last, 2),
            "j scrolls the legend"
        );
        press(&mut model, ">j");
        assert_eq!(
            menu_scroll(&model),
            (Marks, last, most),
            "no further than its end"
        );
        press(&mut model, "k\n");
        assert_eq!(
            menu_scroll(&model),
            (Marks, last, most - 1),
            "Enter does nothing there"
        );
        press(&mut model, "<");
        assert_eq!(menu_scroll(&model), (Marks, last, 0));
        update(
            &mut model,
            Action::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        );
        assert_eq!(menu_scroll(&model), (Actions, last, 0), "S-Tab goes back");
    }

    #[test]
    fn the_legend_heads_each_section_and_keeps_the_work_order() {
        let mut model = model();
        press(&mut model, "?");
        let Some(Modal::Menu { legend, .. }) = &model.modal else {
            panic!();
        };
        let mut sections: Vec<&str> = legend.iter().map(|legend| legend.section).collect();
        sections.dedup();
        assert_eq!(
            sections,
            [
                "Row colour",
                "Rows",
                "Tab",
                "Groups",
                "Changes",
                "Checkout",
                "Default branch",
                "Remote",
                "Checks",
                "Review",
                "Decision",
                "Merge",
                "Finished",
                "Command log",
                "Hint bar"
            ]
        );
        assert_eq!(Legend::lines(legend), legend.len() + sections.len());
    }

    #[test]
    fn actions_menu_runs_the_chosen_command() {
        let mut model = model();
        press(&mut model, "?");
        assert_eq!(menu_keys(&model)[0], "Space");
        press(&mut model, "\n");
        assert!(model.modal.is_none());
        assert!(
            model.schedule.is_loading(Source::Run),
            "Space opened the group"
        );
    }

    pub use crate::reviews::tests::review;

    /// Reviews of the registered api repo and of an unregistered one, in both roles.
    pub fn with_reviews(mut model: Model) -> Model {
        update(
            &mut model,
            Action::Fetched {
                feed: Feed::Reviews(Provider::GitHub),
                rows: Ok(Rows::Reviews(vec![
                    review(Provider::GitHub, Role::ToReview, 1, "https://forge/other"),
                    review(Provider::GitHub, Role::ToReview, 2, "https://forge/api"),
                    review(Provider::GitHub, Role::Mine, 3, "https://forge/api"),
                ])),
                log: Vec::new(),
            },
        );
        model
    }

    fn review_numbers(model: &Model, list: List) -> Vec<u64> {
        model.reviews(list).iter().map(|r| r.number).collect()
    }

    #[test]
    fn unconfigured_review_providers_are_not_fetched() {
        let mut model = Model::new((120, 40));
        let effects = update(
            &mut model,
            Action::Loaded {
                snapshot: Ok(snapshot()),
                full: false,
                log: Vec::new(),
            },
        );
        assert!(
            jobs(effects).is_empty(),
            "registered GitHub repos do not opt in to reviews"
        );
        model.schedule.finish_all();
        assert_eq!(jobs(press(&mut model, "R")), [Job::Refresh { full: true }]);
    }

    #[test]
    fn configured_reviews_fetch_only_the_selected_provider_and_keep_issue_scopes() {
        let config = crate::config::Config::parse(
            "[reviews]\nproviders = ['gitlab', 'gitlab']\n[tracker.github]\nrepos = ['owner/api']",
        )
        .unwrap();
        let mut model = Model::new((120, 40));
        model.review_config = config.reviews;
        model.tracker_config = config.tracker;
        let mut snapshot = snapshot();
        for name in ["lab", "other"] {
            snapshot.forges.insert(
                PathBuf::from(format!("/src/{name}")),
                Forge {
                    url: format!("https://gitlab.example.com/org/{name}"),
                    provider: "gitlab".into(),
                },
            );
        }
        assert_eq!(
            jobs(update(
                &mut model,
                Action::Loaded {
                    snapshot: Ok(snapshot),
                    full: false,
                    log: Vec::new()
                }
            )),
            [
                Job::Fetch {
                    feed: Feed::Reviews(Provider::GitLab),
                    keys: vec!["gitlab.example.com".into()],
                    force: false
                },
                Job::Fetch {
                    feed: Feed::Issues(crate::issues::Tracker::GitHub),
                    keys: vec!["owner/api".into()],
                    force: false
                },
            ]
        );
    }

    #[test]
    fn listings_fetch_the_reviews_of_their_hosts() {
        let mut model = Model::new((120, 40));
        model.review_config.providers = vec![Provider::GitHub];
        let loaded = |model: &mut Model| {
            jobs(update(
                model,
                Action::Loaded {
                    snapshot: Ok(snapshot()),
                    full: false,
                    log: Vec::new(),
                },
            ))
        };
        let first = Job::Fetch {
            feed: Feed::Reviews(Provider::GitHub),
            keys: vec!["forge".into()],
            force: false,
        };
        assert_eq!(loaded(&mut model), [first], "startup lists them");
        model.schedule.finish_all();
        assert!(loaded(&mut model).is_empty(), "a fast refresh does not");
        press(&mut model, "R");
        model.schedule.finish_all();
        let [Job::Fetch { force: true, .. }] = loaded(&mut model)[..] else {
            panic!("R lists them past the cache");
        };
    }

    #[test]
    fn reviews_replace_their_providers_and_keep_the_selection() {
        let mut model = with_reviews(model());
        assert_eq!(
            review_numbers(&model, List::ToReview),
            [2, 1],
            "newest first"
        );
        assert_eq!(review_numbers(&model, List::Mine), [3]);
        press(&mut model, "3j");
        let gitlab = review(Provider::GitLab, Role::ToReview, 9, "https://lab/x");
        update(
            &mut model,
            Action::Fetched {
                feed: Feed::Reviews(Provider::GitLab),
                rows: Ok(Rows::Reviews(vec![gitlab])),
                log: Vec::new(),
            },
        );
        assert_eq!(review_numbers(&model, List::ToReview), [9, 2, 1]);
        assert_eq!(model.review().unwrap().number, 1, "still selected");
        update(
            &mut model,
            Action::Fetched {
                feed: Feed::Reviews(Provider::GitHub),
                rows: Err("offline".into()),
                log: Vec::new(),
            },
        );
        assert_eq!(review_numbers(&model, List::ToReview).len(), 3);
        assert_eq!(model.log.last().unwrap().command, "gh reviews");
    }

    #[test]
    fn space_checks_out_a_review_of_a_registered_repo() {
        let mut model = with_reviews(model());
        press(&mut model, "3");
        assert_eq!(model.active(), List::ToReview);
        assert_eq!(
            jobs(press(&mut model, " ")),
            [Job::LinkedGroup(Pending::Checkout {
                repo: "/src/api".into(),
                workspace: "default".into(),
                review: Box::new(review(
                    Provider::GitHub,
                    Role::ToReview,
                    2,
                    "https://forge/api"
                )),
            })],
            "its group is looked up first"
        );
        press(&mut model, "j");
        assert!(jobs(press(&mut model, " ")).is_empty());
        assert!(
            model
                .log
                .last()
                .unwrap()
                .command
                .contains("org/other is not registered")
        );
    }

    #[test]
    fn review_sub_tabs_browse_and_copy() {
        let mut model = with_reviews(model());
        press(&mut model, "3]");
        assert_eq!(model.active(), List::Mine);
        assert_eq!(
            jobs(press(&mut model, "o")),
            [Job::Browse("https://forge/api/pull/3".into())]
        );
        assert_eq!(
            press(&mut model, "y"),
            [],
            "the copy menu opens without effects"
        );
        assert_eq!(
            menu_keys(&model),
            ["b", "u"],
            "a review copies its branch or URL"
        );
        press(&mut model, "\x1b");
        assert_eq!(
            update(
                &mut model,
                Action::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
            ),
            [Effect::Copy("https://forge/api/pull/3".into())]
        );
        press(&mut model, "[");
        assert_eq!(model.active(), List::ToReview);
        press(&mut model, "/other");
        assert_eq!(review_numbers(&model, List::ToReview), [1]);
        for keys in ["d", "e", "m", "n"] {
            assert!(press(&mut model, keys).is_empty(), "{keys}");
            assert!(model.modal.is_none(), "{keys}");
        }
    }

    #[test]
    fn a_review_row_knows_its_worktree() {
        let mut model = with_reviews(model());
        model.snapshot.work[1].tree_mut().branch = Some("change-2".into());
        let review = model.reviews(List::ToReview)[0].clone();
        assert_eq!(model.review_project(&review), "api");
        assert_eq!(
            model.review_work(&review).map(|work| work.path().clone()),
            Some("/src/api.ABC-1-login".into())
        );
        assert_eq!(model.review_group(&review), group("ABC-1"));
        let other = model.reviews(List::ToReview)[1].clone();
        assert_eq!(model.review_project(&other), "org/other");
        assert!(model.review_work(&other).is_none());
        assert_eq!(model.review_group(&other), None);
    }

    #[test]
    fn a_review_never_matches_another_repos_worktree_on_its_branch() {
        let mut model = with_reviews(model());
        model.snapshot.work[2].tree_mut().branch = Some("change-2".into());
        let review = model.reviews(List::ToReview)[0].clone();
        assert_eq!(review.branch, "change-2");
        assert!(
            model.review_work(&review).is_none(),
            "web:change-2 is not api's"
        );
        assert_eq!(model.review_group(&review), None);
    }

    /// The triage label scheme, with an issue in each section and in Other, one hidden, and a
    /// Jira issue whose key the ABC-1 worktrees link.
    pub fn with_issues(mut model: Model) -> Model {
        use crate::issues::tests::{SCHEME, issue};
        model.tracker_config = crate::config::Config::parse(SCHEME).unwrap().tracker;
        let mut jira = issue("ABC-1", &["ready-for-agent"], false);
        jira.tracker = crate::issues::Tracker::Jira;
        jira.project_url = None;
        update(
            &mut model,
            Action::Fetched {
                feed: Feed::Issues(crate::issues::Tracker::GitHub),
                rows: Ok(Rows::Issues(vec![
                    issue("api#1", &["ready-for-agent"], false),
                    issue("api#2", &["needs-triage"], false),
                    issue("api#3", &["ready-for-agent"], true),
                    issue("api#4", &[], false),
                    issue("api#5", &["wontfix"], false),
                    issue("api#6", &["enhancement"], false),
                ])),
                log: Vec::new(),
            },
        );
        update(
            &mut model,
            Action::Fetched {
                feed: Feed::Issues(crate::issues::Tracker::Jira),
                rows: Ok(Rows::Issues(vec![jira])),
                log: Vec::new(),
            },
        );
        model
    }

    fn issue_keys(model: &Model, section: usize) -> Vec<String> {
        (model.issues(List::Section(section)).iter())
            .map(|issue| issue.key.to_string())
            .collect()
    }

    #[test]
    fn issues_fill_the_sections_in_rule_order() {
        let mut model = with_issues(model());
        assert_eq!(issue_keys(&model, 0), ["api#3"], "blocked comes first");
        assert_eq!(
            issue_keys(&model, 1),
            ["api#1", "ABC-1"],
            "GitHub, then Jira"
        );
        assert_eq!(issue_keys(&model, 2), ["api#2"]);
        assert_eq!(
            issue_keys(&model, 3),
            ["api#4", "api#6"],
            "wontfix is hidden"
        );
        press(&mut model, "4");
        assert_eq!(model.active(), List::Section(0));
        press(&mut model, "]j");
        assert_eq!(model.active(), List::Section(1));
        assert_eq!(model.issue().unwrap().key, key("ABC-1"));
        press(&mut model, "[[");
        assert_eq!(model.active(), List::Section(3), "wraps around");
        press(&mut model, "]]/abc");
        assert_eq!(issue_keys(&model, 1), ["ABC-1"]);
    }

    #[test]
    fn other_shows_only_while_it_lists_issues() {
        let mut model = with_issues(model());
        assert_eq!(model.tabs(Panel::Issues).len(), 4, "Backlog takes the rest");
        model.tracker_config = crate::config::Config::parse(
            "[[tracker.sections]]\ntitle = \"Ready\"\nlabels = [\"ready-for-agent\"]\n",
        )
        .unwrap()
        .tracker;
        assert_eq!(model.tabs(Panel::Issues).len(), 2);
        press(&mut model, "4[");
        assert_eq!(model.title(model.active()), "Other");
        update(
            &mut model,
            Action::Fetched {
                feed: Feed::Issues(crate::issues::Tracker::GitHub),
                rows: Ok(Rows::Issues(vec![crate::issues::tests::issue(
                    "api#1",
                    &["ready-for-agent"],
                    false,
                )])),
                log: Vec::new(),
            },
        );
        assert_eq!(model.tabs(Panel::Issues).len(), 1);
        assert_eq!(
            model.active(),
            List::Section(0),
            "back to the first section"
        );
    }

    fn menu_labels(model: &Model) -> Vec<String> {
        match &model.modal {
            Some(Modal::Menu { entries, .. }) => {
                entries.iter().map(|entry| entry.label.clone()).collect()
            }
            _ => panic!("no menu"),
        }
    }

    #[test]
    fn space_on_an_issue_asks_for_a_repo_then_a_branch_or_opens_its_linked_work() {
        let mut model = with_issues(model());
        press(&mut model, "4]");
        assert!(press(&mut model, " ").is_empty());
        assert_eq!(
            menu_labels(&model),
            ["api", "web"],
            "its own repo first, never picked"
        );
        press(&mut model, "\n");
        let Some(Modal::Prompt { input, title, .. }) = &model.modal else {
            panic!("asks for the branch");
        };
        assert_eq!(input.value(), "1-issue-api-1");
        assert!(title.contains("api for api#1"), "{title}");
        let [
            Job::LinkedGroup(Pending::Start {
                repo,
                branch,
                workspace,
                issue,
            }),
        ] = &jobs(press(&mut model, "\n"))[..]
        else {
            panic!();
        };
        assert_eq!(
            (
                repo,
                branch.as_str(),
                workspace.as_str(),
                issue.key.as_str()
            ),
            (
                &PathBuf::from("/src/api"),
                "1-issue-api-1",
                "default",
                "api#1"
            )
        );
        press(&mut model, "j");
        press(&mut model, " ");
        assert_eq!(
            jobs(press(&mut model, "\n")),
            [Job::Open(vec![
                "/src/api.ABC-1-login".into(),
                "/src/web.ABC-1-form".into()
            ])],
            "the Jira issue's key links its work"
        );
        press(&mut model, "n");
        press(&mut model, "2");
        let Some(Modal::Prompt { then, .. }) = &model.modal else {
            panic!();
        };
        let Submit::Start {
            repo, workspace, ..
        } = then
        else {
            panic!();
        };
        assert_eq!(
            (repo, workspace.as_str()),
            (&PathBuf::from("/src/web"), "default"),
            "in the linked work's workspace, not web's own"
        );
        assert!(jobs(press(&mut model, "\x1b")).is_empty());
    }

    fn issue_plan(model: &Model) -> Vec<(String, bool)> {
        let Some(Modal::IssuePlan { plan, .. }) = &model.modal else {
            panic!("no plan: {:?}", model.modal);
        };
        (plan.lines.iter())
            .map(|line| match line {
                finish::Line::Step { label, checked, .. } => (label.clone(), *checked),
                finish::Line::Info { label, note } => (format!("{label}: {note}"), false),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// The api#2 and org/other#1 reviews link ABC-1, as does a review of api already checked out.
    fn reviewing_abc_1(model: Model) -> Model {
        let mut model = with_reviews(with_issues(model));
        model.snapshot.work[0].tree_mut().branch = Some("change-3".into());
        for review in &mut model.reviews {
            review.issue_keys = keys(&["ABC-1"]);
        }
        model
    }

    #[test]
    fn space_on_an_issue_lists_its_linked_work_checked_and_its_reviews_unchecked() {
        let mut model = reviewing_abc_1(model());
        press(&mut model, "4]j");
        assert!(press(&mut model, " ").is_empty());
        assert_eq!(
            issue_plan(&model),
            [
                ("open api:ABC-1-login".into(), true),
                ("open web:ABC-1-form".into(), true),
                ("check out api#2 (@alice)".into(), false),
                (
                    "check out org/other#1 (@alice): not registered".into(),
                    false
                ),
            ],
            "api#3 is checked out already"
        );
        press(&mut model, "jj ");
        let effects = jobs(press(&mut model, "\n"));
        let [
            Job::Open(paths),
            Job::LinkedGroup(Pending::Checkout { review, .. }),
        ] = &effects[..]
        else {
            panic!("{effects:?}");
        };
        assert_eq!(paths.len(), 2);
        assert_eq!(review.number, 2);
        assert!(model.modal.is_none());
    }

    #[test]
    fn an_issue_plan_runs_only_its_checked_lines_and_cancels() {
        let mut model = reviewing_abc_1(model());
        press(&mut model, "4]j ");
        press(&mut model, " j ");
        assert_eq!(jobs(press(&mut model, "\n")), [], "nothing checked");
        assert!(model.modal.is_none());
        press(&mut model, " ");
        assert!(jobs(press(&mut model, "\x1b")).is_empty());
        assert!(model.modal.is_none());
    }

    #[test]
    fn space_on_an_issue_with_only_a_review_lists_its_checkout() {
        let mut model = with_reviews(with_issues(model()));
        let api = (model.reviews.iter_mut()).find(|review| review.number == 2);
        api.unwrap().issue_keys = keys(&["api#1"]);
        press(&mut model, "4]");
        press(&mut model, " ");
        assert_eq!(
            issue_plan(&model),
            [("check out api#2 (@alice)".into(), false)]
        );
    }

    #[test]
    fn space_on_an_issue_with_nothing_to_list_asks_to_start_one() {
        let mut model = with_reviews(with_issues(model()));
        let other = (model.reviews.iter_mut()).find(|review| review.number == 1);
        other.unwrap().issue_keys = keys(&["api#1"]);
        press(&mut model, "4]");
        press(&mut model, " ");
        assert_eq!(
            menu_labels(&model),
            ["api", "web"],
            "an unregistered review checks nothing out"
        );
    }

    #[test]
    fn group_prompts_wait_for_the_one_open() {
        let mut model = with_reviews(model());
        let pendings: Vec<Pending> = (model.reviews.iter())
            .filter(|review| review.number < 3)
            .map(|review| Pending::Checkout {
                repo: "/src/api".into(),
                workspace: "default".into(),
                review: Box::new(review.clone()),
            })
            .collect();
        for pending in &pendings {
            let linked = Action::Linked {
                pending: pending.clone(),
                group: Ok(None),
                log: Vec::new(),
            };
            assert!(update(&mut model, linked).is_empty());
        }
        let first = prompt(&model).0;
        let [Job::Make { pending, .. }] = &jobs(press(&mut model, "a\n"))[..] else {
            panic!();
        };
        assert_eq!(pending, &pendings[0]);
        assert_ne!(prompt(&model).0, first, "the next one asks now");
        let [Job::Make { pending, group }] = &jobs(press(&mut model, "\n"))[..] else {
            panic!();
        };
        assert_eq!((pending, group), (&pendings[1], &None));
        assert!(model.modal.is_none());
    }

    #[test]
    fn the_repo_menu_suggests_the_linked_works_repo_before_the_issues_own() {
        let mut model = with_issues(model());
        // A tracker-only api: the work on api#1 happens in web.
        model.snapshot.work[3].links.issue_keys = keys(&["api#1"]);
        press(&mut model, "4]n");
        assert_eq!(menu_labels(&model), ["web", "api"]);
        press(&mut model, "\n");
        let Some(Modal::Prompt {
            then: Submit::Start { workspace, .. },
            ..
        }) = &model.modal
        else {
            panic!();
        };
        assert_eq!(workspace, "side", "the linked work's workspace");
    }

    #[test]
    fn a_new_worktree_joins_the_linked_work_in_its_repo() {
        let mut model = with_issues(model());
        model.snapshot.work[2].workspace = "side".into();
        let workspace_for = |model: &mut Model, entry: &str| {
            press(model, "n");
            press(model, entry);
            let Some(Modal::Prompt {
                then: Submit::Start { workspace, .. },
                ..
            }) = model.modal.take()
            else {
                panic!();
            };
            workspace
        };
        press(&mut model, "4]j");
        assert_eq!(model.issue().unwrap().key, key("ABC-1"));
        assert_eq!(
            workspace_for(&mut model, "1"),
            "default",
            "api's linked work"
        );
        assert_eq!(workspace_for(&mut model, "2"), "side", "web's linked work");
    }

    #[test]
    fn issues_browse_and_ignore_local_actions() {
        let mut model = with_issues(model());
        press(&mut model, "4]");
        assert_eq!(
            jobs(press(&mut model, "o")),
            [Job::Browse("https://forge/api/issues/api#1".into())]
        );
        for keys in ["d", "e", "m", "p"] {
            assert!(jobs(press(&mut model, keys)).is_empty(), "{keys}");
            assert!(model.modal.is_none(), "{keys}");
        }
        press(&mut model, "?");
        let keys = menu_keys(&model);
        assert!(keys.contains(&"Space".into()) && keys.contains(&"o".into()));
        assert!(!keys.contains(&"p".into()));
    }

    #[test]
    fn clicking_a_row_focuses_and_selects_it() {
        let mut model = model();
        let areas = areas(&model, Rect::new(0, 0, 120, 40));
        let (_, work) = areas.panels[1];
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: work.x + 2,
            row: work.y + 3,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut model, Action::Mouse(click));
        assert_eq!(model.index(List::Work), 2);
        let (_, workspaces) = areas.panels[0];
        let click = MouseEvent {
            row: workspaces.y + 2,
            ..click
        };
        update(&mut model, Action::Mouse(click));
        assert_eq!(model.focus, Focus::Panel(Panel::Workspaces));
        assert_eq!(model.workspace(), Some("side"));
    }

    #[test]
    fn carnets_follow_the_worktrees_of_their_group() {
        let mut model = with_carnets(model());
        assert_eq!(
            titles(&model),
            [
                "[ABC-1]",
                "api:ABC-1-login",
                "web:ABC-1-form",
                "2026-10-01-ABC-1-logs",
                "api:main",
                "2026-10-02-ideas",
                "2026-09-20-old",
            ],
            "ungrouped carnets list with ungrouped worktrees, after them, newest first"
        );
        press(&mut model, "-");
        assert_eq!(
            titles(&model),
            ["[ABC-1]", "api:main", "2026-10-02-ideas", "2026-09-20-old"]
        );
        press(&mut model, "=");
        assert_eq!(titles(&model).len(), 7);
        press(&mut model, ">n");
        press(&mut model, "c");
        press(&mut model, "x\n");
        press(&mut model, "\n");
        assert_eq!(prompt(&model), ("New carnet: group".into(), String::new()));
        assert_eq!(
            jobs(press(&mut model, "\n\n")),
            [Job::NewCarnet {
                name: "x".into(),
                workspace: "default".into(),
                links: Links::default(),
                summary: "x".into(),
            }],
            "an ungrouped carnet's group is none"
        );
    }

    #[test]
    fn a_carnets_readme_is_read_when_selected_and_again_once_written() {
        let mut model = with_carnets(model());
        press(&mut model, ">kk");
        let path = PathBuf::from("/data/2026-10-02-ideas");
        let any_reads = |effects: &[Effect]| {
            (effects.iter())
                .filter(|effect| matches!(effect, Effect::Run(Job::Readme(_))))
                .count()
        };
        let reads = |effects: &[Effect]| {
            (effects.iter())
                .filter(|effect| matches!(effect, Effect::Run(Job::Readme(read)) if *read == path))
                .count()
        };
        let effects = press(&mut model, "j");
        assert!(
            effects.contains(&Effect::Run(Job::Readme(path.clone()))),
            "{effects:?}"
        );
        let readme = |text: &str, len| Readme {
            path: path.clone(),
            stamp: Some(Stamp {
                len,
                modified: None,
            }),
            text: Some(text.into()),
        };
        let refresh = |model: &mut Model, len| {
            let mut snapshot = model.snapshot.clone();
            let listed = snapshot.work.iter_mut().chain(&mut snapshot.carnets);
            for work in listed.filter(|work| work.path == path) {
                if let WorkKind::Carnet { readme, .. } = &mut work.kind {
                    *readme = Some(Stamp {
                        len,
                        modified: None,
                    });
                }
            }
            model.schedule.finish_all();
            let log = Vec::new();
            let snapshot = Ok(snapshot);
            let action = Action::Loaded {
                snapshot,
                full: false,
                log,
            };
            update(model, action)
        };
        assert_eq!(reads(&refresh(&mut model, 1)), 1);
        update(&mut model, Action::Readme(readme("# ideas", 1)));
        assert_eq!(reads(&press(&mut model, "jk")), 0, "read once");
        assert_eq!(reads(&refresh(&mut model, 1)), 0, "unchanged");
        assert_eq!(reads(&refresh(&mut model, 2)), 1, "written");
        assert!(model.readme.is_some(), "shown until read again");
        press(&mut model, "j");
        update(&mut model, Action::Readme(readme("# late", 2)));
        assert_ne!(
            model
                .readme
                .as_ref()
                .and_then(|readme| readme.text.as_deref()),
            Some("# late"),
            "a read for a carnet since left is dropped"
        );
        press(&mut model, "<");
        assert_eq!(
            any_reads(&press(&mut model, "j")),
            0,
            "a worktree has no README"
        );
    }

    #[test]
    fn fast_refreshes_relist_the_selected_carnets_commits_and_show_them_meanwhile() {
        let mut model = with_carnets(model());
        press(&mut model, ">kk");
        let effects = press(&mut model, "j");
        let path = PathBuf::from("/data/2026-10-02-ideas");
        assert!(
            effects.contains(&Effect::Run(Job::Commits(path.clone()))),
            "{effects:?}"
        );
        let commits = vec![Commit::fake("abc1234", "Note the first lead")];
        update(&mut model, Action::Commits(path.clone(), commits.clone()));
        model.schedule.finish_all();
        let snapshot = Ok(model.snapshot.clone());
        let log = Vec::new();
        let effects = update(
            &mut model,
            Action::Loaded {
                snapshot,
                full: false,
                log,
            },
        );
        assert!(
            effects.contains(&Effect::Run(Job::Commits(path.clone()))),
            "{effects:?}"
        );
        assert_eq!(model.commits.get(&path), Some(&commits));
    }

    #[test]
    fn an_item_linking_an_issues_key_is_its_linked_work() {
        let mut model = with_issues(with_carnets(model()));
        let mut linked = carnet("2026-07-01-notes", "OTHER", "side");
        linked.links.issue_keys = keys(&["XYZ-1", "api#4"]);
        if let WorkKind::Carnet { closed, .. } = &mut linked.kind {
            *closed = true;
        }
        model.snapshot.carnets.push(linked);
        let issue = |key: &str| {
            let issue = model.issues.iter().find(|issue| issue.key.as_str() == key);
            issue.unwrap().clone()
        };
        let titles = |issue| {
            (model.issue_work(&issue).iter())
                .map(|work| work.title())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            titles(issue("api#4")),
            ["2026-07-01-notes"],
            "a closed carnet, by a later key, in another group"
        );
        assert_eq!(
            titles(issue("ABC-1")),
            ["api:ABC-1-login", "web:ABC-1-form", "2026-10-01-ABC-1-logs"],
            "an open carnet once, after the worktrees"
        );
    }

    #[test]
    fn c_closes_the_selected_carnets() {
        let mut model = with_carnets(model());
        assert_eq!(
            jobs(press(&mut model, "c")),
            [Job::CloseCarnet(vec!["/data/2026-10-01-ABC-1-logs".into()])],
            "every carnet of the group"
        );
        press(&mut model, ">");
        assert_eq!(
            jobs(press(&mut model, "c")),
            [Job::CloseCarnet(vec!["/data/2026-09-20-old".into()])],
            "the selected carnet"
        );
        press(&mut model, "<");
        assert_eq!(
            jobs(press(&mut model, "c")),
            [Job::CloseCarnet(vec!["/data/2026-10-01-ABC-1-logs".into()])],
            "only the carnets of a group with worktrees"
        );
        press(&mut model, "j");
        assert!(jobs(press(&mut model, "c")).is_empty(), "a worktree");
    }

    #[test]
    fn new_offers_a_carnet_in_the_selections_group_when_carnets_are_on() {
        let mut model = with_carnets(model());
        press(&mut model, "jn");
        assert_eq!(menu_labels(&model), ["worktree of api", "carnet"]);
        press(&mut model, "c");
        assert!(matches!(&model.modal, Some(Modal::Prompt { .. })));
        assert_eq!(
            prompt(&model),
            ("New carnet: summary".into(), String::new())
        );
        press(&mut model, "\n");
        assert_eq!(
            prompt(&model),
            ("New carnet: folder name".into(), String::new()),
            "no summary, no name to suggest"
        );
        assert!(jobs(press(&mut model, "-/\n")).is_empty(), "no name");
        assert_eq!(
            prompt(&model),
            (
                "New carnet: folder name, with a letter or a digit".into(),
                "-/".into()
            ),
            "asked again, as typed"
        );
        press(&mut model, "\x1b");
        press(&mut model, "nc");
        press(&mut model, "Login fails: after token-refresh, again\n");
        assert_eq!(
            prompt(&model),
            (
                "New carnet: folder name".into(),
                "login-fails-after-token-refresh".into()
            ),
            "the summary's first five words, punctuation dropped"
        );
        press(&mut model, "\n");
        assert_eq!(
            prompt(&model),
            ("New carnet: group".into(), "ABC-1".into()),
            "the selection's group"
        );
        press(&mut model, "\n");
        assert_eq!(
            prompt(&model),
            ("New carnet: issue keys".into(), String::new())
        );
        assert_eq!(
            jobs(press(&mut model, "ABC-1, DEF-2\n")),
            [Job::NewCarnet {
                name: "login-fails-after-token-refresh".into(),
                workspace: "default".into(),
                links: crate::links::tests::links("ABC-1", &["ABC-1", "DEF-2"]),
                summary: "Login fails: after token-refresh, again".into(),
            }]
        );
        press(&mut model, "nc");
        press(&mut model, "x\n");
        assert!(
            jobs(press(&mut model, "\x1b")).is_empty() && model.modal.is_none(),
            "Esc at any step makes nothing"
        );
        press(&mut model, "n1");
        let Some(Modal::Prompt { title, .. }) = &model.modal else {
            panic!("no branch prompt");
        };
        assert!(title.contains("of api"), "{title}");
        press(&mut model, "\x1b>n");
        assert_eq!(
            menu_labels(&model),
            ["worktree of api", "worktree of web", "carnet"],
            "a carnet has no repo to suggest"
        );
    }

    #[test]
    fn pull_skips_carnets() {
        let mut model = with_carnets(model());
        assert_eq!(
            jobs(press(&mut model, "p")),
            [Job::Pull(vec![
                "/src/api.ABC-1-login".into(),
                "/src/web.ABC-1-form".into()
            ])]
        );
        press(&mut model, ">");
        assert!(jobs(press(&mut model, "p")).is_empty());
    }

    #[test]
    fn carnets_are_never_removed() {
        let mut model = with_carnets(model());
        press(&mut model, ">d");
        assert!(model.modal.is_none());
        assert!(
            model
                .log
                .last()
                .unwrap()
                .command
                .contains("carnets are never removed")
        );
    }

    fn carnet_titles(model: &Model) -> Vec<String> {
        model
            .carnet_rows()
            .iter()
            .map(|work| work.title())
            .collect()
    }

    #[test]
    fn the_carnets_sub_tab_lists_every_carnet_while_carnets_are_on() {
        let mut model = model();
        press(&mut model, "]");
        assert_eq!(model.active(), List::Work, "no sub-tab without carnets");
        let mut model = with_carnets(model);
        press(&mut model, "]");
        assert_eq!(model.active(), List::Carnets);
        assert_eq!(
            carnet_titles(&model),
            [
                "2026-10-02-ideas",
                "2026-10-01-ABC-1-logs",
                "2026-09-20-old",
                "2026-08-01-done"
            ]
        );
        press(&mut model, "/done\n");
        assert_eq!(carnet_titles(&model), ["2026-08-01-done"]);
    }

    #[test]
    fn space_on_a_closed_carnet_opens_it_and_leaves_it_closed() {
        let mut model = with_carnets(model());
        press(&mut model, "]>");
        assert_eq!(
            jobs(press(&mut model, " ")),
            [Job::Open(vec!["/data/2026-08-01-done".into()])]
        );
    }

    #[test]
    fn c_closes_an_open_carnet_and_reopens_a_closed_one() {
        let mut model = with_carnets(model());
        press(&mut model, "]");
        assert_eq!(
            jobs(press(&mut model, "c")),
            [Job::CloseCarnet(vec!["/data/2026-10-02-ideas".into()])]
        );
        press(&mut model, ">");
        assert_eq!(
            jobs(press(&mut model, "c")),
            [Job::ReopenCarnet(vec!["/data/2026-08-01-done".into()])]
        );
    }

    #[test]
    fn s_searches_inside_carnets_and_esc_clears_the_search() {
        let mut model = with_carnets(model());
        assert!(jobs(press(&mut model, "s")).is_empty(), "Carnets only");
        assert!(model.modal.is_none());
        press(&mut model, "]s");
        assert!(matches!(&model.modal, Some(Modal::Prompt { .. })));
        assert_eq!(
            jobs(press(&mut model, "bug\n")),
            [Job::SearchCarnets("bug".into())]
        );
        let path = PathBuf::from("/data/2026-09-20-old");
        let effects = update(
            &mut model,
            Action::Searched {
                text: "bug".into(),
                hits: Some([(path.clone(), vec!["README.md:3:a bug".into()])].into()),
                log: Vec::new(),
            },
        );
        assert_eq!(carnet_titles(&model), ["2026-09-20-old"]);
        assert!(
            effects.contains(&Effect::Run(Job::Commits(path))),
            "{effects:?}"
        );
        assert_eq!(
            model.carnet_hits(),
            Some(&vec!["README.md:3:a bug".to_owned()])
        );
        press(&mut model, "\x1b");
        assert_eq!(carnet_titles(&model).len(), 4);
        assert!(model.search.is_none());
    }

    /// The open prompt's title and text.
    fn prompt(model: &Model) -> (String, String) {
        let Some(Modal::Prompt { title, input, .. }) = &model.modal else {
            panic!("no prompt: {:?}", model.modal);
        };
        (title.clone(), input.value().to_owned())
    }

    /// The ABC-1 group spread over both workspaces, with a closed carnet in it.
    fn spread() -> Model {
        let mut model = with_carnets(model());
        model
            .snapshot
            .work
            .push(work("web", "ABC-1-side", "ABC-1", "side"));
        let mut closed = carnet("2026-07-01-gone", "ABC-1", "side");
        if let WorkKind::Carnet { closed, .. } = &mut closed.kind {
            *closed = true;
        }
        model.snapshot.carnets.push(closed);
        model
    }

    #[test]
    fn e_on_a_row_moves_that_item_to_a_group_or_out_of_any() {
        let mut model = spread();
        press(&mut model, "j");
        press(&mut model, "eg");
        assert_eq!(
            prompt(&model),
            ("Group of api:ABC-1-login".into(), "ABC-1".into())
        );
        press(&mut model, "\x1b");
        press(&mut model, "eg");
        press(&mut model, &"\x08".repeat("ABC-1".len()));
        assert_eq!(
            jobs(press(&mut model, " slow pages\n")),
            [Job::Regroup {
                paths: vec!["/src/api.ABC-1-login".into()],
                group: Group::parse("SLOW PAGES"),
            }]
        );
        press(&mut model, "jjeg");
        assert_eq!(
            prompt(&model).0,
            "Group of 2026-10-01-ABC-1-logs",
            "a carnet too"
        );
        press(&mut model, "\x1b");
        press(&mut model, ">eg");
        let (_, text) = prompt(&model);
        assert_eq!(text, "", "an ungrouped item");
    }

    #[test]
    fn e_on_a_header_renames_the_group_in_every_workspace() {
        let mut model = spread();
        press(&mut model, "e");
        assert_eq!(
            prompt(&model),
            ("Rename group ABC-1".into(), "ABC-1".into())
        );
        let [Job::Regroup { paths, group }] = &jobs(press(&mut model, "x\n"))[..] else {
            panic!();
        };
        assert_eq!(group, &Group::parse("ABC-1X"));
        let mut paths = paths.clone();
        paths.sort();
        let expected: Vec<PathBuf> = [
            "/data/2026-07-01-gone",
            "/data/2026-10-01-ABC-1-logs",
            "/src/api.ABC-1-login",
            "/src/web.ABC-1-form",
            "/src/web.ABC-1-side",
        ]
        .map(PathBuf::from)
        .into();
        assert_eq!(
            paths, expected,
            "closed carnets and other workspaces included, once each"
        );
    }

    #[test]
    fn group_prompts_complete_from_every_existing_group_once() {
        let mut model = spread();
        model
            .snapshot
            .work
            .push(work("api", "x", "slow pages", "side"));
        assert_eq!(
            model.groups(),
            [
                Group::parse("ABC-1").unwrap(),
                Group::parse("SLOW PAGES").unwrap()
            ]
        );
        press(&mut model, ">eg");
        press(&mut model, "s\t");
        assert_eq!(prompt(&model).1, "SLOW PAGES");
        press(&mut model, "\x1b>eg\t");
        assert_eq!(
            prompt(&model).1,
            "ABC-1",
            "an empty prompt cycles through all"
        );
        press(&mut model, "\t");
        assert_eq!(prompt(&model).1, "SLOW PAGES");
        press(&mut model, "\t");
        assert_eq!(prompt(&model).1, "ABC-1");
        press(&mut model, "\x1b>eg");
        press(&mut model, "zz\t");
        assert_eq!(prompt(&model).1, "zz", "nothing to complete");
    }

    #[test]
    fn e_on_an_item_edits_its_group_or_its_issue_keys() {
        let mut model = spread();
        model.tracker_config =
            (crate::config::Config::parse("[tracker.github]\nrepos = [\"o/api\"]"))
                .unwrap()
                .tracker;
        press(&mut model, "je");
        assert_eq!(menu_labels(&model), ["group", "issue keys"]);
        press(&mut model, "i");
        assert_eq!(
            prompt(&model),
            ("Issue keys of api:ABC-1-login".into(), "ABC-1".into())
        );
        assert_eq!(
            jobs(press(&mut model, ", api#7, ABC-1, DEF-2\n")),
            [Job::SetIssueKeys {
                path: "/src/api.ABC-1-login".into(),
                issue_keys: keys(&["ABC-1", "o/api#7", "DEF-2"]),
            }],
            "short GitHub keys resolve; order kept, duplicates dropped"
        );
        press(&mut model, "eg");
        assert_eq!(prompt(&model).0, "Group of api:ABC-1-login");
        press(&mut model, "\x1b]ei");
        assert_eq!(
            prompt(&model).0,
            "Issue keys of 2026-10-02-ideas",
            "a carnet, in the Carnets list"
        );
        let effects = press(&mut model, "\n");
        assert_eq!(
            jobs(effects),
            [Job::SetIssueKeys {
                path: "/data/2026-10-02-ideas".into(),
                issue_keys: keys(&[]),
            }],
            "an empty list unlinks every key"
        );
        press(&mut model, "e");
        assert_eq!(menu_labels(&model), ["group", "issue keys", "summary"]);
        press(&mut model, "g");
        assert_eq!(prompt(&model).0, "Group of 2026-10-02-ideas");
        press(&mut model, "\x1bes");
        assert_eq!(
            prompt(&model),
            ("Summary of 2026-10-02-ideas".into(), String::new())
        );
        assert_eq!(
            jobs(press(&mut model, "Found it\n")),
            [Job::SetSummary {
                path: "/data/2026-10-02-ideas".into(),
                summary: "Found it".into(),
            }]
        );
    }

    #[test]
    fn m_in_the_carnets_list_moves_a_carnet_closed_or_not() {
        let mut model = with_carnets(model());
        press(&mut model, "]jjjm");
        assert_eq!(menu_labels(&model), ["default"], "the closed one, in side");
        assert_eq!(
            jobs(press(&mut model, "1")),
            [Job::Move {
                paths: vec!["/data/2026-08-01-done".into()],
                workspace: "default".into(),
            }]
        );
    }

    #[test]
    fn e_on_a_group_header_renames_it_at_once() {
        let mut model = spread();
        press(&mut model, "e");
        assert!(matches!(model.modal, Some(Modal::Prompt { .. })), "no menu");
    }

    #[test]
    fn a_group_header_has_no_issue_keys_to_edit() {
        let mut model = spread();
        press(&mut model, "e");
        assert_eq!(prompt(&model).0, "Rename group ABC-1");
        let [Job::Regroup { group, .. }] = &jobs(press(&mut model, "i\n"))[..] else {
            panic!("`i` is typed into the rename, not an issue keys entry");
        };
        assert_eq!(group, &Group::parse("ABC-1I"));
    }

    #[test]
    fn h_and_l_move_between_panels_as_in_lazygit() {
        let mut model = spread();
        assert_eq!(model.panel, Panel::Work);
        press(&mut model, "l");
        assert_eq!(model.panel, Panel::Reviews);
        press(&mut model, "h");
        assert_eq!(model.panel, Panel::Work);
    }

    /// Answers the pending group lookup that `effects` started with `group`.
    fn linked(model: &mut Model, effects: Vec<Effect>, group: Option<&str>) -> Vec<Effect> {
        let [Job::LinkedGroup(pending)] = &jobs(effects)[..] else {
            panic!("no group lookup");
        };
        update(
            model,
            Action::Linked {
                pending: pending.clone(),
                group: Ok(group.and_then(Group::parse)),
                log: Vec::new(),
            },
        )
    }

    fn started_group(effects: Vec<Effect>) -> Option<Group> {
        let [
            Job::Make {
                pending: Pending::Start { .. },
                group,
            },
        ] = &jobs(effects)[..]
        else {
            panic!("no start");
        };
        group.clone()
    }

    #[test]
    fn starting_an_issue_joins_its_one_linked_group_else_asks_for_one() {
        let mut model = with_issues(model());
        press(&mut model, "4] \n");
        let branch = press(&mut model, "\n");
        assert_eq!(
            started_group(linked(&mut model, branch.clone(), Some("login"))),
            Group::parse("LOGIN"),
            "one linked group: joined without asking"
        );
        assert!(linked(&mut model, branch.clone(), None).is_empty());
        let (title, text) = prompt(&model);
        assert_eq!(
            (title.as_str(), text.as_str()),
            ("Group of the worktree for api#1 (empty: none)", ""),
            "no prefill"
        );
        assert_eq!(started_group(press(&mut model, "\n")), None);
        linked(&mut model, branch, None);
        assert_eq!(
            started_group(press(&mut model, "web\n")),
            Group::parse("WEB")
        );
    }

    #[test]
    fn a_failed_group_lookup_still_asks_for_the_group() {
        let mut model = with_issues(model());
        press(&mut model, "4] \n");
        let [Job::LinkedGroup(pending)] = &jobs(press(&mut model, "\n"))[..] else {
            panic!("no group lookup");
        };
        let failed = Action::Linked {
            pending: pending.clone(),
            group: Err("no database".into()),
            log: Vec::new(),
        };
        assert!(update(&mut model, failed).is_empty());
        assert_eq!(
            model.log.last().unwrap().error.as_deref(),
            Some("no database")
        );
        assert!(
            prompt(&model)
                .0
                .starts_with("Group of the worktree for api#1")
        );
        assert_eq!(started_group(press(&mut model, "x\n")), Group::parse("X"));
    }

    #[test]
    fn starting_on_a_branch_already_checked_out_asks_nothing() {
        let mut model = with_issues(model());
        press(&mut model, "4]n1");
        press(&mut model, &"\x08".repeat("1-issue-api-1".len()));
        let effects = press(&mut model, "ABC-1-login\n");
        assert_eq!(started_group(effects), None, "the worktree keeps its group");
    }

    #[test]
    fn checking_out_a_review_joins_its_one_linked_group_else_asks_for_one() {
        let mut model = with_reviews(model());
        press(&mut model, "3");
        let checkout = press(&mut model, " ");
        let [
            Job::Make {
                pending: Pending::Checkout { .. },
                group,
            },
        ] = &jobs(linked(&mut model, checkout.clone(), Some("a")))[..]
        else {
            panic!();
        };
        assert_eq!(group, &Group::parse("A"));
        linked(&mut model, checkout, None);
        assert_eq!(
            prompt(&model).0,
            "Group of the worktree for api#2 (empty: none)"
        );
        let [
            Job::Make {
                pending: Pending::Checkout { .. },
                group,
            },
        ] = &jobs(press(&mut model, "b\n"))[..]
        else {
            panic!();
        };
        assert_eq!(group, &Group::parse("B"));
        model.snapshot.work[0].tree_mut().branch = Some("change-2".into());
        let [
            Job::Make {
                pending: Pending::Checkout { .. },
                group: None,
            },
        ] = &jobs(press(&mut model, " "))[..]
        else {
            panic!("checked out already: no lookup");
        };
    }
}
