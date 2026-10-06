//! `atelier context`: what atelier knows about a directory or an issue key, for scripts and
//! agents.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use color_eyre::eyre::Result;
use serde::Serialize;

use crate::carnet::{self, Carnet, Names};
use crate::config::Config;
use crate::finish::{self, Signal};
use crate::git;
use crate::issues::{self, Issue};
use crate::items::canonical;
use crate::process::Runner;
use crate::state::{Item, ItemKind, State, dir_name};
use crate::worktrunk::{self, CiState, Decision, Statusline, Worktree};

/// What to describe: the item holding a directory, or an issue key.
pub enum Target<'a> {
    Dir(&'a Path),
    Key(&'a str),
}

/// Everything atelier knows about a directory's item and its group. Read from the database,
/// the carnet folders, the issue cache and `wt list`: nothing is written.
#[derive(Debug, Serialize)]
pub struct Context {
    /// The worktree or carnet holding the directory; `None` for an issue key or a directory
    /// in no item atelier recorded.
    pub item: Option<ItemInfo>,
    /// The item's workspace.
    pub workspace: Option<Workspace>,
    /// The item's group; `None` when it has none.
    pub group: Option<String>,
    /// The issues the item links, in order, or the issue key described.
    pub issue_keys: Vec<String>,
    /// The first key's issue as last cached; `None` when it is not a cached issue.
    pub issue: Option<CachedIssue>,
    /// The worktrees in the group, across repos; for an issue key, those linking it.
    pub worktrees: Vec<WorktreeInfo>,
    /// The carnets in the group or linking the first key, closed ones included, newest first.
    pub carnets: Vec<CarnetInfo>,
    /// The newest open one.
    pub carnet: Option<PathBuf>,
}

/// The item described.
#[derive(Debug, Serialize)]
pub struct ItemInfo {
    pub path: PathBuf,
    pub kind: ItemKind,
    /// A worktree's repo.
    pub repo: Option<PathBuf>,
    /// A worktree's branch, `None` when detached or not listed.
    pub branch: Option<String>,
}

/// The item's workspace.
#[derive(Debug, Serialize)]
pub struct Workspace {
    pub name: String,
    /// Whether this process runs in its zellij session.
    pub current: bool,
    /// The zellij session of the item's recorded tab.
    pub session: Option<String>,
}

/// An issue as the cache holds it.
#[derive(Debug, Serialize)]
pub struct CachedIssue {
    #[serde(flatten)]
    pub issue: Issue,
    /// When the listing holding it was fetched, `YYYY-MM-DD HH:MM:SS` UTC.
    pub cached_at: String,
}

/// A worktree in the group.
#[derive(Debug, Serialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    /// The repo's alias, else its directory name.
    pub repo: String,
    /// The repo's main worktree.
    pub repo_path: PathBuf,
    pub branch: Option<String>,
    pub workspace: String,
    /// The zellij session of its recorded tab.
    pub session: Option<String>,
    /// Whether it holds the directory described.
    pub current: bool,
    /// What `wt list` reports; `None` when it failed or did not list this worktree.
    pub status: Option<Status>,
    /// Why there is no status.
    pub error: Option<String>,
}

/// A worktree's state as `wt list` reports it.
#[derive(Debug, Serialize)]
pub struct Status {
    pub main: bool,
    pub dirty: bool,
    /// Lines added and deleted in the working tree.
    pub added: u64,
    pub deleted: u64,
    pub upstream: Option<Upstream>,
    pub head: Head,
    /// worktrunk's compact status, such as `!?↑`.
    pub symbols: String,
    /// The branch's CI; only for the worktree holding the directory, from `wt list statusline`.
    pub ci: Option<CiStatus>,
    /// Its open review, as worktrunk finds it; only for the worktree holding the directory.
    pub review: Option<Review>,
    /// Why it is finished: `integrated`, or `upstream_gone`, which is only checked for the
    /// worktree holding the directory.
    pub finished: Option<Signal>,
}

/// A branch's CI, as worktrunk's CI column reports it.
#[derive(Debug, Serialize)]
pub struct CiStatus {
    /// `passed`, `running`, `failed`, `conflicts`, `error`, `changes_requested` or
    /// `approval_pending`.
    pub state: CiState,
    /// The status is of an older commit than local HEAD.
    pub stale: bool,
    /// The checks are of the branch's own workflow: it has no review.
    pub branch_workflow: bool,
}

/// A branch's open review.
#[derive(Debug, Serialize)]
pub struct Review {
    pub number: Option<u64>,
    pub url: Option<String>,
    /// `changes_requested`, `pending`, `draft` or `approved`.
    pub decision: Option<Decision>,
}

/// Commits ahead of and behind the upstream branch.
#[derive(Debug, Serialize)]
pub struct Upstream {
    pub ahead: u64,
    pub behind: u64,
}

/// The commit checked out.
#[derive(Debug, Serialize)]
pub struct Head {
    /// Short.
    pub sha: String,
    pub subject: String,
    pub committed_at: String,
}

impl From<&Worktree> for Status {
    fn from(tree: &Worktree) -> Self {
        Self {
            main: tree.main,
            dirty: tree.dirty,
            added: tree.diff.0,
            deleted: tree.diff.1,
            upstream: (tree.upstream).map(|(ahead, behind)| Upstream { ahead, behind }),
            head: Head {
                sha: tree.short_sha.clone(),
                subject: tree.subject.clone(),
                committed_at: tree.committed_at.clone(),
            },
            symbols: tree.symbols.clone(),
            ci: tree.ci.as_ref().map(|ci| CiStatus {
                state: ci.state,
                stale: ci.stale,
                branch_workflow: ci.branch_workflow,
            }),
            review: (tree.ci.as_ref().and_then(|ci| ci.review.as_ref())).map(|review| Review {
                number: review.number,
                url: review.url.clone(),
                decision: review.decision,
            }),
            finished: finish::tree_signal(tree),
        }
    }
}

/// A carnet in the group or linking its issue.
#[derive(Debug, Serialize)]
pub struct CarnetInfo {
    pub path: PathBuf,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The folder name after its date.
    pub name: String,
    pub group: String,
    pub issue_keys: Vec<String>,
    pub closed: bool,
    pub summary: String,
    pub workspace: String,
    /// The zellij session of its recorded tab.
    pub session: Option<String>,
}

impl CarnetInfo {
    fn new(carnet: &Carnet, workspace: String, session: Option<String>) -> Self {
        Self {
            path: carnet.path.clone(),
            date: carnet.date.clone(),
            name: carnet.name.clone(),
            group: carnet.group.clone(),
            issue_keys: carnet.issue_keys.clone(),
            closed: carnet.closed,
            summary: carnet.summary.clone(),
            workspace,
            session,
        }
    }
}

/// What the database records, read once.
struct Records {
    items: Vec<Item>,
    /// Each recorded tab's session, by item path.
    sessions: HashMap<PathBuf, String>,
    /// Each repo's name, by path.
    repos: HashMap<PathBuf, String>,
    default_workspace: String,
}

impl Records {
    fn read(state: &State) -> Result<Self> {
        Ok(Self {
            items: state.items()?,
            sessions: (state.tabs()?.into_iter())
                .map(|tab| (tab.path, tab.session))
                .collect(),
            repos: (state.repos()?.into_iter())
                .map(|repo| (repo.path.clone(), repo.name()))
                .collect(),
            default_workspace: state.default_workspace().to_owned(),
        })
    }

    fn session(&self, path: &Path) -> Option<String> {
        self.sessions.get(path).cloned()
    }

    /// The innermost item holding `dir`.
    fn containing(&self, dir: &Path) -> Option<&Item> {
        let dir = canonical(dir);
        (self.items.iter())
            .filter(|item| dir.starts_with(&item.path))
            .max_by_key(|item| item.path.as_os_str().len())
    }

    /// The worktrees `linked` picks.
    fn worktrees(&self, linked: impl Fn(&Item) -> bool) -> Vec<&Item> {
        (self.items.iter())
            .filter(|item| item.kind == ItemKind::Worktree && linked(item))
            .collect()
    }
}

/// The item holding a directory and its issue keys as `describe` reads them: a carnet's from
/// its folder.
pub struct Located {
    pub item: Item,
    pub issue_keys: Vec<String>,
    /// The carnet's folder, for a carnet item.
    pub carnet: Option<Carnet>,
}

/// The innermost item holding `dir`, `None` when atelier recorded none. Carnets are scanned
/// only when it is one.
pub fn locate(state: &State, config: &Config, dir: &Path) -> Result<Option<Located>> {
    let records = Records::read(state)?;
    let Some(item) = records.containing(dir).cloned() else {
        return Ok(None);
    };
    let carnets = match (item.is_carnet(), config.carnet_root()) {
        (true, Some(root)) => scan(config, &root)?,
        _ => Vec::new(),
    };
    let carnet = carnets.into_iter().find(|carnet| carnet.path == item.path);
    let (_, issue_keys) = links(&item, carnet.as_ref());
    Ok(Some(Located {
        item,
        issue_keys,
        carnet,
    }))
}

/// The carnets under `root`.
fn scan(config: &Config, root: &Path) -> Result<Vec<Carnet>> {
    let names = Names::new(config.issue_key_pattern())?;
    carnet::scan(root, &names, &config.tracker)
}

/// An item's group and issue keys: a carnet's as its folder records them, else the recorded
/// ones.
fn links(item: &Item, carnet: Option<&Carnet>) -> (String, Vec<String>) {
    match carnet {
        Some(carnet) => (carnet.group.clone(), carnet.issue_keys.clone()),
        None => (item.group.clone(), item.issue_keys.clone()),
    }
}

/// The worktree item holding a directory, from `wt list statusline`, its CI included, and
/// whether its upstream is gone; or why there is none. worktrunk's CI lookup is the one
/// network call, cached for a short while in the repo's `.git/wt/`.
pub fn current_tree(runner: &dyn Runner, item: &Item) -> Result<Statusline, String> {
    let mut statusline =
        worktrunk::statusline(runner, &item.path).map_err(|err| format!("{err:#}"))?;
    let tree = &mut statusline.tree;
    tree.path = canonical(&tree.path);
    if let (Some(repo), Some(branch)) = (&item.repo, &tree.branch) {
        tree.gone = git::gone_branches(runner, repo).is_ok_and(|gone| gone.contains(branch));
    }
    Ok(statusline)
}

/// Each repo's worktrees from one `wt list`, with canonical paths as the database records
/// them, or why the listing failed.
struct Listings(HashMap<PathBuf, Result<Vec<Worktree>, String>>);

impl Listings {
    /// Lists each of the items' repos once, without `--full`.
    fn of<'a>(runner: &dyn Runner, items: impl IntoIterator<Item = &'a Item>) -> Self {
        let mut listings = HashMap::new();
        for repo in items.into_iter().filter_map(|item| item.repo.as_ref()) {
            if !listings.contains_key(repo) {
                let listing = (worktrunk::list(runner, repo, false))
                    .map(|listing| {
                        let mut trees = listing.worktrees;
                        for tree in &mut trees {
                            tree.path = canonical(&tree.path);
                        }
                        trees
                    })
                    .map_err(|err| format!("{err:#}"));
                listings.insert(repo.clone(), listing);
            }
        }
        Self(listings)
    }

    /// A worktree item's listing, or why there is none.
    fn find(&self, item: &Item) -> Result<&Worktree, String> {
        let repo = item.repo.as_ref().ok_or("not a worktree")?;
        let trees = (self.0.get(repo))
            .ok_or("not listed")?
            .as_ref()
            .map_err(Clone::clone)?;
        (trees.iter())
            .find(|tree| tree.path == item.path)
            .ok_or_else(|| "not listed by wt list".to_owned())
    }
}

/// Describes `target`. Only worktrunk runs: `wt list statusline` for the worktree holding the
/// directory, and `wt list` once per other repo with a worktree to describe.
pub fn describe(
    state: &State,
    config: &Config,
    runner: &dyn Runner,
    here: Option<&str>,
    target: Target,
) -> Result<Context> {
    let carnets = match config.carnet_root() {
        Some(root) => scan(config, &root)?,
        None => Vec::new(),
    };
    let records = Records::read(state)?;
    let (current, group, keys) = match target {
        Target::Key(key) => (None, String::new(), vec![config.tracker.resolve_key(key)]),
        Target::Dir(dir) => match records.containing(dir) {
            Some(item) => {
                let carnet = (carnets.iter()).find(|carnet| carnet.path == item.path);
                let (group, keys) = links(item, carnet.filter(|_| item.is_carnet()));
                (Some(item), group, keys)
            }
            None => (None, String::new(), Vec::new()),
        },
    };
    let first = keys.first().map_or("", String::as_str);
    let linked = match target {
        Target::Key(_) => records.worktrees(|item| item.issue_keys.iter().any(|key| key == first)),
        Target::Dir(_) => records.worktrees(|item| !group.is_empty() && item.group == group),
    };
    let tree = current
        .filter(|item| item.kind == ItemKind::Worktree)
        .map(|item| Current {
            path: &item.path,
            tree: current_tree(runner, item).map(|statusline| statusline.tree),
        });
    let others = linked.iter().copied();
    let listings = Listings::of(
        runner,
        others.filter(|item| current.is_none_or(|current| current.path != item.path)),
    );
    let issue = match first {
        "" => None,
        key => issues::cached(state, &config.tracker, key)?
            .map(|(issue, cached_at)| CachedIssue { issue, cached_at }),
    };
    let listed: Vec<&Carnet> = (carnets.iter())
        .filter(|carnet| {
            (!group.is_empty() && carnet.group == group)
                || (!first.is_empty() && carnet.issue_keys.iter().any(|key| key == first))
        })
        .collect();
    Ok(Context {
        item: current.map(|item| ItemInfo {
            path: item.path.clone(),
            kind: item.kind,
            repo: item.repo.clone(),
            branch: (tree.as_ref()).and_then(|current| current.tree.as_ref().ok()?.branch.clone()),
        }),
        workspace: current.map(|item| Workspace {
            current: here == Some(item.workspace.as_str()),
            name: item.workspace.clone(),
            session: records.session(&item.path),
        }),
        worktrees: worktrees(&records, &listings, &linked, tree.as_ref()),
        carnets: carnet_infos(&records, &listed),
        carnet: (listed.iter())
            .filter(|carnet| !carnet.closed)
            .max_by(|a, b| a.path.cmp(&b.path))
            .map(|carnet| carnet.path.clone()),
        group: Some(group).filter(|group| !group.is_empty()),
        issue_keys: keys,
        issue,
    })
}

/// The worktree holding the directory, as `wt list statusline` reported it.
struct Current<'a> {
    path: &'a Path,
    tree: Result<Worktree, String>,
}

/// The linked worktrees with what worktrunk reports of them: the current one's statusline,
/// the others' `wt list`.
fn worktrees(
    records: &Records,
    listings: &Listings,
    linked: &[&Item],
    current: Option<&Current>,
) -> Vec<WorktreeInfo> {
    (linked.iter())
        .map(|item| {
            let repo_path = item.repo.clone().unwrap_or_default();
            let holding = current.filter(|current| current.path == item.path);
            let tree = match holding {
                Some(current) => current.tree.as_ref().map_err(Clone::clone),
                None => listings.find(item),
            };
            WorktreeInfo {
                path: item.path.clone(),
                repo: (records.repos.get(&repo_path).cloned())
                    .unwrap_or_else(|| dir_name(&repo_path)),
                repo_path,
                branch: tree.as_ref().ok().and_then(|tree| tree.branch.clone()),
                workspace: item.workspace.clone(),
                session: records.session(&item.path),
                current: holding.is_some(),
                status: tree.as_ref().ok().map(|tree| Status::from(*tree)),
                error: tree.err(),
            }
        })
        .collect()
}

/// The carnets, each in its workspace: the recorded one, else the default workspace.
fn carnet_infos(records: &Records, carnets: &[&Carnet]) -> Vec<CarnetInfo> {
    let workspaces: HashMap<&PathBuf, &str> = (records.items.iter())
        .map(|item| (&item.path, item.workspace.as_str()))
        .collect();
    (carnets.iter())
        .map(|carnet| {
            let workspace =
                (workspaces.get(&carnet.path).copied()).unwrap_or(&records.default_workspace);
            CarnetInfo::new(carnet, workspace.to_owned(), records.session(&carnet.path))
        })
        .collect()
}

/// The context as lines for a person to read.
pub fn render(context: &Context) -> String {
    let mut out = String::new();
    let mut line = |label: &str, text: String| {
        let _ = writeln!(out, "{label:<10} {text}");
    };
    match &context.item {
        Some(item) => {
            let branch = (item.branch.as_ref()).map_or(String::new(), |b| format!(" on {b}"));
            line(
                "item",
                format!("{} ({}{branch})", item.path.display(), item.kind.as_str()),
            );
        }
        None if context.issue_keys.is_empty() => line("item", "not in an atelier item".into()),
        None => {}
    }
    if let Some(workspace) = &context.workspace {
        let mut notes = Vec::new();
        if workspace.current {
            notes.push("current session".to_owned());
        }
        if let Some(session) = &workspace.session {
            notes.push(format!("tab in {session}"));
        }
        let notes = match notes.is_empty() {
            true => String::new(),
            false => format!(" ({})", notes.join(", ")),
        };
        line("workspace", format!("{}{notes}", workspace.name));
    }
    if let Some(group) = &context.group {
        line("group", group.clone());
    }
    if !context.issue_keys.is_empty() {
        line("issues", context.issue_keys.join(", "));
    }
    if let Some(cached) = &context.issue {
        let issue = &cached.issue;
        let url = (issue.url.as_ref()).map_or(String::new(), |url| format!("  {url}"));
        line(
            "issue",
            format!("{}  {}  [{}]{url}", issue.key, issue.title, issue.status),
        );
    }
    if !context.worktrees.is_empty() {
        let _ = writeln!(out, "worktrees");
        for tree in &context.worktrees {
            let mark = if tree.current { '*' } else { ' ' };
            let branch = tree.branch.as_deref().unwrap_or("?");
            let state = match (&tree.status, &tree.error) {
                (Some(status), _) => status.symbols.clone(),
                (None, Some(error)) => format!("({error})"),
                (None, None) => String::new(),
            };
            let _ = writeln!(
                out,
                "  {mark} {}:{branch}  {}  {}  {state}",
                tree.repo,
                tree.path.display(),
                tree.workspace
            );
        }
    }
    if !context.carnets.is_empty() {
        let _ = writeln!(out, "carnets");
        for carnet in &context.carnets {
            let mark = if context.carnet.as_ref() == Some(&carnet.path) {
                '*'
            } else {
                ' '
            };
            let open = if carnet.closed { "closed" } else { "open" };
            let _ = writeln!(
                out,
                "  {mark} {}  {open}  {}",
                carnet.path.display(),
                carnet.summary
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::carnet::tests::repo;
    use crate::issues::tests::issue;
    use crate::process::fake::Fake;
    use crate::state::Tab;

    /// `wt list` reporting `path` on `branch`, dirty, one ahead.
    fn listing(path: &Path, branch: &str) -> String {
        format!(
            r#"{{"items": [{{"branch": "{branch}",
                "head": {{"short_sha": "abc1234", "subject": "Fix it", "committed_at": "2026-10-01T00:00:00Z"}},
                "worktree": {{"path": "{}", "main": false, "detached": false,
                    "changes": {{"modified": true, "diff": {{"added": 3, "deleted": 1}}}}}},
                "upstream": {{"ahead": 1, "behind": 0}},
                "display": {{"symbols": "!↑"}}}}]}}"#,
            path.display()
        )
    }

    struct Setup {
        dir: tempfile::TempDir,
        state: State,
        config: Config,
        tree: PathBuf,
        other: PathBuf,
        newer: PathBuf,
        closed: PathBuf,
    }

    /// Repos `/a` (aliased `api`) and `/b`, a worktree of each in group `LOGIN`, the first
    /// linking `ABC-1`, and one ungrouped; carnets in `LOGIN` (one closed, one older), one
    /// linking `ABC-1` after `ORD-7`, one linking only `ORD-7`; and `ABC-1` cached.
    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("carnets");
        let config = Config::parse(&format!(
            "[carnets]\nroot = \"{}\"\n[tracker.github]\nrepos = [\"o/api\"]\n",
            root.display()
        ))
        .unwrap();
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("w").unwrap();
        state.add_repo("/a", Some("api"), "w").unwrap();
        state.add_repo("/b", None, "default").unwrap();
        let tree = dir.path().join("a.ABC-1");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        let tree = tree.canonicalize().unwrap();
        let other = dir.path().join("b.ABC-1");
        let plain = dir.path().join("a.plain");
        let add = |path: &Path, repo: &str, group: &str, keys: &[String], workspace: &str| {
            let repo = Some(Path::new(repo));
            (state.add_item(path, ItemKind::Worktree, repo, group, keys, workspace)).unwrap();
        };
        add(&tree, "/a", "LOGIN", &["ABC-1".into()], "w");
        add(&other, "/b", "LOGIN", &[], "default");
        add(&plain, "/a", "", &[], "w");
        state
            .set_tab(&Tab {
                path: tree.clone(),
                session: "w".into(),
                tab_id: 1,
                pane_id: "terminal_1".into(),
            })
            .unwrap();
        repo(
            &root,
            "2026-01-01-ABC-1-older",
            Some("+++\ngroup = \"login\"\n+++\n"),
        );
        let newer = repo(
            &root,
            "2026-02-01-notes",
            Some("+++\nissues = [\"ORD-7\", \"ABC-1\"]\nsummary = \"Notes\"\n+++\n"),
        );
        let closed = repo(
            &root,
            "2026-03-01-ABC-1-done",
            Some("+++\ngroup = \"LOGIN\"\nclosed = true\n+++\n"),
        );
        repo(
            &root,
            "2026-04-01-ORD-7-other",
            Some("+++\nissues = [\"ORD-7\"]\n+++\n"),
        );
        let cached = serde_json::to_string(&[issue("ABC-1", &[], false)]).unwrap();
        state.store_cache("gh", "issues o/api", &cached).unwrap();
        Setup {
            dir,
            state,
            config,
            tree,
            other,
            newer,
            closed,
        }
    }

    #[test]
    fn a_worktree_with_its_group_issue_worktrees_and_carnets() {
        let setup = setup();
        // worktrunk may report a path through a symlink, as macOS's `/var` is.
        let link = setup.dir.path().join("link");
        std::os::unix::fs::symlink(&setup.tree, &link).unwrap();
        let statusline = listing(&link, "ABC-1-fix").replace(
            r#""display""#,
            r#""checks": {"status": "failed", "source": "pr"},
                "pr": {"number": 31, "url": "https://github.com/o/api/pull/31", "review": "approved"},
                "display""#,
        );
        let fake = Fake::default()
            .always(
                &format!("wt -C {}", setup.tree.display()),
                Some(&statusline),
            )
            .always("git -C /a", Some("ABC-1-fix\0[gone]\n"))
            .always("wt -C /a", Some(&listing(&link, "ABC-1-fix")))
            .always("wt -C /b", None);
        let dir = setup.tree.join("src");
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            Some("w"),
            Target::Dir(&dir),
        )
        .unwrap();
        assert_eq!(
            fake.calls(),
            [
                format!(
                    "wt -C {} --config-set list.json-schema=2 list statusline --format json",
                    setup.tree.display()
                ),
                "git -C /a for-each-ref refs/heads --format=%(refname:short)%00%(upstream:track)"
                    .into(),
                "wt -C /b --config-set list.json-schema=2 list --format json".into(),
            ],
            "the current worktree's statusline, then once per other repo, never --full"
        );
        let item = context.item.as_ref().unwrap();
        assert_eq!(item.path, setup.tree);
        assert_eq!(
            (item.kind, item.branch.as_deref()),
            (ItemKind::Worktree, Some("ABC-1-fix"))
        );
        let workspace = context.workspace.as_ref().unwrap();
        assert_eq!(workspace.name, "w");
        assert!(workspace.current);
        assert_eq!(workspace.session.as_deref(), Some("w"));
        assert_eq!(context.group.as_deref(), Some("LOGIN"));
        assert_eq!(context.issue_keys, ["ABC-1"]);
        let cached = context.issue.as_ref().unwrap();
        assert_eq!(cached.issue.title, "Issue ABC-1");
        assert!(!cached.cached_at.is_empty());

        let [tree, other] = &context.worktrees[..] else {
            panic!("{:?}", context.worktrees);
        };
        assert_eq!((tree.repo.as_str(), tree.current), ("api", true));
        let status = tree.status.as_ref().unwrap();
        assert!(status.dirty && !status.main);
        assert_eq!((status.added, status.deleted), (3, 1));
        assert_eq!(status.head.subject, "Fix it");
        assert_eq!(
            (other.path.as_path(), other.repo.as_str()),
            (setup.other.as_path(), "b")
        );
        assert!(!other.current && other.status.is_none() && other.branch.is_none());
        assert!(other.error.as_ref().unwrap().contains("failed"));

        let carnets: Vec<_> = (context.carnets.iter())
            .map(|carnet| (carnet.name.as_str(), carnet.closed))
            .collect();
        assert_eq!(
            carnets,
            [
                ("ABC-1-done", true),
                ("notes", false),
                ("ABC-1-older", false)
            ],
            "the group's, and one linking the first key later; ORD-7's own does not"
        );
        assert_eq!(context.carnets[0].path, setup.closed);
        assert_eq!(
            context.carnet,
            Some(setup.newer.clone()),
            "the newest open one"
        );

        let json = serde_json::to_value(&context).unwrap();
        assert_eq!(
            json["issue"]["key"], "ABC-1",
            "the issue's fields at its top"
        );
        assert_eq!(json["worktrees"][0]["status"]["upstream"]["ahead"], 1);
        assert_eq!(
            json["worktrees"][0]["status"]["ci"],
            serde_json::json!({"state": "failed", "stale": false, "branch_workflow": false})
        );
        assert_eq!(
            json["worktrees"][0]["status"]["review"],
            serde_json::json!({
                "number": 31, "url": "https://github.com/o/api/pull/31", "decision": "approved"
            })
        );
        assert_eq!(json["worktrees"][0]["status"]["finished"], "upstream_gone");

        let text = render(&context);
        assert!(
            text.starts_with(&format!(
                "item       {} (worktree on ABC-1-fix)\nworkspace  w (current session, tab in w)\n\
             group      LOGIN\nissues     ABC-1\nissue      ABC-1  Issue ABC-1  [open]  ",
                setup.tree.display()
            )),
            "{text}"
        );
        assert!(text.contains("  * api:ABC-1-fix  "), "{text}");
        assert!(
            text.contains(&format!("  * {}  open  Notes", setup.newer.display())),
            "{text}"
        );
    }

    #[test]
    fn a_carnet_takes_its_group_from_its_folder() {
        let setup = setup();
        let closed = &setup.closed;
        (setup.state)
            .add_item(closed, ItemKind::Carnet, None, "stale", &[], "w")
            .unwrap();
        let fake = Fake::default();
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::Dir(closed),
        )
        .unwrap();
        assert_eq!(
            context.group.as_deref(),
            Some("LOGIN"),
            "the folder's, not the row's"
        );
        assert!(context.issue_keys.is_empty());
        let item = context.item.unwrap();
        assert_eq!(
            (item.kind, item.repo, item.branch),
            (ItemKind::Carnet, None, None)
        );
        assert!(!context.workspace.unwrap().current);
        assert_eq!(context.carnets[0].workspace, "w");
        assert_eq!(context.carnets[1].workspace, "default", "never placed");
    }

    #[test]
    fn an_issue_key_without_a_directory() {
        let setup = setup();
        let fake = Fake::default();
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::Key("ORD-7"),
        )
        .unwrap();
        assert!(context.item.is_none() && context.workspace.is_none());
        assert_eq!(
            (context.group, context.issue_keys),
            (None, vec!["ORD-7".into()])
        );
        assert!(context.issue.is_none(), "not cached");
        assert!(context.worktrees.is_empty());
        assert_eq!(context.carnets.len(), 2);
        assert_eq!(
            context.carnet.unwrap().file_name().unwrap(),
            "2026-04-01-ORD-7-other"
        );
        assert!(fake.calls().is_empty());

        let fake = Fake::default().always("wt", None);
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::Key("ABC-1"),
        )
        .unwrap();
        let paths: Vec<_> = context.worktrees.iter().map(|tree| &tree.path).collect();
        assert_eq!(
            paths,
            [&setup.tree],
            "the worktrees linking it, not its group"
        );
        let carnets: Vec<_> = (context.carnets.iter())
            .map(|carnet| carnet.name.as_str())
            .collect();
        assert_eq!(carnets, ["notes"]);
        assert_eq!(context.issue.unwrap().issue.key, "ABC-1");
    }

    #[test]
    fn a_directory_in_no_item_or_no_group_describes_nothing() {
        let setup = setup();
        let fake = Fake::default();
        let outside = setup.dir.path().join("carnets");
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::Dir(&outside),
        )
        .unwrap();
        let json = serde_json::to_value(&context).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "item": null, "workspace": null, "group": null, "issue_keys": [], "issue": null,
                "worktrees": [], "carnets": [], "carnet": null,
            })
        );
        assert_eq!(render(&context), "item       not in an atelier item\n");

        let plain = setup.dir.path().join("a.plain");
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::Dir(&plain),
        )
        .unwrap();
        assert!(context.item.is_some() && context.group.is_none());
        assert!(context.worktrees.is_empty() && context.carnets.is_empty());
        assert_eq!(fake.calls().len(), 1, "only its own repo, for its branch");
    }
}
