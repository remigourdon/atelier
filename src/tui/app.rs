//! The TUI's model: what is loaded, what is selected and focused, and the keymap.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use tui_input::Input;

use super::lists;
pub use super::lists::carnets::Search;
pub use super::lists::work::Row;
use super::schedule::Schedule;
use crate::carnet::Stamp;
use crate::finish::{Plan, Scope, Step};
use crate::git::Commit;
use crate::issues::{self, Issue, TrackerConfig};
pub use crate::items::{Removal, Snapshot, Work, WorkKind};
use crate::links::{Group, IssueKeys};
use crate::process::Logged;
use crate::reviews::{Provider, Review};

/// The side panels, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Workspaces,
    Work,
    Reviews,
    Issues,
}

impl Panel {
    pub const ALL: [Panel; 4] = [
        Panel::Workspaces,
        Panel::Work,
        Panel::Reviews,
        Panel::Issues,
    ];

    pub fn number(self) -> usize {
        Panel::ALL.iter().position(|&p| p == self).unwrap() + 1
    }

    /// Its sub-tabs, which `[` and `]` cycle through, given how many issue sections there are
    /// and whether carnets are enabled.
    pub fn tabs(self, sections: usize, carnets: bool) -> Vec<List> {
        match self {
            Panel::Workspaces => vec![List::Workspaces, List::Repos],
            Panel::Work if carnets => vec![List::Work, List::Carnets],
            Panel::Work => vec![List::Work],
            Panel::Reviews => vec![List::ToReview, List::Mine],
            Panel::Issues => (0..sections).map(List::Section).collect(),
        }
    }
}

/// The selectable lists, each a sub-tab of a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    Workspaces,
    Repos,
    Work,
    /// Every carnet, closed ones included.
    Carnets,
    ToReview,
    Mine,
    /// An Issues sub-tab, by its index in the tracker's sections.
    Section(usize),
}

/// What a list holds, as the keymap names lists: both review lists hold reviews, and every
/// section holds issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Workspaces,
    Repos,
    Work,
    Carnets,
    Reviews,
    Issues,
}

impl List {
    pub fn kind(self) -> Kind {
        lists::of(self).kind()
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

/// Background work, run off the UI thread; each reports back with actions.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    Refresh {
        full: bool,
    },
    Commits(PathBuf),
    /// Reads a carnet's README.
    Readme(PathBuf),
    Open(Vec<PathBuf>),
    Close(Vec<PathBuf>),
    Pull(Vec<PathBuf>),
    Create {
        repo: PathBuf,
        branch: String,
        workspace: String,
        group: Option<Group>,
    },
    Remove(Vec<Removal>),
    /// Closes carnets and their tabs.
    CloseCarnet(Vec<PathBuf>),
    /// Reopens closed carnets.
    ReopenCarnet(Vec<PathBuf>),
    /// Searches inside every carnet with `rg`.
    SearchCarnets(String),
    /// Creates a carnet and opens its tab.
    NewCarnet {
        name: String,
        workspace: String,
        group: Option<Group>,
    },
    Move {
        paths: Vec<PathBuf>,
        workspace: String,
    },
    Regroup {
        paths: Vec<PathBuf>,
        group: Option<Group>,
    },
    /// Replaces the issue keys an item links.
    SetIssueKeys {
        path: PathBuf,
        issue_keys: IssueKeys,
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
    /// Removes a workspace; its carnets move to the default workspace.
    RemoveWorkspace(String),
    SwitchWorkspace(String),
    Browse(String),
    /// Saves the current command log, captured when requested.
    ExportLog(Vec<Logged>),
    /// Lists a feed in each of its keys, hosts or scopes, from the cache unless `force`.
    Fetch {
        feed: Feed,
        keys: Vec<String>,
        force: bool,
    },
    /// Fetches the scope's repos, then builds its finish plan from a fresh listing.
    Plan {
        scope: Scope,
        repos: Vec<PathBuf>,
    },
    /// Runs a finish plan's checked steps.
    Finish(Vec<Step>),
    /// Checks out a review's branch with `wt switch pr:N` or `mr:N` in its registered repo and
    /// workspace, and focuses its tab. A new worktree is in `group`.
    Checkout {
        repo: PathBuf,
        workspace: String,
        review: Box<Review>,
        group: Option<Group>,
    },
    /// Creates a worktree on `branch` for an issue, linking its key, in `group`.
    Start {
        repo: PathBuf,
        branch: String,
        workspace: String,
        issue: Box<Issue>,
        group: Option<Group>,
    },
    /// Looks up the one group among the items linking a pending worktree's issue keys.
    LinkedGroup(Pending),
}

/// A worktree for an issue or a review, waiting for its group.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Start {
        repo: PathBuf,
        branch: String,
        workspace: String,
        issue: Box<Issue>,
    },
    Checkout {
        repo: PathBuf,
        workspace: String,
        review: Box<Review>,
    },
}

impl Pending {
    /// The job that makes it, in `group`.
    pub fn job(self, group: Option<Group>) -> Job {
        match self {
            Pending::Start {
                repo,
                branch,
                workspace,
                issue,
            } => Job::Start {
                repo,
                branch,
                workspace,
                issue,
                group,
            },
            Pending::Checkout {
                repo,
                workspace,
                review,
            } => Job::Checkout {
                repo,
                workspace,
                review,
                group,
            },
        }
    }
}

/// A remote listing: a provider's reviews or a tracker's issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feed {
    Reviews(Provider),
    Issues(issues::Tracker),
}

impl Feed {
    pub const ALL: [Feed; 4] = [
        Feed::Reviews(Provider::GitHub),
        Feed::Reviews(Provider::GitLab),
        Feed::Issues(issues::Tracker::GitHub),
        Feed::Issues(issues::Tracker::Jira),
    ];

    /// The CLI it is listed through.
    pub fn cli(self) -> &'static str {
        match self {
            Feed::Reviews(provider) => provider.cli(),
            Feed::Issues(tracker) => tracker.cli(),
        }
    }

    /// How the command log names its listing.
    pub fn what(self) -> String {
        match self {
            Feed::Reviews(provider) => format!("{} reviews", provider.cli()),
            Feed::Issues(tracker) => format!("{} issues", tracker.cli()),
        }
    }
}

/// A feed's listing.
#[derive(Debug, Clone, PartialEq)]
pub enum Rows {
    Reviews(Vec<Review>),
    Issues(Vec<Issue>),
}

/// What the hint bar shows as loading: worktree, commit and feed listings, or actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Wt,
    Git,
    Feed(Feed),
    Run,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Wt => "wt",
            Source::Git => "git",
            Source::Feed(feed) => feed.cli(),
            Source::Run => "run",
        }
    }
}

impl Job {
    /// The loading indicator it shows in the hint bar.
    pub fn source(&self) -> Source {
        match self {
            Job::Refresh { .. } => Source::Wt,
            Job::Commits(_) | Job::Readme(_) => Source::Git,
            Job::Fetch { feed, .. } => Source::Feed(*feed),
            _ => Source::Run,
        }
    }

    /// Whether it changes items, workspaces, repos or tabs, so a refresh shows it once done.
    pub fn changes_items(&self) -> bool {
        !matches!(
            self,
            Job::Browse(_) | Job::SwitchWorkspace(_) | Job::ExportLog(_) | Job::LinkedGroup(_)
        )
    }
}

/// What `update` asks the loop to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Run(Job),
    /// Attach to a session from outside zellij, handing it the terminal.
    Attach(String),
    /// Run the configured tool in an item's directory; a carnet's branch is read when it runs.
    Tool {
        path: PathBuf,
        branch: Option<String>,
    },
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
        group: Option<Group>,
    },
    /// A branch for an issue's worktree.
    Start {
        repo: PathBuf,
        workspace: String,
        issue: Box<Issue>,
    },
    /// A new carnet's name.
    Carnet {
        workspace: String,
        group: Option<Group>,
    },
    Group(Vec<PathBuf>),
    /// An item's issue keys, comma-separated.
    IssueKeys(PathBuf),
    /// The group of a pending worktree, empty for none.
    Join(Pending),
    Alias(PathBuf),
    Workspace,
    /// The text to search the carnets for.
    Search,
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
        completion: Completion,
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
    /// A finish plan, its lines toggled before running; `selected` indexes its lines.
    Finish { plan: Plan, selected: usize },
}

/// What `Tab` completes a prompt's text to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completion {
    pub options: Vec<String>,
    /// While `Tab` cycles: the text typed before it, and the match shown.
    pub cycle: Option<(String, usize)>,
}

impl Completion {
    pub fn new(options: Vec<String>) -> Self {
        Self {
            options,
            cycle: None,
        }
    }

    /// The options starting with `typed`, ignoring case.
    pub fn matches(&self, typed: &str) -> Vec<&str> {
        let typed = typed.trim().to_uppercase();
        (self.options.iter())
            .filter(|option| option.to_uppercase().starts_with(&typed))
            .map(String::as_str)
            .collect()
    }

    /// The next match for `text`, cycling through the matches of what was typed before the
    /// first `Tab`; `None` when nothing matches.
    pub fn next(&mut self, text: &str) -> Option<String> {
        let (typed, at) = match self.cycle.take() {
            Some((typed, at)) => (typed, at + 1),
            None => (text.to_owned(), 0),
        };
        let matches = self.matches(&typed);
        if matches.is_empty() {
            return None;
        }
        let at = at % matches.len();
        let chosen = matches[at].to_owned();
        self.cycle = Some((typed, at));
        Some(chosen)
    }
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
        /// What `Tab` completes the text to.
        completions: Vec<String>,
    },
    Copy(String),
    /// Commands that ran outside any job, for the command log.
    Logged(Vec<Logged>),
    Loaded {
        snapshot: Result<Snapshot, String>,
        /// Whether it was a full refresh.
        full: bool,
        log: Vec<Logged>,
    },
    Commits(PathBuf, Vec<Commit>),
    Readme(Readme),
    /// A carnet search's hit lines by carnet, `None` when it failed.
    Searched {
        text: String,
        hits: Option<BTreeMap<PathBuf, Vec<String>>>,
        log: Vec<Logged>,
    },
    /// A finish plan, built once its repos were fetched.
    Planned {
        plan: Result<Plan, String>,
        log: Vec<Logged>,
    },
    /// The one group linked to a pending worktree's issue keys, if there is one.
    Linked {
        pending: Pending,
        group: Result<Option<Group>, String>,
        log: Vec<Logged>,
    },
    /// A feed's rows, replacing the ones it listed before.
    Fetched {
        feed: Feed,
        rows: Result<Rows, String>,
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
    Link,
    Move,
    Remove,
    Close,
    Pull,
    Finish,
    ToggleCarnet,
    Tool,
    Search,
    Browse,
    CopyMenu,
    CopyPath,
    Filter,
    Refresh,
    Menu,
    NextScreen,
    PrevScreen,
    ToggleLog,
    ExportLog,
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
    /// The kinds of list whose hint bar shows it.
    pub hint: &'static [Kind],
    /// Where `?` lists it.
    pub on: On,
}

/// Where a binding belongs in the `?` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum On {
    /// Movement and scrolling: never listed.
    Nav,
    /// An action on the selection of these kinds of list, listed first.
    Lists(&'static [Kind]),
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

const WORK: &[Kind] = &[Kind::Work];
const CARNETS: &[Kind] = &[Kind::Work, Kind::Carnets];
const LOCAL: &[Kind] = &[Kind::Workspaces, Kind::Repos, Kind::Work];
const TABBED: &[Kind] = &[
    Kind::Workspaces,
    Kind::Repos,
    Kind::Carnets,
    Kind::Reviews,
    Kind::Issues,
];
const REMOTE: &[Kind] = &[Kind::Reviews, Kind::Issues];
const ALL: &[Kind] = &[
    Kind::Workspaces,
    Kind::Repos,
    Kind::Work,
    Kind::Carnets,
    Kind::Reviews,
    Kind::Issues,
];
const TABS: &[Kind] = &[Kind::Work, Kind::Carnets, Kind::Reviews, Kind::Issues];
const FINISH: &[Kind] = &[Kind::Workspaces, Kind::Work, Kind::Issues];
const NONE: &[Kind] = &[];

/// The keymap: it drives key handling, the `?` menu and the hint bar.
#[rustfmt::skip]
pub const KEYMAP: &[Binding] = &[
    Binding { keys: &[ch('j'), code(KeyCode::Down)], label: "j/↓", cmd: Cmd::Down, help: "next item", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('k'), code(KeyCode::Up)], label: "k/↑", cmd: Cmd::Up, help: "previous item", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('.')], label: ".", cmd: Cmd::PageDown, help: "next page", hint: NONE, on: On::Nav },
    Binding { keys: &[ch(',')], label: ",", cmd: Cmd::PageUp, help: "previous page", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('<'), code(KeyCode::Home)], label: "</Home", cmd: Cmd::Top, help: "top", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('>'), code(KeyCode::End)], label: ">/End", cmd: Cmd::Bottom, help: "bottom", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('h'), code(KeyCode::Left), code(KeyCode::BackTab)], label: "h/←/S-Tab", cmd: Cmd::PrevPanel, help: "previous panel", hint: NONE, on: On::Nav },
    Binding { keys: &[code(KeyCode::Right), code(KeyCode::Tab)], label: "→/Tab", cmd: Cmd::NextPanel, help: "next panel", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('1')], label: "1", cmd: Cmd::Jump(1), help: "Workspaces │ Repos", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('2')], label: "2", cmd: Cmd::Jump(2), help: "Work │ Carnets", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('3')], label: "3", cmd: Cmd::Jump(3), help: "To review │ Mine", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('4')], label: "4", cmd: Cmd::Jump(4), help: "Issues", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('0')], label: "0", cmd: Cmd::FocusMain, help: "focus the main view", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('J')], label: "J", cmd: Cmd::ScrollDown, help: "scroll the main view down", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('K')], label: "K", cmd: Cmd::ScrollUp, help: "scroll the main view up", hint: NONE, on: On::Nav },
    Binding { keys: &[ctrl('d'), code(KeyCode::PageDown)], label: "C-d/PgDn", cmd: Cmd::ScrollPageDown, help: "scroll the main view a page down", hint: NONE, on: On::Nav },
    Binding { keys: &[ctrl('u'), code(KeyCode::PageUp)], label: "C-u/PgUp", cmd: Cmd::ScrollPageUp, help: "scroll the main view a page up", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('H')], label: "H", cmd: Cmd::ScrollLeft, help: "scroll the main view left", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('L')], label: "L", cmd: Cmd::ScrollRight, help: "scroll the main view right", hint: NONE, on: On::Nav },
    Binding { keys: &[ch('[')], label: "[", cmd: Cmd::PrevTab, help: "previous sub-tab", hint: NONE, on: On::Lists(TABBED) },
    Binding { keys: &[ch(']')], label: "]", cmd: Cmd::NextTab, help: "next sub-tab", hint: NONE, on: On::Lists(TABBED) },
    Binding { keys: &[ch(' ')], label: "Space", cmd: Cmd::Activate, help: "open tab · check out review · start issue · switch workspace", hint: &[Kind::Workspaces, Kind::Work, Kind::Carnets, Kind::Reviews, Kind::Issues], on: On::Lists(&[Kind::Workspaces, Kind::Work, Kind::Carnets, Kind::Reviews, Kind::Issues]) },
    Binding { keys: &[code(KeyCode::Enter)], label: "Enter", cmd: Cmd::Enter, help: "fold group · focus the main view", hint: NONE, on: On::Lists(WORK) },
    Binding { keys: &[ch('-')], label: "-", cmd: Cmd::CollapseAll, help: "collapse all groups", hint: NONE, on: On::Lists(WORK) },
    Binding { keys: &[ch('=')], label: "=", cmd: Cmd::ExpandAll, help: "expand all groups", hint: NONE, on: On::Lists(WORK) },
    Binding { keys: &[ch('n')], label: "n", cmd: Cmd::New, help: "new worktree or carnet · new workspace", hint: &[Kind::Workspaces, Kind::Work, Kind::Issues], on: On::Lists(&[Kind::Workspaces, Kind::Work, Kind::Issues]) },
    Binding { keys: &[ch('e')], label: "e", cmd: Cmd::Edit, help: "edit group · edit repo alias", hint: &[Kind::Repos, Kind::Work], on: On::Lists(&[Kind::Repos, Kind::Work]) },
    Binding { keys: &[ch('l')], label: "l", cmd: Cmd::Link, help: "link issue keys", hint: CARNETS, on: On::Lists(CARNETS) },
    Binding { keys: &[ch('m')], label: "m", cmd: Cmd::Move, help: "move to workspace · set repo workspace", hint: &[Kind::Repos, Kind::Work], on: On::Lists(&[Kind::Repos, Kind::Work]) },
    Binding { keys: &[ch('d')], label: "d", cmd: Cmd::Remove, help: "remove", hint: LOCAL, on: On::Lists(LOCAL) },
    Binding { keys: &[ch('x')], label: "x", cmd: Cmd::Close, help: "close tab", hint: TABS, on: On::Lists(TABS) },
    Binding { keys: &[ch('c')], label: "c", cmd: Cmd::ToggleCarnet, help: "close or reopen carnet", hint: CARNETS, on: On::Lists(CARNETS) },
    Binding { keys: &[ch('g')], label: "g", cmd: Cmd::Tool, help: "open tool", hint: CARNETS, on: On::Lists(CARNETS) },
    Binding { keys: &[ch('s')], label: "s", cmd: Cmd::Search, help: "search inside carnets (rg)", hint: &[Kind::Carnets], on: On::Lists(&[Kind::Carnets]) },
    Binding { keys: &[ch('p')], label: "p", cmd: Cmd::Pull, help: "pull (git pull --ff-only --prune)", hint: WORK, on: On::Lists(WORK) },
    Binding { keys: &[ch('f')], label: "f", cmd: Cmd::Finish, help: "finish merged work: remove worktrees, close carnet, pull main", hint: FINISH, on: On::Lists(FINISH) },
    Binding { keys: &[ch('o')], label: "o", cmd: Cmd::Browse, help: "browse (open in the browser)", hint: REMOTE, on: On::Lists(&[Kind::Repos, Kind::Work, Kind::Reviews, Kind::Issues]) },
    Binding { keys: &[ch('y')], label: "y", cmd: Cmd::CopyMenu, help: "copy path, branch or URL", hint: NONE, on: On::Lists(ALL) },
    Binding { keys: &[ctrl('o')], label: "C-o", cmd: Cmd::CopyPath, help: "copy path", hint: NONE, on: On::Lists(ALL) },
    Binding { keys: &[ch('/')], label: "/", cmd: Cmd::Filter, help: "filter", hint: NONE, on: On::Global },
    Binding { keys: &[ch('R')], label: "R", cmd: Cmd::Refresh, help: "refresh", hint: NONE, on: On::Global },
    Binding { keys: &[ch('?')], label: "?", cmd: Cmd::Menu, help: "actions menu", hint: ALL, on: On::Nav },
    Binding { keys: &[ch('+')], label: "+", cmd: Cmd::NextScreen, help: "next screen mode", hint: NONE, on: On::Global },
    Binding { keys: &[ch('_')], label: "_", cmd: Cmd::PrevScreen, help: "previous screen mode", hint: NONE, on: On::Global },
    Binding { keys: &[ch('@')], label: "@", cmd: Cmd::ToggleLog, help: "toggle the command log", hint: NONE, on: On::Global },
    Binding { keys: &[ch('E')], label: "E", cmd: Cmd::ExportLog, help: "export the command log to a file", hint: NONE, on: On::Global },
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
    Finish,
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
    /// Check or uncheck a line.
    Toggle,
    /// Complete the prompt's text.
    Complete,
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
    PopupBinding { popup: Popup::Prompt, keys: &[code(KeyCode::Tab)], label: "Tab", cmd: PopupCmd::Complete, help: "complete" },
    PopupBinding { popup: Popup::Confirm, keys: &[code(KeyCode::Enter), ch('y')], label: "Enter/y", cmd: PopupCmd::Accept, help: "confirm" },
    PopupBinding { popup: Popup::Confirm, keys: &[code(KeyCode::Esc), ch('n'), ch('q')], label: "Esc/n", cmd: PopupCmd::Cancel, help: "cancel" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('j'), code(KeyCode::Down)], label: "j/↓", cmd: PopupCmd::Down, help: "next" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('k'), code(KeyCode::Up)], label: "k/↑", cmd: PopupCmd::Up, help: "previous" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('<'), code(KeyCode::Home)], label: "</Home", cmd: PopupCmd::Top, help: "top" },
    PopupBinding { popup: Popup::Menu, keys: &[ch('>'), code(KeyCode::End)], label: ">/End", cmd: PopupCmd::Bottom, help: "bottom" },
    PopupBinding { popup: Popup::Menu, keys: &[code(KeyCode::Enter)], label: "Enter", cmd: PopupCmd::Accept, help: "run" },
    PopupBinding { popup: Popup::Menu, keys: &[code(KeyCode::Esc), ch('q')], label: "Esc", cmd: PopupCmd::Cancel, help: "close" },
    PopupBinding { popup: Popup::Finish, keys: &[ch('j'), code(KeyCode::Down)], label: "j/↓", cmd: PopupCmd::Down, help: "next" },
    PopupBinding { popup: Popup::Finish, keys: &[ch('k'), code(KeyCode::Up)], label: "k/↑", cmd: PopupCmd::Up, help: "previous" },
    PopupBinding { popup: Popup::Finish, keys: &[ch(' ')], label: "Space", cmd: PopupCmd::Toggle, help: "toggle" },
    PopupBinding { popup: Popup::Finish, keys: &[code(KeyCode::Enter)], label: "Enter", cmd: PopupCmd::Accept, help: "run" },
    PopupBinding { popup: Popup::Finish, keys: &[code(KeyCode::Esc), ch('q')], label: "Esc", cmd: PopupCmd::Cancel, help: "cancel" },
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
    hints(popup, |_| String::new())
}

/// A finish plan's keys, counting the lines `Enter` runs.
pub fn finish_hints(checked: usize) -> String {
    hints(Popup::Finish, |cmd| {
        if cmd == PopupCmd::Accept {
            format!(" {checked}")
        } else {
            String::new()
        }
    })
}

fn hints(popup: Popup, suffix: impl Fn(PopupCmd) -> String) -> String {
    POPUP_KEYMAP
        .iter()
        .filter(|binding| {
            binding.popup == popup
                && matches!(
                    binding.cmd,
                    PopupCmd::Toggle | PopupCmd::Accept | PopupCmd::Cancel
                )
        })
        .map(|binding| format!("{} {}{}", binding.label, binding.help, suffix(binding.cmd)))
        .collect::<Vec<_>>()
        .join(" · ")
}

const LOG_LIMIT: usize = 500;

/// A carnet's README as a job read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Readme {
    pub path: PathBuf,
    /// Taken before reading, so a README written meanwhile reads again at the next refresh.
    pub stamp: Option<Stamp>,
    /// `None` when the carnet has none.
    pub text: Option<String>,
}

pub struct Model {
    pub snapshot: Snapshot,
    pub loaded: bool,
    /// Whether `[carnets]` is configured, so `n` offers one.
    pub carnets: bool,
    pub focus: Focus,
    /// The side panel focus returns to from the main view.
    pub panel: Panel,
    /// Each panel's sub-tab, when not its first.
    pub sub: HashMap<Panel, List>,
    pub selected: HashMap<List, usize>,
    pub filters: HashMap<List, Input>,
    /// The list whose filter is being typed.
    pub filtering: Option<List>,
    /// The keys of the folded groups.
    pub folded: HashSet<String>,
    /// The main view's vertical and horizontal scroll.
    pub scroll: (u16, u16),
    pub screen: Screen,
    pub show_log: bool,
    pub log: Vec<Logged>,
    /// What is loading, and when to refresh.
    pub schedule: Schedule,
    pub commits: HashMap<PathBuf, Vec<Commit>>,
    /// The selected carnet's README, the only one kept, shown until read again.
    pub readme: Option<Readme>,
    /// Both providers' reviews in both roles, most recently updated first.
    pub reviews: Vec<Review>,
    /// Where issues come from and their sections.
    pub tracker_config: TrackerConfig,
    pub review_config: crate::reviews::ReviewConfig,
    /// Every tracker's issues, each source in its own order.
    pub issues: Vec<Issue>,
    /// The carnet search narrowing the Carnets list, until `Esc`.
    pub search: Option<Search>,
    /// The spinner's frame, advanced each tick.
    pub frame: usize,
    pub modal: Option<Modal>,
    pub size: (u16, u16),
}

impl Model {
    pub fn new(size: (u16, u16)) -> Self {
        Self {
            snapshot: Snapshot::default(),
            loaded: false,
            carnets: false,
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
            schedule: Schedule::default(),
            commits: HashMap::new(),
            readme: None,
            reviews: Vec::new(),
            tracker_config: TrackerConfig::default(),
            review_config: crate::reviews::ReviewConfig::default(),
            issues: Vec::new(),
            search: None,
            frame: 0,
            modal: None,
            size,
        }
    }

    /// Whether something on screen moves on each tick.
    pub fn animating(&self) -> bool {
        self.schedule.animating()
    }

    /// A panel's sub-tabs. Issues have one per section, then Other while it lists any.
    pub fn tabs(&self, panel: Panel) -> Vec<List> {
        let mut sections = self.tracker_config.sections().len();
        let other = Some(sections);
        if self
            .issues
            .iter()
            .any(|issue| self.tracker_config.section(issue) == other)
        {
            sections += 1;
        }
        panel.tabs(sections, self.carnets)
    }

    /// Every list, sub-tabs included.
    pub fn lists(&self) -> Vec<List> {
        Panel::ALL
            .into_iter()
            .flat_map(|panel| self.tabs(panel))
            .collect()
    }

    /// A panel's chosen sub-tab, else its first, as when Other empties.
    pub fn list(&self, panel: Panel) -> List {
        let tabs = self.tabs(panel);
        (self.sub.get(&panel).copied())
            .filter(|list| tabs.contains(list))
            .unwrap_or(tabs[0])
    }

    pub fn title(&self, list: List) -> &str {
        lists::of(list).title(self, list)
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

    pub(super) fn matches(&self, list: List, fields: &[&str]) -> bool {
        let filter = self.filter(list).to_lowercase();
        filter.is_empty()
            || fields
                .iter()
                .any(|field| field.to_lowercase().contains(&filter))
    }

    pub fn len(&self, list: List) -> usize {
        lists::of(list).len(self, list)
    }
}
