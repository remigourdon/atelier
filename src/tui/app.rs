//! The TUI's model: what is loaded, what is selected and focused, and the keymap.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use tui_input::Input;

use crate::process::Logged;
use crate::state::Repo;
use crate::worktrunk::Worktree;

/// The side panels, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Workspaces,
    Work,
}

impl Panel {
    pub const ALL: [Panel; 2] = [Panel::Workspaces, Panel::Work];

    pub fn number(self) -> usize {
        Panel::ALL.iter().position(|&p| p == self).unwrap() + 1
    }
}

/// The selectable lists. Panel 1 shows Workspaces or Repos, its two sub-tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    Workspaces,
    Repos,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Panel(Panel),
    Main,
}

/// Lazygit's screen modes: `+` widens the focused view, `_` narrows it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    Normal,
    Half,
    Full,
}

/// Everything loaded from the database, worktrunk and zellij.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// The zellij session the TUI runs in.
    pub here: Option<String>,
    /// The current session first.
    pub workspaces: Vec<String>,
    pub repos: Vec<Repo>,
    pub work: Vec<Work>,
    /// Each repo's forge web page, by repo path.
    pub forges: HashMap<PathBuf, String>,
}

/// A worktree with what atelier records about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Work {
    pub repo: PathBuf,
    pub repo_name: String,
    pub workspace: String,
    pub group: String,
    pub tab: bool,
    pub tree: Worktree,
}

impl Work {
    pub fn path(&self) -> &PathBuf {
        &self.tree.path
    }

    /// The branch, else the directory name of a detached worktree.
    pub fn branch(&self) -> String {
        self.tree
            .branch
            .clone()
            .unwrap_or_else(|| crate::state::dir_name(&self.tree.path))
    }

    pub fn title(&self) -> String {
        format!("{}:{}", self.repo_name, self.branch())
    }
}

/// A row of the Work panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// A group header; `members` index `Snapshot::work`.
    Group {
        key: String,
        name: String,
        members: Vec<usize>,
        folded: bool,
    },
    Item(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub command: String,
    pub error: Option<String>,
}

impl From<Logged> for LogEntry {
    fn from(logged: Logged) -> Self {
        Self {
            command: logged.command,
            error: logged.error,
        }
    }
}

/// A removal: the worktree, and whether it has changes that will be discarded.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub force: bool,
}

/// Background work, run off the UI thread; each reports back with actions.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    Refresh {
        full: bool,
    },
    Commits(PathBuf),
    Open(Vec<PathBuf>),
    Close(Vec<PathBuf>),
    Pull(Vec<PathBuf>),
    Create {
        repo: PathBuf,
        branch: String,
        workspace: String,
        group: String,
    },
    Remove(Vec<Removal>),
    Move {
        paths: Vec<PathBuf>,
        workspace: String,
    },
    Regroup {
        paths: Vec<PathBuf>,
        group: String,
    },
    SetAlias {
        repo: PathBuf,
        alias: String,
    },
    SetRepoWorkspace {
        repo: PathBuf,
        workspace: String,
    },
    Forget(PathBuf),
    AddWorkspace(String),
    RemoveWorkspace(String),
    SwitchWorkspace(String),
    Browse(String),
}

impl Job {
    /// The loading indicator it shows in the hint bar.
    pub fn source(&self) -> &'static str {
        match self {
            Job::Refresh { .. } => "wt",
            Job::Commits(_) => "git",
            _ => "run",
        }
    }
}

/// What `update` asks the loop to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Run(Job),
    /// Attach to a session from outside zellij, handing it the terminal.
    Attach(String),
    /// Copy to the clipboard through OSC 52.
    Copy(String),
    Quit,
}

/// What a submitted prompt does.
#[derive(Debug, Clone, PartialEq)]
pub enum Submit {
    Branch {
        repo: PathBuf,
        workspace: String,
        group: String,
    },
    Group(Vec<PathBuf>),
    Alias(PathBuf),
    Workspace,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuEntry {
    pub key: String,
    pub label: String,
    pub action: Action,
}

#[derive(Debug, Clone)]
pub enum Modal {
    Prompt {
        title: String,
        input: Input,
        then: Submit,
    },
    Confirm {
        title: String,
        lines: Vec<String>,
        job: Job,
    },
    Menu {
        title: String,
        entries: Vec<MenuEntry>,
        selected: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    /// Once a second.
    Tick,
    Cmd(Cmd),
    Run(Job),
    Ask {
        title: String,
        initial: String,
        then: Submit,
    },
    Copy(String),
    Loaded {
        snapshot: Result<Snapshot, String>,
        log: Vec<LogEntry>,
    },
    Commits(PathBuf, Vec<String>),
    Finished {
        source: &'static str,
        log: Vec<LogEntry>,
        error: Option<String>,
    },
}

/// Every command a key can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    PrevPanel,
    NextPanel,
    Jump(usize),
    FocusMain,
    ScrollDown,
    ScrollUp,
    ScrollPageDown,
    ScrollPageUp,
    ScrollLeft,
    ScrollRight,
    PrevTab,
    NextTab,
    Activate,
    Enter,
    CollapseAll,
    ExpandAll,
    New,
    Edit,
    Move,
    Remove,
    Close,
    Pull,
    Browse,
    CopyMenu,
    CopyPath,
    Filter,
    Refresh,
    Menu,
    NextScreen,
    PrevScreen,
    ToggleLog,
    Back,
    Quit,
}

/// A key: its code and whether Control is held. Shift is part of the character.
pub type Key = (KeyCode, bool);

pub struct Binding {
    pub keys: &'static [Key],
    /// How `?` and the hint bar spell the keys.
    pub label: &'static str,
    pub cmd: Cmd,
    pub help: &'static str,
    /// The lists whose hint bar shows it.
    pub hint: &'static [List],
}

const fn ch(c: char) -> Key {
    (KeyCode::Char(c), false)
}

const fn ctrl(c: char) -> Key {
    (KeyCode::Char(c), true)
}

const fn code(code: KeyCode) -> Key {
    (code, false)
}

const WORK: &[List] = &[List::Work];
const ALL: &[List] = &[List::Workspaces, List::Repos, List::Work];
const NONE: &[List] = &[];

/// The keymap: it drives key handling, the `?` menu and the hint bar.
#[rustfmt::skip]
pub const KEYMAP: &[Binding] = &[
    Binding { keys: &[ch('j'), code(KeyCode::Down)], label: "j/↓", cmd: Cmd::Down, help: "next item", hint: NONE },
    Binding { keys: &[ch('k'), code(KeyCode::Up)], label: "k/↑", cmd: Cmd::Up, help: "previous item", hint: NONE },
    Binding { keys: &[ch('.')], label: ".", cmd: Cmd::PageDown, help: "next page", hint: NONE },
    Binding { keys: &[ch(',')], label: ",", cmd: Cmd::PageUp, help: "previous page", hint: NONE },
    Binding { keys: &[ch('<'), code(KeyCode::Home)], label: "</Home/gg", cmd: Cmd::Top, help: "top", hint: NONE },
    Binding { keys: &[ch('>'), code(KeyCode::End), ch('G')], label: ">/End/G", cmd: Cmd::Bottom, help: "bottom", hint: NONE },
    Binding { keys: &[ch('h'), code(KeyCode::Left), code(KeyCode::BackTab)], label: "h/←/S-Tab", cmd: Cmd::PrevPanel, help: "previous panel", hint: NONE },
    Binding { keys: &[ch('l'), code(KeyCode::Right), code(KeyCode::Tab)], label: "l/→/Tab", cmd: Cmd::NextPanel, help: "next panel", hint: NONE },
    Binding { keys: &[ch('1')], label: "1", cmd: Cmd::Jump(1), help: "Workspaces │ Repos", hint: NONE },
    Binding { keys: &[ch('2')], label: "2", cmd: Cmd::Jump(2), help: "Work", hint: NONE },
    Binding { keys: &[ch('0')], label: "0", cmd: Cmd::FocusMain, help: "focus the main view", hint: NONE },
    Binding { keys: &[ch('J')], label: "J", cmd: Cmd::ScrollDown, help: "scroll the main view down", hint: NONE },
    Binding { keys: &[ch('K')], label: "K", cmd: Cmd::ScrollUp, help: "scroll the main view up", hint: NONE },
    Binding { keys: &[ctrl('d'), code(KeyCode::PageDown)], label: "C-d/PgDn", cmd: Cmd::ScrollPageDown, help: "scroll the main view a page down", hint: NONE },
    Binding { keys: &[ctrl('u'), code(KeyCode::PageUp)], label: "C-u/PgUp", cmd: Cmd::ScrollPageUp, help: "scroll the main view a page up", hint: NONE },
    Binding { keys: &[ch('H')], label: "H", cmd: Cmd::ScrollLeft, help: "scroll the main view left", hint: NONE },
    Binding { keys: &[ch('L')], label: "L", cmd: Cmd::ScrollRight, help: "scroll the main view right", hint: NONE },
    Binding { keys: &[ch('[')], label: "[", cmd: Cmd::PrevTab, help: "previous sub-tab", hint: NONE },
    Binding { keys: &[ch(']')], label: "]", cmd: Cmd::NextTab, help: "next sub-tab", hint: NONE },
    Binding { keys: &[ch(' ')], label: "Space", cmd: Cmd::Activate, help: "open tab · switch workspace", hint: &[List::Workspaces, List::Work] },
    Binding { keys: &[code(KeyCode::Enter)], label: "Enter", cmd: Cmd::Enter, help: "fold group · focus the main view", hint: NONE },
    Binding { keys: &[ch('-')], label: "-", cmd: Cmd::CollapseAll, help: "collapse all groups", hint: NONE },
    Binding { keys: &[ch('=')], label: "=", cmd: Cmd::ExpandAll, help: "expand all groups", hint: NONE },
    Binding { keys: &[ch('n')], label: "n", cmd: Cmd::New, help: "new worktree · new workspace", hint: &[List::Workspaces, List::Work] },
    Binding { keys: &[ch('e')], label: "e", cmd: Cmd::Edit, help: "edit group · edit repo alias", hint: &[List::Repos, List::Work] },
    Binding { keys: &[ch('m')], label: "m", cmd: Cmd::Move, help: "move to workspace · set repo workspace", hint: &[List::Repos, List::Work] },
    Binding { keys: &[ch('d')], label: "d", cmd: Cmd::Remove, help: "remove", hint: ALL },
    Binding { keys: &[ch('x')], label: "x", cmd: Cmd::Close, help: "close tab", hint: WORK },
    Binding { keys: &[ch('p')], label: "p", cmd: Cmd::Pull, help: "pull (git pull --ff-only)", hint: WORK },
    Binding { keys: &[ch('o')], label: "o", cmd: Cmd::Browse, help: "open in browser", hint: NONE },
    Binding { keys: &[ch('y')], label: "y", cmd: Cmd::CopyMenu, help: "copy path, branch or URL", hint: NONE },
    Binding { keys: &[ctrl('o')], label: "C-o", cmd: Cmd::CopyPath, help: "copy path", hint: NONE },
    Binding { keys: &[ch('/')], label: "/", cmd: Cmd::Filter, help: "filter", hint: NONE },
    Binding { keys: &[ch('R')], label: "R", cmd: Cmd::Refresh, help: "refresh", hint: NONE },
    Binding { keys: &[ch('?')], label: "?", cmd: Cmd::Menu, help: "actions menu", hint: ALL },
    Binding { keys: &[ch('+')], label: "+", cmd: Cmd::NextScreen, help: "next screen mode", hint: NONE },
    Binding { keys: &[ch('_')], label: "_", cmd: Cmd::PrevScreen, help: "previous screen mode", hint: NONE },
    Binding { keys: &[ch('@')], label: "@", cmd: Cmd::ToggleLog, help: "toggle the command log", hint: NONE },
    Binding { keys: &[code(KeyCode::Esc)], label: "Esc", cmd: Cmd::Back, help: "back", hint: NONE },
    Binding { keys: &[ch('q'), ctrl('c')], label: "q", cmd: Cmd::Quit, help: "quit", hint: ALL },
];

/// The command bound to a key.
pub fn lookup(key: &KeyEvent) -> Option<Cmd> {
    let pressed = (key.code, key.modifiers.contains(KeyModifiers::CONTROL));
    KEYMAP
        .iter()
        .find(|binding| binding.keys.contains(&pressed))
        .map(|binding| binding.cmd)
}

/// Seconds between refreshes: a fast one once idle, a full one regardless.
pub const FAST_REFRESH: u32 = 10;
pub const FULL_REFRESH: u32 = 300;
const LOG_LIMIT: usize = 500;

pub struct Model {
    pub snapshot: Snapshot,
    pub loaded: bool,
    pub focus: Focus,
    /// The side panel focus returns to from the main view.
    pub panel: Panel,
    /// Panel 1's sub-tab.
    pub sub: List,
    pub selected: HashMap<List, usize>,
    pub filters: HashMap<List, Input>,
    /// The list whose filter is being typed.
    pub filtering: Option<List>,
    /// Folded group keys.
    pub folded: HashSet<String>,
    /// The main view's vertical and horizontal scroll.
    pub scroll: (u16, u16),
    pub screen: Screen,
    pub show_log: bool,
    pub log: Vec<LogEntry>,
    /// Jobs in flight by source.
    pub loading: BTreeMap<&'static str, usize>,
    pub commits: HashMap<PathBuf, Vec<String>>,
    pub modal: Option<Modal>,
    /// The first `g` of `gg`.
    pub pending_g: bool,
    pub size: (u16, u16),
    /// Seconds since the last input, the last refresh and the last full refresh.
    pub idle: u32,
    pub since_refresh: u32,
    pub since_full: u32,
    pub quit: bool,
}

impl Model {
    pub fn new(size: (u16, u16)) -> Self {
        Self {
            snapshot: Snapshot::default(),
            loaded: false,
            focus: Focus::Panel(Panel::Work),
            panel: Panel::Work,
            sub: List::Workspaces,
            selected: HashMap::new(),
            filters: HashMap::new(),
            filtering: None,
            folded: HashSet::new(),
            scroll: (0, 0),
            screen: Screen::Normal,
            show_log: true,
            log: Vec::new(),
            loading: BTreeMap::new(),
            commits: HashMap::new(),
            modal: None,
            pending_g: false,
            size,
            idle: 0,
            since_refresh: 0,
            since_full: 0,
            quit: false,
        }
    }

    pub fn list(&self, panel: Panel) -> List {
        match panel {
            Panel::Workspaces => self.sub,
            Panel::Work => List::Work,
        }
    }

    /// The list keys act on: the focused panel's, or the last one's from the main view.
    pub fn active(&self) -> List {
        self.list(self.panel)
    }

    pub fn filter(&self, list: List) -> &str {
        self.filters.get(&list).map(Input::value).unwrap_or("")
    }

    pub fn index(&self, list: List) -> usize {
        self.selected.get(&list).copied().unwrap_or(0)
    }

    pub fn push_log(&mut self, entries: impl IntoIterator<Item = LogEntry>) {
        self.log.extend(entries);
        let excess = self.log.len().saturating_sub(LOG_LIMIT);
        self.log.drain(..excess);
    }

    fn matches(&self, list: List, fields: &[&str]) -> bool {
        let filter = self.filter(list).to_lowercase();
        filter.is_empty()
            || fields
                .iter()
                .any(|field| field.to_lowercase().contains(&filter))
    }

    pub fn workspaces(&self) -> Vec<&String> {
        self.snapshot
            .workspaces
            .iter()
            .filter(|name| self.matches(List::Workspaces, &[name]))
            .collect()
    }

    pub fn repos(&self) -> Vec<&Repo> {
        self.snapshot
            .repos
            .iter()
            .filter(|repo| self.matches(List::Repos, &[&repo.name(), &repo.path.to_string_lossy()]))
            .collect()
    }

    /// The workspace whose work panel 2 shows: the one selected in panel 1.
    pub fn workspace(&self) -> Option<&str> {
        let names = self.workspaces();
        names
            .get(self.index(List::Workspaces))
            .or(names.first())
            .map(|name| name.as_str())
            .or(self.snapshot.here.as_deref())
    }

    pub fn repo(&self) -> Option<&Repo> {
        self.repos().get(self.index(List::Repos)).copied()
    }

    /// Panel 2's rows: named groups, foldable, then ungrouped worktrees.
    pub fn lines(&self) -> Vec<Line> {
        let Some(workspace) = self.workspace() else {
            return Vec::new();
        };
        let filtering = !self.filter(List::Work).is_empty();
        let mut members: Vec<usize> = (0..self.snapshot.work.len())
            .filter(|&index| {
                let work = &self.snapshot.work[index];
                work.workspace == workspace
                    && self.matches(
                        List::Work,
                        &[
                            &work.repo_name,
                            &work.branch(),
                            &work.group,
                            &work.path().to_string_lossy(),
                        ],
                    )
            })
            .collect();
        members.sort_by_key(|&index| {
            let work = &self.snapshot.work[index];
            (
                work.group.is_empty(),
                work.group.clone(),
                work.repo_name.clone(),
                !work.tree.main,
                work.branch(),
            )
        });
        let mut lines = Vec::new();
        let mut index = 0;
        while index < members.len() {
            let group = &self.snapshot.work[members[index]].group;
            let end = members[index..]
                .iter()
                .position(|&other| self.snapshot.work[other].group != *group)
                .map_or(members.len(), |offset| index + offset);
            let slice = &members[index..end];
            if group.is_empty() {
                lines.extend(slice.iter().map(|&member| Line::Item(member)));
            } else {
                let key = format!("{workspace}\0{group}");
                let folded = !filtering && self.folded.contains(&key);
                lines.push(Line::Group {
                    key,
                    name: group.clone(),
                    members: slice.to_vec(),
                    folded,
                });
                if !folded {
                    lines.extend(slice.iter().map(|&member| Line::Item(member)));
                }
            }
            index = end;
        }
        lines
    }

    pub fn line(&self) -> Option<Line> {
        self.lines().into_iter().nth(self.index(List::Work))
    }

    /// The selected worktree, or every worktree of the selected group.
    pub fn targets(&self) -> Vec<&Work> {
        match self.line() {
            Some(Line::Item(index)) => vec![&self.snapshot.work[index]],
            Some(Line::Group { members, .. }) => members
                .iter()
                .map(|&index| &self.snapshot.work[index])
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn len(&self, list: List) -> usize {
        match list {
            List::Workspaces => self.workspaces().len(),
            List::Repos => self.repos().len(),
            List::Work => self.lines().len(),
        }
    }
}
