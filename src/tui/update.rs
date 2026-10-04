//! `update(model, action) -> effects`: every state change, with no I/O.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use super::app::{
    Action, Binding, Cmd, Effect, FAST_REFRESH, FULL_REFRESH, Focus, Job, KEYMAP, Line, List,
    LogEntry, MenuEntry, Modal, Model, On, Panel, Popup, PopupCmd, Removal, Screen, Submit, lookup,
    popup_lookup,
};
use super::view::{areas, main_len, offset};

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
            model.push_log([LogEntry {
                command: format!("copy {text}"),
                error: None,
            }]);
            vec![Effect::Copy(text)]
        }
        Action::Loaded { snapshot, log } => {
            done(model, "wt");
            model.push_log(log);
            match snapshot {
                Ok(snapshot) => {
                    let keep = Keep::of(model);
                    model.snapshot = snapshot;
                    model.loaded = true;
                    keep.restore(model);
                    model.commits.clear();
                    commits(model)
                }
                Err(error) => {
                    model.push_log([LogEntry {
                        command: "refresh".into(),
                        error: Some(error),
                    }]);
                    Vec::new()
                }
            }
        }
        Action::Commits(path, lines) => {
            done(model, "git");
            model.commits.insert(path, lines);
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
                model.push_log([LogEntry {
                    command: "atelier".into(),
                    error: Some(error),
                }]);
            }
            vec![run(model, Job::Refresh { full: false })]
        }
    }
}

/// Starts a job, showing its loading indicator.
fn run(model: &mut Model, job: Job) -> Effect {
    *model.loading.entry(job.source()).or_default() += 1;
    if let Job::Pull(paths) = &job {
        model.pulling.extend(paths.iter().cloned());
    }
    if let Job::Refresh { full } = job {
        model.since_refresh = 0;
        if full {
            model.since_full = 0;
        }
    }
    Effect::Run(job)
}

fn done(model: &mut Model, source: &'static str) {
    if let Some(count) = model.loading.get_mut(source) {
        *count -= 1;
        if *count == 0 {
            model.loading.remove(source);
        }
    }
}

fn tick(model: &mut Model) -> Vec<Effect> {
    model.frame = model.frame.wrapping_add(1);
    model.idle += 1;
    model.since_refresh += 1;
    model.since_full += 1;
    if model.loading.contains_key("wt") {
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

/// Selections, kept by identity across a refresh.
struct Keep {
    workspace: Option<String>,
    repo: Option<std::path::PathBuf>,
    line: Option<LineKey>,
}

#[derive(PartialEq)]
enum LineKey {
    Group(String),
    Item(std::path::PathBuf),
}

impl Keep {
    fn of(model: &Model) -> Self {
        Self {
            workspace: model.workspace().map(Into::into),
            repo: model.repo().map(|repo| repo.path.clone()),
            line: model.line().map(|line| line_key(model, &line)),
        }
    }

    fn restore(self, model: &mut Model) {
        let workspace = self
            .workspace
            .and_then(|name| model.workspaces().iter().position(|other| **other == name));
        model
            .selected
            .insert(List::Workspaces, workspace.unwrap_or(0));
        let repo = self
            .repo
            .and_then(|path| model.repos().iter().position(|repo| repo.path == path));
        model.selected.insert(List::Repos, repo.unwrap_or(0));
        let line = self.line.and_then(|key| {
            model
                .lines()
                .iter()
                .position(|line| line_key(model, line) == key)
        });
        let line = line.unwrap_or(model.index(List::Work));
        model.selected.insert(List::Work, line);
        clamp_all(model);
    }
}

fn line_key(model: &Model, line: &Line) -> LineKey {
    match line {
        Line::Group { key, .. } => LineKey::Group(key.clone()),
        Line::Item(index) => LineKey::Item(model.snapshot.work[*index].path().clone()),
    }
}

fn clamp_all(model: &mut Model) {
    for list in [List::Workspaces, List::Repos, List::Work] {
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

/// Fetches the selected worktree's recent commits unless they are loaded.
fn commits(model: &mut Model) -> Vec<Effect> {
    if let Some(Line::Item(index)) = model.line() {
        let path = model.snapshot.work[index].path().clone();
        if !model.commits.contains_key(&path) {
            return vec![run(model, Job::Commits(path))];
        }
    }
    Vec::new()
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
        _ if value.is_empty() && matches!(then, Submit::Branch { .. }) => {
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
        Submit::Group(paths) => Job::Regroup {
            paths,
            group: value.into(),
        },
        Submit::Alias(repo) => Job::SetAlias {
            repo,
            alias: value.into(),
        },
    };
    vec![run(model, job)]
}

fn note(model: &mut Model, message: &str) -> Vec<Effect> {
    model.push_log([LogEntry {
        command: message.into(),
        error: None,
    }]);
    Vec::new()
}

/// A page of the active list.
fn page(model: &Model) -> usize {
    let areas = areas(model, Rect::new(0, 0, model.size.0, model.size.1));
    areas
        .panels
        .iter()
        .find(|(panel, _)| *panel == model.panel)
        .map_or(1, |(_, rect)| rect.height.saturating_sub(2).max(1) as usize)
}

fn main_height(model: &Model) -> u16 {
    let areas = areas(model, Rect::new(0, 0, model.size.0, model.size.1));
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
        Cmd::Jump(number) => match Panel::ALL.get(number.wrapping_sub(1)) {
            Some(&panel) => return focus_panel(model, panel),
            None => return note(model, "that panel comes in a later phase"),
        },
        Cmd::FocusMain => model.focus = Focus::Main,
        Cmd::ScrollDown => scroll(model, 1, 0),
        Cmd::ScrollUp => scroll(model, -1, 0),
        Cmd::ScrollPageDown => scroll(model, main_height(model) as i32, 0),
        Cmd::ScrollPageUp => scroll(model, -(main_height(model) as i32), 0),
        Cmd::ScrollLeft => scroll(model, 0, -4),
        Cmd::ScrollRight => scroll(model, 0, 4),
        Cmd::NextTab | Cmd::PrevTab => {
            if model.panel == Panel::Workspaces {
                model.sub = match model.sub {
                    List::Workspaces => List::Repos,
                    _ => List::Workspaces,
                };
                model.scroll = (0, 0);
            }
        }
        Cmd::Activate => return activate(model),
        Cmd::Enter => match model.line() {
            Some(Line::Group { key, .. }) if list == List::Work => {
                if !model.folded.remove(&key) {
                    model.folded.insert(key);
                }
            }
            _ => model.focus = Focus::Main,
        },
        Cmd::CollapseAll => {
            let keys: Vec<String> = model
                .lines()
                .into_iter()
                .filter_map(|line| match line {
                    Line::Group { key, .. } => Some(key),
                    Line::Item(_) => None,
                })
                .collect();
            model.folded.extend(keys);
            clamp_all(model);
        }
        Cmd::ExpandAll => model.folded.clear(),
        Cmd::New => return new(model),
        Cmd::Edit => return edit(model),
        Cmd::Move => return move_to(model),
        Cmd::Remove => return remove(model),
        Cmd::Close => {
            let paths: Vec<_> = work_targets(model)
                .into_iter()
                .filter(|work| work.tab)
                .map(|work| work.path().clone())
                .collect();
            if !paths.is_empty() {
                return vec![run(model, Job::Close(paths))];
            }
        }
        Cmd::Pull => {
            let paths: Vec<_> = work_targets(model)
                .into_iter()
                .map(|work| work.path().clone())
                .collect();
            if !paths.is_empty() {
                return vec![run(model, Job::Pull(paths))];
            }
        }
        Cmd::Browse => {
            return match url(model) {
                Some(url) => vec![run(model, Job::Browse(url))],
                None => note(model, "no forge URL for this selection"),
            };
        }
        Cmd::CopyMenu => {
            let mut entries = Vec::new();
            for (key, label, value) in [
                ("p", "path", copy_path(model)),
                ("b", "branch", branch(model)),
                ("u", "URL", url(model)),
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
            if let Some(path) = copy_path(model) {
                return update(model, Action::Copy(path));
            }
        }
        Cmd::Filter if !in_main => model.filtering = Some(list),
        Cmd::Filter => {}
        Cmd::Refresh => return vec![run(model, Job::Refresh { full: true })],
        Cmd::Menu => {
            let here = |binding: &&Binding| matches!(binding.on, On::Lists(lists) if lists.contains(&list));
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
            model.quit = true;
            return vec![Effect::Quit];
        }
    }
    Vec::new()
}

/// The worktrees the Work panel's selection covers, or none from another panel.
fn work_targets(model: &Model) -> Vec<&super::app::Work> {
    if model.active() == List::Work {
        model.targets()
    } else {
        Vec::new()
    }
}

fn activate(model: &mut Model) -> Vec<Effect> {
    match model.active() {
        List::Workspaces => {
            let Some(name) = model.workspace().map(str::to_owned) else {
                return Vec::new();
            };
            if model.snapshot.here.is_some() {
                vec![run(model, Job::SwitchWorkspace(name))]
            } else {
                vec![Effect::Attach(name)]
            }
        }
        List::Repos => Vec::new(),
        List::Work => {
            let paths: Vec<_> = model
                .targets()
                .into_iter()
                .map(|work| work.path().clone())
                .collect();
            if paths.is_empty() {
                Vec::new()
            } else {
                vec![run(model, Job::Open(paths))]
            }
        }
    }
}

fn new(model: &mut Model) -> Vec<Effect> {
    match model.active() {
        List::Workspaces => note(model, "create workspaces with `atelier ws add <name>`"),
        List::Repos => note(model, "register repos with `atelier add <path>`"),
        List::Work => {
            let Some(workspace) = model.workspace().map(str::to_owned) else {
                return Vec::new();
            };
            let group = match model.line() {
                Some(Line::Group { name, .. }) => name,
                Some(Line::Item(index)) => model.snapshot.work[index].group.clone(),
                None => String::new(),
            };
            let ask = |repo: std::path::PathBuf, name: String| Action::Ask {
                title: format!("New worktree of {name}: branch"),
                initial: String::new(),
                then: Submit::Branch {
                    repo,
                    workspace: workspace.clone(),
                    group: group.clone(),
                },
            };
            if let Some(work) = model.targets().first() {
                let action = ask(work.repo.clone(), work.repo_name.clone());
                return update(model, action);
            }
            let entries: Vec<MenuEntry> = model
                .snapshot
                .repos
                .iter()
                .enumerate()
                .map(|(index, repo)| MenuEntry {
                    key: (index + 1).to_string(),
                    label: repo.name(),
                    action: ask(repo.path.clone(), repo.name()),
                })
                .collect();
            if entries.is_empty() {
                return note(
                    model,
                    "no repos yet: register one with `atelier add <path>`",
                );
            }
            model.modal = Some(Modal::Menu {
                title: "New worktree in".into(),
                entries,
                selected: 0,
            });
            Vec::new()
        }
    }
}

fn edit(model: &mut Model) -> Vec<Effect> {
    let action = match model.active() {
        List::Workspaces => return Vec::new(),
        List::Repos => {
            let Some(repo) = model.repo() else {
                return Vec::new();
            };
            Action::Ask {
                title: format!("Alias of {} (empty clears it)", repo.path.display()),
                initial: repo.alias.clone().unwrap_or_default(),
                then: Submit::Alias(repo.path.clone()),
            }
        }
        List::Work => {
            let targets = model.targets();
            let Some(first) = targets.first() else {
                return Vec::new();
            };
            Action::Ask {
                title: format!("Group of {} worktree(s)", targets.len()),
                initial: first.group.clone(),
                then: Submit::Group(targets.iter().map(|work| work.path().clone()).collect()),
            }
        }
    };
    update(model, action)
}

fn workspace_menu(
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
        return note(
            model,
            "no other workspace: create one with `atelier ws add <name>`",
        );
    }
    model.modal = Some(Modal::Menu {
        title,
        entries,
        selected: 0,
    });
    Vec::new()
}

fn move_to(model: &mut Model) -> Vec<Effect> {
    match model.active() {
        List::Workspaces => Vec::new(),
        List::Repos => {
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
        List::Work => {
            let targets = model.targets();
            let Some(first) = targets.first() else {
                return Vec::new();
            };
            let current = first.workspace.clone();
            let paths: Vec<_> = targets.iter().map(|work| work.path().clone()).collect();
            workspace_menu(
                model,
                format!("Move {} worktree(s) to", paths.len()),
                &current,
                |workspace| Job::Move {
                    paths: paths.clone(),
                    workspace,
                },
            )
        }
    }
}

fn confirm(model: &mut Model, title: String, lines: Vec<String>, job: Job) -> Vec<Effect> {
    model.modal = Some(Modal::Confirm { title, lines, job });
    Vec::new()
}

fn remove(model: &mut Model) -> Vec<Effect> {
    match model.active() {
        List::Workspaces => note(model, "remove workspaces with `atelier ws rm <name>`"),
        List::Repos => note(model, "forget repos with `atelier rm <repo>`"),
        List::Work => {
            let targets = model.targets();
            let removals: Vec<Removal> = targets
                .iter()
                .filter(|work| !work.tree.main)
                .map(|work| Removal {
                    repo: work.repo.clone(),
                    path: work.path().clone(),
                    branch: work.tree.branch.clone(),
                    force: work.tree.dirty,
                })
                .collect();
            if removals.is_empty() {
                return note(model, "main worktrees are never removed");
            }
            let mut lines = vec!["Remove these worktrees?".to_owned()];
            for work in targets.iter().filter(|work| !work.tree.main) {
                let dirty = if work.tree.dirty {
                    "  (uncommitted changes will be lost)"
                } else {
                    ""
                };
                lines.push(format!("  {}{dirty}", work.title()));
            }
            confirm(model, "Remove".into(), lines, Job::Remove(removals))
        }
    }
}

fn copy_path(model: &Model) -> Option<String> {
    match model.active() {
        List::Workspaces => model.workspace().map(Into::into),
        List::Repos => model.repo().map(|repo| repo.path.display().to_string()),
        List::Work => match model.line()? {
            Line::Item(index) => Some(model.snapshot.work[index].path().display().to_string()),
            Line::Group { name, .. } => Some(name),
        },
    }
}

fn branch(model: &Model) -> Option<String> {
    match (model.active(), model.line()?) {
        (List::Work, Line::Item(index)) => model.snapshot.work[index].tree.branch.clone(),
        _ => None,
    }
}

/// The forge page of the selected repo, or of the selected worktree's branch.
fn url(model: &Model) -> Option<String> {
    let forges = &model.snapshot.forges;
    match model.active() {
        List::Repos => forges
            .get(&model.repo()?.path)
            .map(|forge| forge.url.clone()),
        List::Work => match model.line()? {
            Line::Item(index) => {
                let work = &model.snapshot.work[index];
                let forge = forges.get(&work.repo)?;
                Some(match &work.tree.branch {
                    Some(branch) => forge.branch_url(branch),
                    None => forge.url.clone(),
                })
            }
            Line::Group { .. } => None,
        },
        List::Workspaces => None,
    }
}

fn mouse_event(model: &mut Model, mouse: MouseEvent) -> Vec<Effect> {
    if model.modal.is_some() {
        return Vec::new();
    }
    let areas = areas(model, Rect::new(0, 0, model.size.0, model.size.1));
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
    use crate::state::Repo;
    use crate::tui::app::{Snapshot, Work};
    use crate::worktrunk::{Forge, Worktree};

    pub fn work(repo: &str, branch: &str, group: &str, workspace: &str) -> Work {
        let main = branch == "main";
        let path = if main {
            PathBuf::from(format!("/src/{repo}"))
        } else {
            PathBuf::from(format!("/src/{repo}.{branch}"))
        };
        Work {
            repo: PathBuf::from(format!("/src/{repo}")),
            repo_name: repo.into(),
            workspace: workspace.into(),
            group: group.into(),
            tab: false,
            tree: Worktree {
                path,
                branch: Some(branch.into()),
                main,
                short_sha: "abc1234".into(),
                subject: "Commit".into(),
                ..Worktree::default()
            },
        }
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
            .lines()
            .into_iter()
            .map(|line| match line {
                Line::Group { name, .. } => format!("[{name}]"),
                Line::Item(index) => model.snapshot.work[index].title(),
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
        model.snapshot.work[1].tree.dirty = true;
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
    fn workspaces_and_repos_are_created_and_removed_from_the_cli_only() {
        let mut model = model();
        for (keys, hint) in [
            ("1n", "atelier ws add"),
            ("d", "atelier ws rm"),
            ("]d", "atelier rm"),
        ] {
            assert!(jobs(press(&mut model, keys)).is_empty(), "{keys}");
            assert!(model.modal.is_none(), "{keys}");
            assert!(model.log.last().unwrap().command.contains(hint), "{keys}");
        }
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
        assert!(model.loading.contains_key("run"));
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
                log: Vec::new(),
            },
        );
        assert_eq!(
            model.targets()[0].path(),
            &PathBuf::from("/src/web.ABC-1-form")
        );
    }

    #[test]
    fn finished_jobs_log_and_refresh() {
        let mut model = model();
        press(&mut model, "p");
        let effects = update(
            &mut model,
            Action::Finished {
                job: Job::Pull(vec!["/src/api".into()]),
                log: vec![LogEntry {
                    command: "git pull".into(),
                    error: Some("boom".into()),
                }],
                error: Some("boom".into()),
            },
        );
        assert_eq!(effects, [Effect::Run(Job::Refresh { full: false })]);
        assert_eq!(model.log.len(), 1);
        assert_eq!(model.loading.keys().collect::<Vec<_>>(), [&"wt"]);
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
        assert!(model.quit);
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
        assert!(model.loading.contains_key("run"), "Space opened the group");
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
}
