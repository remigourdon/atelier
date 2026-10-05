//! `update(model, action) -> effects`: every state change, with no I/O.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use super::app::{
    Action, Binding, Cmd, Due, Effect, FAST_REFRESH, FULL_REFRESH, Focus, Job, KEYMAP, List,
    MenuEntry, Modal, Model, On, Panel, Popup, PopupCmd, Row, Screen, Snapshot, Source, Submit,
    Work, WorkKind, lookup, popup_lookup,
};
use super::lists;
use super::view::{areas, main_len, offset};
use crate::process::Logged;
use crate::reviews::Provider;
use crate::worktrunk;

pub fn update(model: &mut Model, action: Action) -> Vec<Effect> {
    match action {
        Action::Key(key) => {
            model.idle = 0;
            key_press(model, key)
        }
        Action::Mouse(mouse) => {
            model.idle = 0;
            mouse_event(model, mouse)
        }
        Action::Resize(width, height) => {
            model.size = (width, height);
            Vec::new()
        }
        Action::Tick => tick(model),
        Action::Cmd(cmd) => command(model, cmd),
        Action::Run(job) => vec![run(model, job)],
        Action::Ask {
            title,
            initial,
            then,
        } => {
            model.modal = Some(Modal::Prompt {
                title,
                input: Input::new(initial),
                then,
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
        Action::Loaded {
            snapshot,
            full,
            log,
        } => {
            done(model, Source::Wt);
            model.push_log(log);
            match snapshot {
                Ok(snapshot) => {
                    let keep = Keep::of(model);
                    let old = std::mem::replace(&mut model.snapshot, snapshot);
                    model.loaded = true;
                    keep.restore(model);
                    drop_stale(model, &old, full);
                    let mut effects = commits(model);
                    effects.extend(fetch_reviews(model));
                    effects.extend(fetch_issues(model));
                    effects
                }
                Err(error) => {
                    model.push_log([Logged {
                        command: "refresh".into(),
                        error: Some(error),
                    }]);
                    Vec::new()
                }
            }
        }
        Action::Commits(path, lines) => {
            done(model, Source::Git);
            model.commits.insert(path, lines);
            Vec::new()
        }
        Action::Readme(path, readme) => {
            done(model, Source::Git);
            model.readmes.insert(path, readme);
            Vec::new()
        }
        Action::Reviews {
            provider,
            reviews,
            log,
        } => {
            done(model, Source::Reviews(provider));
            model.push_log(log);
            match reviews {
                Ok(reviews) => {
                    let keep = Keep::of(model);
                    model.reviews.retain(|review| review.provider != provider);
                    model.reviews.extend(reviews);
                    model
                        .reviews
                        .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                    keep.restore(model);
                }
                Err(error) => model.push_log([Logged {
                    command: format!("{} reviews", provider.cli()),
                    error: Some(error),
                }]),
            }
            Vec::new()
        }
        Action::Issues {
            tracker,
            issues,
            log,
        } => {
            done(model, Source::Issues(tracker));
            model.push_log(log);
            match issues {
                Ok(issues) => {
                    let keep = Keep::of(model);
                    model.issues.retain(|issue| issue.tracker != tracker);
                    model.issues.extend(issues);
                    model.issues.sort_by_key(|issue| issue.tracker);
                    keep.restore(model);
                }
                Err(error) => model.push_log([Logged {
                    command: format!("{} issues", tracker.cli()),
                    error: Some(error),
                }]),
            }
            Vec::new()
        }
        Action::Finished { job, log, error } => {
            done(model, job.source());
            if let Job::Pull(paths) = &job {
                for path in paths {
                    model.pulling.remove(path);
                }
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
            vec![run(model, Job::Refresh { full: false })]
        }
    }
}

/// Starts a job, showing its loading indicator.
pub(super) fn run(model: &mut Model, job: Job) -> Effect {
    *model.loading.entry(job.source()).or_default() += 1;
    if let Job::Pull(paths) = &job {
        model.pulling.extend(paths.iter().cloned());
    }
    if let Job::Refresh { full } = job {
        model.since_refresh = 0;
        if full {
            model.since_full = 0;
            for due in [&mut model.reviews_due, &mut model.issues_due] {
                if *due == Due::No {
                    *due = Due::Cached;
                }
            }
        }
    }
    Effect::Run(job)
}

/// Lists each provider's reviews on the hosts of the registered repos, once a listing that
/// asked for them has found those hosts.
fn fetch_reviews(model: &mut Model) -> Vec<Effect> {
    let due = std::mem::replace(&mut model.reviews_due, Due::No);
    if due == Due::No {
        return Vec::new();
    }
    let mut jobs = Vec::new();
    for provider in Provider::ALL {
        let mut hosts: Vec<String> = (model.snapshot.forges.values())
            .filter(|forge| Provider::from_name(&forge.provider) == Some(provider))
            .filter_map(|forge| worktrunk::host(&forge.url).map(Into::into))
            .collect();
        hosts.sort();
        hosts.dedup();
        if hosts.is_empty() {
            model.reviews.retain(|review| review.provider != provider);
        } else {
            jobs.push(Job::Reviews {
                provider,
                hosts,
                force: due == Due::Fresh,
            });
        }
    }
    let (effects, due) = run_due(model, due, jobs);
    model.reviews_due = due;
    effects
}

/// Lists each tracker's issues in its configured scopes, once a listing asked for them.
fn fetch_issues(model: &mut Model) -> Vec<Effect> {
    let due = std::mem::replace(&mut model.issues_due, Due::No);
    if due == Due::No {
        return Vec::new();
    }
    let jobs = (model.tracker_config.scopes().into_iter())
        .map(|(tracker, scopes)| Job::Issues {
            tracker,
            scopes,
            force: due == Due::Fresh,
        })
        .collect();
    let (effects, due) = run_due(model, due, jobs);
    model.issues_due = due;
    effects
}

/// Starts the due listings whose source is idle, and returns what is still due: a source still
/// listing gets its turn at the next listing, so `R` is not lost.
fn run_due(model: &mut Model, due: Due, jobs: Vec<Job>) -> (Vec<Effect>, Due) {
    let mut effects = Vec::new();
    let mut busy = false;
    for job in jobs {
        if model.loading.contains_key(&job.source()) {
            busy = true;
        } else {
            effects.push(run(model, job));
        }
    }
    (effects, if busy { due } else { Due::No })
}

fn done(model: &mut Model, source: Source) {
    if let Some(count) = model.loading.get_mut(&source) {
        *count -= 1;
        if *count == 0 {
            model.loading.remove(&source);
        }
    }
}

fn tick(model: &mut Model) -> Vec<Effect> {
    model.frame = model.frame.wrapping_add(1);
    model.idle += 1;
    model.since_refresh += 1;
    model.since_full += 1;
    if model.loading.contains_key(&Source::Wt) {
        return Vec::new();
    }
    if model.since_full >= FULL_REFRESH {
        vec![run(model, Job::Refresh { full: true })]
    } else if model.idle >= FAST_REFRESH && model.since_refresh >= FAST_REFRESH {
        vec![run(model, Job::Refresh { full: false })]
    } else {
        Vec::new()
    }
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

/// Drops what a refresh made stale: everything on a full refresh, else what was loaded for
/// items no longer listed, the commits of worktrees whose head moved, and the README and
/// commits of carnets whose README changed.
fn drop_stale(model: &mut Model, old: &Snapshot, full: bool) {
    if full {
        model.commits.clear();
        model.readmes.clear();
        return;
    }
    let head = |work: &Work| work.tree().map(|tree| tree.short_sha.clone());
    let heads: HashMap<&Path, Option<String>> = (model.snapshot.work.iter())
        .map(|work| (work.path.as_path(), head(work)))
        .collect();
    let rewritten = (model.snapshot.work.iter()).filter(|work| match &work.kind {
        WorkKind::Carnet { readme, .. } => {
            (model.readmes.get(&work.path)).is_some_and(|loaded| loaded != readme)
        }
        WorkKind::Worktree { .. } => false,
    });
    let moved: HashSet<&Path> = (old.work.iter())
        .filter(|work| {
            heads
                .get(work.path.as_path())
                .is_some_and(|now| *now != head(work))
        })
        .chain(rewritten)
        .map(|work| work.path.as_path())
        .collect();
    (model.commits)
        .retain(|path, _| heads.contains_key(path.as_path()) && !moved.contains(path.as_path()));
    (model.readmes)
        .retain(|path, _| heads.contains_key(path.as_path()) && !moved.contains(path.as_path()));
}

/// Fetches the selected item's recent commits, and a carnet's README, unless they are loaded.
fn commits(model: &mut Model) -> Vec<Effect> {
    let mut effects = Vec::new();
    if let Some(Row::Item(index)) = model.work_row() {
        let work = &model.snapshot.work[index];
        let (path, carnet) = (work.path.clone(), work.is_carnet());
        if carnet && !model.readmes.contains_key(&path) {
            effects.push(run(model, Job::Readme(path.clone())));
        }
        if !model.commits.contains_key(&path) {
            effects.push(run(model, Job::Commits(path)));
        }
    }
    effects
}

fn key_press(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return command(model, Cmd::Quit);
    }
    if let Some(modal) = model.modal.take() {
        return modal_key(model, modal, key);
    }
    if let Some(list) = model.filtering {
        return filter_key(model, list, key);
    }
    // `gg` is a two-key sequence, so it is matched here; KEYMAP's top binding lists it.
    let plain_g = key.code == KeyCode::Char('g') && !key.modifiers.contains(KeyModifiers::CONTROL);
    if plain_g {
        model.pending_g = !model.pending_g;
        return if model.pending_g {
            Vec::new()
        } else {
            command(model, Cmd::Top)
        };
    }
    model.pending_g = false;
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
        } => match popup_lookup(Popup::Prompt, &key) {
            Some(PopupCmd::Accept) => submit(model, then, input.value().trim()),
            Some(PopupCmd::Cancel) => Vec::new(),
            _ => {
                input.handle_event(&Event::Key(key));
                model.modal = Some(Modal::Prompt { title, input, then });
                Vec::new()
            }
        },
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
        } => {
            let last = entries.len().saturating_sub(1);
            match popup_lookup(Popup::Menu, &key) {
                Some(PopupCmd::Accept) => {
                    let action = entries[selected].action.clone();
                    return update(model, action);
                }
                Some(PopupCmd::Cancel) => return Vec::new(),
                Some(PopupCmd::Down) => selected = (selected + 1).min(last),
                Some(PopupCmd::Up) => selected = selected.saturating_sub(1),
                Some(PopupCmd::Top) => selected = 0,
                Some(PopupCmd::Bottom) => selected = last,
                None => {
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
            });
            Vec::new()
        }
    }
}

fn submit(model: &mut Model, then: Submit, value: &str) -> Vec<Effect> {
    let job = match then {
        _ if value.is_empty()
            && matches!(
                then,
                Submit::Branch { .. }
                    | Submit::Start { .. }
                    | Submit::Carnet { .. }
                    | Submit::Workspace
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
        } => Job::Start {
            repo,
            branch: value.into(),
            workspace,
            issue,
        },
        Submit::Carnet { workspace, group } => Job::NewCarnet {
            name: value.into(),
            workspace,
            group,
        },
        Submit::Group(paths) => Job::Regroup {
            paths,
            group: value.into(),
        },
        Submit::Alias(repo) => Job::SetAlias {
            repo,
            alias: value.into(),
        },
        Submit::Workspace => Job::AddWorkspace(value.into()),
    };
    vec![run(model, job)]
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
        Cmd::Close | Cmd::CloseCarnet | Cmd::Pull => {
            return lists::of(list).command(model, list, cmd);
        }
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
                model.modal = Some(Modal::Menu {
                    title: "Copy".into(),
                    entries,
                    selected: 0,
                });
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
            model.reviews_due = Due::Fresh;
            model.issues_due = Due::Fresh;
            return vec![run(model, Job::Refresh { full: true })];
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
        Cmd::Back => {
            if in_main {
                model.focus = Focus::Panel(model.panel);
            } else if model.filters.remove(&list).is_some() {
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
    model.modal = Some(Modal::Menu {
        title,
        entries,
        selected: 0,
    });
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
    use std::path::PathBuf;

    use super::*;
    use crate::reviews::{Review, Role};
    use crate::state::Repo;
    use crate::worktrunk::{Forge, Worktree};

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
            group: group.into(),
            tab: false,
            kind: WorkKind::Worktree {
                repo: PathBuf::from(format!("/src/{repo}")),
                repo_name: repo.into(),
                tree: Box::new(Worktree {
                    path,
                    branch: Some(branch.into()),
                    main,
                    short_sha: "abc1234".into(),
                    subject: "Commit".into(),
                    ..Worktree::default()
                }),
            },
        }
    }

    /// A carnet whose one ticket, if any, is `group`.
    pub fn carnet(name: &str, group: &str, workspace: &str) -> Work {
        Work {
            path: PathBuf::from(format!("/data/{name}")),
            workspace: workspace.into(),
            group: group.into(),
            tab: false,
            kind: WorkKind::Carnet {
                tickets: Some(group.to_owned())
                    .filter(|group| !group.is_empty())
                    .into_iter()
                    .collect(),
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
        model.loading.clear();
        model
    }

    fn press(model: &mut Model, keys: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\x1b' => KeyCode::Esc,
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
                Row::Group { name, .. } => format!("[{name}]"),
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
        press(&mut model, "gg");
        assert_eq!(model.index(List::Work), 0);
        press(&mut model, "G");
        assert_eq!(model.index(List::Work), 3);
        press(&mut model, "<");
        assert_eq!(model.index(List::Work), 0);
    }

    #[test]
    fn panels_and_sub_tabs() {
        let mut model = tall_main();
        press(&mut model, "h");
        assert_eq!(model.focus, Focus::Panel(Panel::Workspaces));
        press(&mut model, "]");
        assert_eq!(model.active(), List::Repos);
        press(&mut model, "l");
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
            Action::Commits("/src/api.ABC-1-login".into(), vec!["abc x".into()]),
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
        press(&mut model, "G");
        press(&mut model, "d");
        assert!(model.modal.is_none());
        assert!(model.log.last().unwrap().command.contains("main"));
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
        press(&mut model, "?G");
        let Some(Modal::Menu {
            selected, entries, ..
        }) = &model.modal
        else {
            panic!();
        };
        assert_eq!(*selected, entries.len() - 1, "G goes to the last entry");
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
                group: "ABC-1".into(),
            }]
        );
        assert!(model.loading.contains_key(&Source::Run));
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
        press(&mut model, "G");
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
        press(&mut model, "e");
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
    fn pull_and_close_act_on_the_selection() {
        let mut model = model();
        model.snapshot.work[2].tab = true;
        assert_eq!(
            jobs(press(&mut model, "x")),
            [Job::Close(vec!["/src/web.ABC-1-form".into()])]
        );
        press(&mut model, "G");
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
    fn space_on_a_workspace_switches_or_attaches() {
        let mut model = model();
        press(&mut model, "1j");
        assert_eq!(
            jobs(press(&mut model, " ")),
            [Job::SwitchWorkspace("side".into())]
        );
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
    fn enter_under_a_filter_toggles_a_groups_fold() {
        let mut model = model();
        press(&mut model, "\n");
        assert_eq!(titles(&model), ["[ABC-1]", "api:main"]);
        press(&mut model, "/form\ngg\n\x1b");
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
    fn fast_refreshes_keep_commits_whose_head_did_not_move() {
        let mut model = with_carnets(model());
        let loaded = |model: &Model| -> Vec<String> {
            let mut paths: Vec<String> = (model.commits.keys().chain(model.readmes.keys()))
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
            let carnet = PathBuf::from("/data/2026-10-02-ideas");
            model.readmes.insert(carnet, None);
        };
        fill(&mut model);
        let mut snapshot = model.snapshot.clone();
        snapshot.work[1].tree_mut().short_sha = "def5678".into();
        snapshot
            .work
            .retain(|work| work.path != Path::new("/src/web"));
        snapshot
            .work
            .retain(|work| !work.path.ends_with("2026-09-20-old"));
        refresh(&mut model, snapshot, false);
        assert_eq!(
            loaded(&model),
            [
                "/data/2026-10-01-ABC-1-logs",
                "/data/2026-10-02-ideas",
                "/data/2026-10-02-ideas",
                "/src/api",
                "/src/web.ABC-1-form",
            ],
            "the moved head and the unlisted items are dropped"
        );
        fill(&mut model);
        let snapshot = model.snapshot.clone();
        refresh(&mut model, snapshot, true);
        assert!(loaded(&model).is_empty(), "a full refresh drops everything");
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
        assert_eq!(model.loading.keys().collect::<Vec<_>>(), [&Source::Wt]);
    }

    #[test]
    fn pulling_rows_spin_until_their_job_finishes() {
        let mut model = model();
        press(&mut model, " ");
        model.loading.clear();
        assert!(!model.animating());
        let jobs = jobs(press(&mut model, "p"));
        let paths = vec![
            PathBuf::from("/src/api.ABC-1-login"),
            PathBuf::from("/src/web.ABC-1-form"),
        ];
        assert_eq!(jobs, [Job::Pull(paths.clone())]);
        assert!(paths.iter().all(|path| model.pulling.contains(path)));
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
        assert!(model.pulling.is_empty());
    }

    #[test]
    fn timers_refresh_fast_when_idle_and_fully_every_five_minutes() {
        let mut model = model();
        for _ in 0..FAST_REFRESH - 1 {
            assert!(update(&mut model, Action::Tick).is_empty());
        }
        assert_eq!(
            update(&mut model, Action::Tick),
            [Effect::Run(Job::Refresh { full: false })]
        );
        assert!(update(&mut model, Action::Tick).is_empty(), "one in flight");
        model.loading.clear();
        model.since_full = FULL_REFRESH - 1;
        press(&mut model, "j");
        assert_eq!(
            jobs(update(&mut model, Action::Tick)),
            [Job::Refresh { full: true }]
        );
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
        let commits = (0..20).map(|n| format!("commit {n}")).collect();
        update(
            &mut model,
            Action::Commits("/src/api.ABC-1-login".into(), commits),
        );
        model
    }

    #[test]
    fn list_keys_scroll_the_focused_main_view() {
        let mut model = tall_main();
        press(&mut model, "0G");
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

    #[test]
    fn actions_menu_runs_the_chosen_command() {
        let mut model = model();
        press(&mut model, "?");
        assert_eq!(menu_keys(&model)[0], "Space");
        press(&mut model, "\n");
        assert!(model.modal.is_none());
        assert!(
            model.loading.contains_key(&Source::Run),
            "Space opened the group"
        );
    }

    pub fn review(provider: Provider, role: Role, number: u64, project_url: &str) -> Review {
        let project = project_url.rsplit('/').next().unwrap();
        Review {
            provider,
            role,
            number,
            title: format!("Change {number}"),
            url: format!("{project_url}/pull/{number}"),
            project: format!("org/{project}"),
            project_url: project_url.into(),
            author: "alice".into(),
            branch: format!("change-{number}"),
            base: "main".into(),
            draft: false,
            updated_at: format!("2026-10-0{number}T00:00:00Z"),
        }
    }

    /// Reviews of the registered api repo and of an unregistered one, in both roles.
    pub fn with_reviews(mut model: Model) -> Model {
        update(
            &mut model,
            Action::Reviews {
                provider: Provider::GitHub,
                reviews: Ok(vec![
                    review(Provider::GitHub, Role::ToReview, 1, "https://forge/other"),
                    review(Provider::GitHub, Role::ToReview, 2, "https://forge/api"),
                    review(Provider::GitHub, Role::Mine, 3, "https://forge/api"),
                ]),
                log: Vec::new(),
            },
        );
        model
    }

    fn review_numbers(model: &Model, list: List) -> Vec<u64> {
        model.reviews(list).iter().map(|r| r.number).collect()
    }

    #[test]
    fn listings_that_ask_for_reviews_fetch_them_per_provider() {
        let mut model = Model::new((120, 40));
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
        let first = Job::Reviews {
            provider: Provider::GitHub,
            hosts: vec!["forge".into()],
            force: false,
        };
        assert_eq!(
            loaded(&mut model),
            std::slice::from_ref(&first),
            "startup lists them"
        );
        model.loading.clear();
        assert!(loaded(&mut model).is_empty(), "a fast refresh does not");
        press(&mut model, "R");
        model.loading.clear();
        let [Job::Reviews { force: true, .. }] = loaded(&mut model)[..] else {
            panic!("R lists them past the cache");
        };
        model.loading.clear();
        model.since_full = FULL_REFRESH;
        update(&mut model, Action::Tick);
        model.loading.clear();
        assert_eq!(loaded(&mut model), [first], "so does the full refresh");
        press(&mut model, "R");
        assert!(
            loaded(&mut model).is_empty(),
            "the full refresh is still listing reviews"
        );
        assert_eq!(model.reviews_due, Due::Fresh, "R waits for it");
        model.loading.clear();
        let [Job::Reviews { force: true, .. }] = loaded(&mut model)[..] else {
            panic!("then R lists them past the cache");
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
            Action::Reviews {
                provider: Provider::GitLab,
                reviews: Ok(vec![gitlab]),
                log: Vec::new(),
            },
        );
        assert_eq!(review_numbers(&model, List::ToReview), [9, 2, 1]);
        assert_eq!(model.review().unwrap().number, 1, "still selected");
        update(
            &mut model,
            Action::Reviews {
                provider: Provider::GitHub,
                reviews: Err("offline".into()),
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
            [Job::Checkout {
                repo: "/src/api".into(),
                workspace: "default".into(),
                review: Box::new(review(
                    Provider::GitHub,
                    Role::ToReview,
                    2,
                    "https://forge/api"
                )),
            }]
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
        let other = model.reviews(List::ToReview)[1].clone();
        assert_eq!(model.review_project(&other), "org/other");
        assert!(model.review_work(&other).is_none());
    }

    /// The triage label scheme, with an issue in each section and in Other, one hidden, and a
    /// Jira issue whose key groups the ABC-1 worktrees.
    pub fn with_issues(mut model: Model) -> Model {
        use crate::issues::tests::{SCHEME, issue};
        model.tracker_config = crate::config::Config::parse(SCHEME).unwrap().tracker;
        let mut jira = issue("ABC-1", &["ready-for-agent"], false);
        jira.tracker = crate::issues::Tracker::Jira;
        jira.project_url = None;
        update(
            &mut model,
            Action::Issues {
                tracker: crate::issues::Tracker::GitHub,
                issues: Ok(vec![
                    issue("api#1", &["ready-for-agent"], false),
                    issue("api#2", &["needs-triage"], false),
                    issue("api#3", &["ready-for-agent"], true),
                    issue("api#4", &[], false),
                    issue("api#5", &["wontfix"], false),
                    issue("api#6", &["enhancement"], false),
                ]),
                log: Vec::new(),
            },
        );
        update(
            &mut model,
            Action::Issues {
                tracker: crate::issues::Tracker::Jira,
                issues: Ok(vec![jira]),
                log: Vec::new(),
            },
        );
        model
    }

    fn issue_keys(model: &Model, section: usize) -> Vec<String> {
        (model.issues(List::Section(section)).iter())
            .map(|issue| issue.key.clone())
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
        assert_eq!(model.issue().unwrap().key, "ABC-1");
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
            Action::Issues {
                tracker: crate::issues::Tracker::GitHub,
                issues: Ok(vec![crate::issues::tests::issue(
                    "api#1",
                    &["ready-for-agent"],
                    false,
                )]),
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
            Job::Start {
                repo,
                branch,
                workspace,
                issue,
            },
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
        assert_eq!(
            jobs(press(&mut model, " ")),
            [Job::Open(vec![
                "/src/api.ABC-1-login".into(),
                "/src/web.ABC-1-form".into()
            ])],
            "the Jira issue's key groups its linked work"
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

    #[test]
    fn the_repo_menu_suggests_the_linked_works_repo_before_the_issues_own() {
        let mut model = with_issues(model());
        // A tracker-only api: the work on api#1 happens in web.
        model.snapshot.work[3].group = "api#1".into();
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
        assert_eq!(model.issue().unwrap().key, "ABC-1");
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
        for keys in ["d", "e", "m", "x", "p"] {
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
                "[]"
            ],
            "ungrouped carnets start folded"
        );
        press(&mut model, "G\n");
        assert_eq!(
            titles(&model)[5..],
            ["[]", "2026-10-02-ideas", "2026-09-20-old"],
            "newest first"
        );
        press(&mut model, "-");
        assert_eq!(titles(&model), ["[ABC-1]", "api:main", "[]"]);
        press(&mut model, "=");
        assert_eq!(titles(&model).len(), 8);
        press(&mut model, "Gn");
        press(&mut model, "c");
        assert_eq!(
            jobs(press(&mut model, "x\n")),
            [Job::NewCarnet {
                name: "x".into(),
                workspace: "default".into(),
                group: String::new(),
            }],
            "the Carnets group is no group"
        );
    }

    #[test]
    fn selecting_a_carnet_reads_its_readme_once() {
        let mut model = with_carnets(model());
        press(&mut model, "G\n");
        let effects = press(&mut model, "j");
        let path = PathBuf::from("/data/2026-10-02-ideas");
        assert!(
            effects.contains(&Effect::Run(Job::Readme(path.clone()))),
            "{effects:?}"
        );
        update(&mut model, Action::Readme(path, Some("# ideas".into())));
        let effects = press(&mut model, "jk");
        assert!(
            !effects.iter().any(|effect| matches!(effect, Effect::Run(Job::Readme(path)) if path.ends_with("2026-10-02-ideas"))),
            "{effects:?}"
        );
        press(&mut model, "gg");
        assert!(
            !press(&mut model, "j")
                .iter()
                .any(|effect| matches!(effect, Effect::Run(Job::Readme(_)))),
            "a worktree has no README"
        );
    }

    #[test]
    fn a_carnet_listing_an_issues_key_is_its_linked_work() {
        let mut model = with_issues(with_carnets(model()));
        let mut linked = carnet("2026-07-01-notes", "", "side");
        if let WorkKind::Carnet {
            tickets, closed, ..
        } = &mut linked.kind
        {
            *tickets = vec!["XYZ-1".into(), "api#4".into()];
            *closed = true;
        }
        model.snapshot.carnets.push(linked);
        let issue = |key: &str| {
            let issue = model.issues.iter().find(|issue| issue.key == key);
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
            "a closed carnet, by a later ticket"
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
        press(&mut model, "G");
        assert_eq!(
            jobs(press(&mut model, "c")),
            [Job::CloseCarnet(vec![
                "/data/2026-10-02-ideas".into(),
                "/data/2026-09-20-old".into()
            ])],
            "every carnet of the group"
        );
        press(&mut model, "gg");
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
        assert!(jobs(press(&mut model, "\n")).is_empty(), "no name");
        press(&mut model, "nc");
        assert_eq!(
            jobs(press(&mut model, "logs\n")),
            [Job::NewCarnet {
                name: "logs".into(),
                workspace: "default".into(),
                group: "ABC-1".into(),
            }]
        );
        press(&mut model, "n1");
        let Some(Modal::Prompt { title, .. }) = &model.modal else {
            panic!("no branch prompt");
        };
        assert!(title.contains("of api"), "{title}");
        press(&mut model, "\x1bGn");
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
        assert!(jobs(press(&mut model, "Gp")).is_empty());
    }

    #[test]
    fn carnets_are_never_removed() {
        let mut model = with_carnets(model());
        press(&mut model, "Gd");
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
}
