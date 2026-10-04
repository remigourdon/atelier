//! The TUI's model: what is loaded, what is selected and focused, and the keymap.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use tui_input::Input;

use crate::process::Logged;
use crate::reviews::{self, Provider, Review, Role};
use crate::state::Repo;
use crate::worktrunk::{Forge, Worktree};

/// The side panels, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Workspaces,
    Work,
    Reviews,
}

impl Panel {
    pub const ALL: [Panel; 3] = [Panel::Workspaces, Panel::Work, Panel::Reviews];

    pub fn number(self) -> usize {
        Panel::ALL.iter().position(|&p| p == self).unwrap() + 1
    }

    /// Its sub-tabs, which `[` and `]` cycle through.
    pub fn tabs(self) -> &'static [List] {
        match self {
            Panel::Workspaces => &[List::Workspaces, List::Repos],
            Panel::Work => &[List::Work],
            Panel::Reviews => &[List::ToReview, List::Mine],
        }
    }
}

/// The selectable lists, each a sub-tab of a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    Workspaces,
    Repos,
    Work,
    ToReview,
    Mine,
}

impl List {
    pub const ALL: [List; 5] = [
        List::Workspaces,
        List::Repos,
        List::Work,
        List::ToReview,
        List::Mine,
    ];

    pub fn title(self) -> &'static str {
        match self {
            List::Workspaces => "Workspaces",
            List::Repos => "Repos",
            List::Work => "Work",
            List::ToReview => "To review",
            List::Mine => "Mine",
        }
    }

    /// The reviews it lists, for the Reviews panel's sub-tabs.
    pub fn role(self) -> Option<Role> {
        match self {
            List::ToReview => Some(Role::ToReview),
            List::Mine => Some(Role::Mine),
            _ => None,
        }
    }
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
    pub forges: HashMap<PathBuf, Forge>,
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
pub enum Row {
    /// A group header; `members` index `Snapshot::work`.
    Group {
        key: String,
        name: String,
        members: Vec<usize>,
        folded: bool,
    },
    Item(usize),
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
    /// Lists my reviews on each host, from the cache unless `force`.
    Reviews {
        provider: Provider,
        hosts: Vec<String>,
        force: bool,
    },
    /// Checks out a review's branch with `wt switch pr:N` or `mr:N` in its registered repo and
    /// workspace, and focuses its tab.
    Checkout {
        repo: PathBuf,
        workspace: String,
        review: Box<Review>,
    },
}

/// Whether the next worktree listing also lists reviews, and whether from the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewsDue {
    No,
    Cached,
    /// Past the cache, as `R` asks.
    Fresh,
}

/// What the hint bar shows as loading: worktree, commit and review listings, or actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Wt,
    Git,
    Reviews(Provider),
    Run,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Wt => "wt",
            Source::Git => "git",
            Source::Reviews(provider) => provider.cli(),
            Source::Run => "run",
        }
    }
}

impl Job {
    /// The loading indicator it shows in the hint bar.
    pub fn source(&self) -> Source {
        match self {
            Job::Refresh { .. } => Source::Wt,
            Job::Commits(_) => Source::Git,
            Job::Reviews { provider, .. } => Source::Reviews(*provider),
            _ => Source::Run,
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
        log: Vec<Logged>,
    },
    Commits(PathBuf, Vec<String>),
    /// A provider's reviews, replacing the ones listed before.
    Reviews {
        provider: Provider,
        reviews: Result<Vec<Review>, String>,
        log: Vec<Logged>,
    },
    Finished {
        job: Job,
        log: Vec<Logged>,
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
    /// Where `?` lists it.
    pub on: On,
}

/// Where a binding belongs in the `?` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum On {
    /// Movement and scrolling: never listed.
    Nav,
    /// An action on these lists' selection, listed first.
    Lists(&'static [List]),
    /// Anywhere, listed after the panel's actions.
    Global,
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
const REVIEWS: &[List] = &[List::ToReview, List::Mine];
const LOCAL: &[List] = &[List::Workspaces, List::Repos, List::Work];
const ALL: &[List] = &List::ALL;
const NONE: &[List] = &[];

/// The keymap: it drives key handling, the `?` menu and the hint bar.
#[rustfmt::skip]
pub const KEYMAP: &[Binding] = &[
    Binding { keys: &[ch('j'), code(KeyCode::Down)], label: "j/↓", cmd: Cmd::Down, help: "next item", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('k'), code(KeyCode::Up)], label: "k/↑", cmd: Cmd::Up, help: "previous item", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('.')], label: ".", cmd: Cmd::PageDown, help: "next page", hint: NONE, on: On::Nav },
    Binding { keys: &[ch(',')], label: ",", cmd: Cmd::PageUp, help: "previous page", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('<'), code(KeyCode::Home)], label: "</Home/gg", cmd: Cmd::Top, help: "top", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('>'), code(KeyCode::End), ch('G')], label: ">/End/G", cmd: Cmd::Bottom, help: "bottom", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('h'), code(KeyCode::Left), code(KeyCode::BackTab)], label: "h/←/S-Tab", cmd: Cmd::PrevPanel, help: "previous panel", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('l'), code(KeyCode::Right), code(KeyCode::Tab)], label: "l/→/Tab", cmd: Cmd::NextPanel, help: "next panel", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('1')], label: "1", cmd: Cmd::Jump(1), help: "Workspaces │ Repos", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('2')], label: "2", cmd: Cmd::Jump(2), help: "Work", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('3')], label: "3", cmd: Cmd::Jump(3), help: "To review │ Mine", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('0')], label: "0", cmd: Cmd::FocusMain, help: "focus the main view", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('J')], label: "J", cmd: Cmd::ScrollDown, help: "scroll the main view down", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('K')], label: "K", cmd: Cmd::ScrollUp, help: "scroll the main view up", hint: NONE, on: On::Nav },
    Binding { keys: &[ctrl('d'), code(KeyCode::PageDown)], label: "C-d/PgDn", cmd: Cmd::ScrollPageDown, help: "scroll the main view a page down", hint: NONE, on: On::Nav },
    Binding { keys: &[ctrl('u'), code(KeyCode::PageUp)], label: "C-u/PgUp", cmd: Cmd::ScrollPageUp, help: "scroll the main view a page up", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('H')], label: "H", cmd: Cmd::ScrollLeft, help: "scroll the main view left", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('L')], label: "L", cmd: Cmd::ScrollRight, help: "scroll the main view right", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('[')], label: "[", cmd: Cmd::PrevTab, help: "previous sub-tab", hint: NONE, on: On::Lists(&[List::Workspaces, List::Repos, List::ToReview, List::Mine]) },
    Binding { keys: &[ch(']')], label: "]", cmd: Cmd::NextTab, help: "next sub-tab", hint: NONE, on: On::Lists(&[List::Workspaces, List::Repos, List::ToReview, List::Mine]) },
    Binding { keys: &[ch(' ')], label: "Space", cmd: Cmd::Activate, help: "open tab · check out review · switch workspace", hint: &[List::Workspaces, List::Work, List::ToReview, List::Mine], on: On::Lists(&[List::Workspaces, List::Work, List::ToReview, List::Mine]) },
    Binding { keys: &[code(KeyCode::Enter)], label: "Enter", cmd: Cmd::Enter, help: "fold group · focus the main view", hint: NONE, on: On::Lists(WORK) },
    Binding { keys: &[ch('-')], label: "-", cmd: Cmd::CollapseAll, help: "collapse all groups", hint: NONE, on: On::Lists(WORK) },
    Binding { keys: &[ch('=')], label: "=", cmd: Cmd::ExpandAll, help: "expand all groups", hint: NONE, on: On::Lists(WORK) },
    Binding { keys: &[ch('n')], label: "n", cmd: Cmd::New, help: "new worktree · new workspace", hint: &[List::Workspaces, List::Work], on: On::Lists(&[List::Workspaces, List::Work]) },
    Binding { keys: &[ch('e')], label: "e", cmd: Cmd::Edit, help: "edit group · edit repo alias", hint: &[List::Repos, List::Work], on: On::Lists(&[List::Repos, List::Work]) },
    Binding { keys: &[ch('m')], label: "m", cmd: Cmd::Move, help: "move to workspace · set repo workspace", hint: &[List::Repos, List::Work], on: On::Lists(&[List::Repos, List::Work]) },
    Binding { keys: &[ch('d')], label: "d", cmd: Cmd::Remove, help: "remove", hint: LOCAL, on: On::Lists(LOCAL) },
    Binding { keys: &[ch('x')], label: "x", cmd: Cmd::Close, help: "close tab", hint: WORK, on: On::Lists(WORK) },
    Binding { keys: &[ch('p')], label: "p", cmd: Cmd::Pull, help: "pull (git pull --ff-only)", hint: WORK, on: On::Lists(WORK) },
    Binding { keys: &[ch('o')], label: "o", cmd: Cmd::Browse, help: "browse (open in the browser)", hint: REVIEWS, on: On::Lists(&[List::Repos, List::Work, List::ToReview, List::Mine]) },
    Binding { keys: &[ch('y')], label: "y", cmd: Cmd::CopyMenu, help: "copy path, branch or URL", hint: NONE, on: On::Lists(ALL) },
    Binding { keys: &[ctrl('o')], label: "C-o", cmd: Cmd::CopyPath, help: "copy path", hint: NONE, on: On::Lists(ALL) },
    Binding { keys: &[ch('/')], label: "/", cmd: Cmd::Filter, help: "filter", hint: NONE, on: On::Global },
    Binding { keys: &[ch('R')], label: "R", cmd: Cmd::Refresh, help: "refresh", hint: NONE, on: On::Global },
    Binding { keys: &[ch('?')], label: "?", cmd: Cmd::Menu, help: "actions menu", hint: ALL, on: On::Nav },
    Binding { keys: &[ch('+')], label: "+", cmd: Cmd::NextScreen, help: "next screen mode", hint: NONE, on: On::Global },
    Binding { keys: &[ch('_')], label: "_", cmd: Cmd::PrevScreen, help: "previous screen mode", hint: NONE, on: On::Global },
    Binding { keys: &[ch('@')], label: "@", cmd: Cmd::ToggleLog, help: "toggle the command log", hint: NONE, on: On::Global },
    Binding { keys: &[code(KeyCode::Esc)], label: "Esc", cmd: Cmd::Back, help: "back", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('q'), ctrl('c')], label: "q", cmd: Cmd::Quit, help: "quit", hint: ALL, on: On::Global },
];

/// The command bound to a key.
pub fn lookup(key: &KeyEvent) -> Option<Cmd> {
    let pressed = pressed(key);
    KEYMAP
        .iter()
        .find(|binding| binding.keys.contains(&pressed))
        .map(|binding| binding.cmd)
}

/// The popups and inputs that take keys before `KEYMAP` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    Prompt,
    Confirm,
    Menu,
    Filter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupCmd {
    Down,
    Up,
    Top,
    Bottom,
    /// Submit, confirm, run the menu entry or keep the filter.
    Accept,
    /// Close, or clear the filter.
    Cancel,
}

pub struct PopupBinding {
    pub popup: Popup,
    pub keys: &'static [Key],
    pub label: &'static str,
    pub cmd: PopupCmd,
    pub help: &'static str,
}

/// Popup keys. Other keys go to the text input, or pick a menu entry by its key.
#[rustfmt::skip]
pub const POPUP_KEYMAP: &[PopupBinding] = &[
    PopupBinding { popup: Popup::Prompt, keys: &[code(KeyCode::Enter)], label: "Enter", cmd: PopupCmd::Accept, help: "submit" },
    PopupBinding { popup: Popup::Prompt, keys: &[code(KeyCode::Esc)], label: "Esc", cmd: PopupCmd::Cancel, help: "cancel" },
    PopupBinding { popup: Popup::Confirm, keys: &[code(KeyCode::Enter), ch('y')], label: "Enter/y", cmd: PopupCmd::Accept, help: "confirm" },
    PopupBinding { popup: Popup::Confirm, keys: &[code(KeyCode::Esc), ch('n'), ch('q')], label: "Esc/n", cmd: PopupCmd::Cancel, help: "cancel" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('j'), code(KeyCode::Down)], label: "j/↓", cmd: PopupCmd::Down, help: "next" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('k'), code(KeyCode::Up)], label: "k/↑", cmd: PopupCmd::Up, help: "previous" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('<'), code(KeyCode::Home)], label: "</Home", cmd: PopupCmd::Top, help: "top" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('>'), code(KeyCode::End), ch('G')], label: ">/End/G", cmd: PopupCmd::Bottom, help: "bottom" },
    PopupBinding { popup: Popup::Menu, keys: &[code(KeyCode::Enter)], label: "Enter", cmd: PopupCmd::Accept, help: "run" },
    PopupBinding { popup: Popup::Menu, keys: &[code(KeyCode::Esc), ch('q')], label: "Esc", cmd: PopupCmd::Cancel, help: "close" },
    PopupBinding { popup: Popup::Filter, keys: &[code(KeyCode::Enter)], label: "Enter", cmd: PopupCmd::Accept, help: "keep" },
    PopupBinding { popup: Popup::Filter, keys: &[code(KeyCode::Esc)], label: "Esc", cmd: PopupCmd::Cancel, help: "clear" },
];

fn pressed(key: &KeyEvent) -> Key {
    (key.code, key.modifiers.contains(KeyModifiers::CONTROL))
}

pub fn popup_lookup(popup: Popup, key: &KeyEvent) -> Option<PopupCmd> {
    let pressed = pressed(key);
    POPUP_KEYMAP
        .iter()
        .find(|binding| binding.popup == popup && binding.keys.contains(&pressed))
        .map(|binding| binding.cmd)
}

/// How to accept or leave a popup, as its border shows.
pub fn popup_hints(popup: Popup) -> String {
    POPUP_KEYMAP
        .iter()
        .filter(|binding| {
            binding.popup == popup && matches!(binding.cmd, PopupCmd::Accept | PopupCmd::Cancel)
        })
        .map(|binding| format!("{} {}", binding.label, binding.help))
        .collect::<Vec<_>>()
        .join(" · ")
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
    /// Each panel's sub-tab, when not its first.
    pub sub: HashMap<Panel, List>,
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
    pub log: Vec<Logged>,
    /// Jobs in flight by source.
    pub loading: BTreeMap<Source, usize>,
    pub commits: HashMap<PathBuf, Vec<String>>,
    /// Both providers' reviews in both roles, most recently updated first.
    pub reviews: Vec<Review>,
    pub reviews_due: ReviewsDue,
    /// Worktrees with a pull in flight, which show a spinner.
    pub pulling: HashSet<PathBuf>,
    /// The spinner's frame, advanced each tick.
    pub frame: usize,
    pub modal: Option<Modal>,
    /// The first `g` of `gg`.
    pub pending_g: bool,
    pub size: (u16, u16),
    /// Seconds since the last input, the last refresh and the last full refresh.
    pub idle: u32,
    pub since_refresh: u32,
    pub since_full: u32,
}

impl Model {
    pub fn new(size: (u16, u16)) -> Self {
        Self {
            snapshot: Snapshot::default(),
            loaded: false,
            focus: Focus::Panel(Panel::Work),
            panel: Panel::Work,
            sub: HashMap::new(),
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
            reviews: Vec::new(),
            // At startup, so the panel fills.
            reviews_due: ReviewsDue::Cached,
            pulling: HashSet::new(),
            frame: 0,
            modal: None,
            pending_g: false,
            size,
            idle: 0,
            since_refresh: 0,
            since_full: 0,
        }
    }

    /// Whether something on screen moves on each tick.
    pub fn animating(&self) -> bool {
        !self.pulling.is_empty()
    }

    pub fn list(&self, panel: Panel) -> List {
        self.sub.get(&panel).copied().unwrap_or(panel.tabs()[0])
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

    pub fn push_log(&mut self, entries: impl IntoIterator<Item = Logged>) {
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

    /// A list's reviews, narrowed by its filter.
    pub fn reviews(&self, list: List) -> Vec<&Review> {
        self.reviews
            .iter()
            .filter(|review| {
                Some(review.role) == list.role()
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

    /// The registered repo a review belongs to, by its forge web page.
    pub fn review_repo(&self, review: &Review) -> Option<&Repo> {
        let path = self.snapshot.forges.iter().find_map(|(path, forge)| {
            reviews::same_project(&forge.url, &review.project_url).then_some(path)
        })?;
        self.snapshot.repos.iter().find(|repo| repo.path == *path)
    }

    /// The registered repo's name for a review's project, else the project's path.
    pub fn review_project(&self, review: &Review) -> String {
        self.review_repo(review)
            .map_or_else(|| review.project.clone(), Repo::name)
    }

    /// The worktree that has a review's branch checked out.
    pub fn review_work(&self, review: &Review) -> Option<&Work> {
        let repo = self.review_repo(review)?;
        self.snapshot.work.iter().find(|work| {
            work.repo == repo.path && work.tree.branch.as_deref() == Some(review.branch.as_str())
        })
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
    pub fn work_rows(&self) -> Vec<Row> {
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
                lines.extend(slice.iter().map(|&member| Row::Item(member)));
            } else {
                let key = format!("{workspace}\0{group}");
                let folded = !filtering && self.folded.contains(&key);
                lines.push(Row::Group {
                    key,
                    name: group.clone(),
                    members: slice.to_vec(),
                    folded,
                });
                if !folded {
                    lines.extend(slice.iter().map(|&member| Row::Item(member)));
                }
            }
            index = end;
        }
        lines
    }

    pub fn work_row(&self) -> Option<Row> {
        self.work_rows().into_iter().nth(self.index(List::Work))
    }

    /// The selected worktree, or every worktree of the selected group.
    pub fn targets(&self) -> Vec<&Work> {
        match self.work_row() {
            Some(Row::Item(index)) => vec![&self.snapshot.work[index]],
            Some(Row::Group { members, .. }) => members
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
            List::Work => self.work_rows().len(),
            List::ToReview | List::Mine => self.reviews(list).len(),
        }
    }
}
