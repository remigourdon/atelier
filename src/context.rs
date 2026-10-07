//! `atelier context`: what atelier knows about a directory or an issue key, for scripts and
//! agents.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use color_eyre::eyre::Result;
use serde::Serialize;

use crate::carnet::{Carnet, Carnets};
use crate::config::Config;
use crate::finish::{self, Signal};
use crate::git;
use crate::issues::{self, Issue, TrackerConfig};
use crate::items::canonical;
use crate::links::{IssueKey, IssueKeys, Links, group_text};
use crate::process::Runner;
use crate::reviews::{self, Provider};
use crate::state::{Item, ItemKind, State, dir_name};
use crate::worktrunk::{self, CiState, Decision, Forge, Listing, Statusline, Worktree};

/// What to describe: the item holding a directory, or an issue key.
pub enum Target<'a> {
    Dir(&'a Path),
    IssueKey(&'a str),
}

/// Everything atelier knows about a directory's item: its group and issues, the work linked to
/// either, and the reviews linking its issues. Read from the database, the carnet folders, the
/// caches and `wt list`: nothing is written.
#[derive(Debug, Serialize)]
pub struct Context {
    /// The worktree or carnet holding the directory; `None` for an issue key or a directory
    /// in no item atelier recorded.
    pub item: Option<ItemInfo>,
    /// The item's workspace.
    pub workspace: Option<Workspace>,
    /// The item's group and the issue keys it links, in order; or the issue key described.
    #[serde(flatten)]
    pub links: Links,
    /// One per issue key, in order.
    pub issues: Vec<IssueInfo>,
    /// The worktrees in the group, in any workspace, and those sharing an issue key.
    pub worktrees: Vec<WorktreeInfo>,
    /// The carnets in the group and those sharing an issue key, closed ones included, newest
    /// first.
    pub carnets: Vec<CarnetInfo>,
    /// The open reviews linking an issue key, most recently updated first.
    pub reviews: Vec<ReviewInfo>,
    /// The newest open carnet in the group: where notes go.
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

/// An issue key, with its issue as the cache holds it.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum IssueInfo {
    Cached(Box<CachedIssue>),
    /// Not in the cache.
    Uncached {
        key: IssueKey,
    },
}

impl IssueInfo {
    pub fn key(&self) -> &IssueKey {
        match self {
            IssueInfo::Cached(cached) => &cached.issue.key,
            IssueInfo::Uncached { key } => key,
        }
    }
}

/// An issue as the cache holds it.
#[derive(Debug, Serialize)]
pub struct CachedIssue {
    #[serde(flatten)]
    pub issue: Issue,
    /// When the listing holding it was fetched, `YYYY-MM-DD HH:MM:SS` UTC.
    pub cached_at: String,
}

/// A worktree in the group or sharing an issue key.
#[derive(Debug, Serialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    /// The repo's alias, else its directory name.
    pub repo: String,
    /// The repo's main worktree.
    pub repo_path: PathBuf,
    pub branch: Option<String>,
    /// Its group and issue keys, which tell why it is listed.
    #[serde(flatten)]
    pub links: Links,
    pub workspace: String,
    /// The zellij session of its recorded tab.
    pub session: Option<String>,
    /// Whether it holds the directory described.
    pub holds: bool,
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
            ci: tree.ci.as_ref().and_then(|ci| {
                Some(CiStatus {
                    state: ci.state?,
                    stale: ci.stale,
                    branch_workflow: ci.branch_workflow,
                })
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

/// A carnet in the group or sharing an issue key.
#[derive(Debug, Serialize)]
pub struct CarnetInfo {
    pub path: PathBuf,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The folder name after its date.
    pub name: String,
    /// Its group and issue keys, which tell why it is listed.
    #[serde(flatten)]
    pub links: Links,
    pub closed: bool,
    pub summary: String,
    pub workspace: String,
    /// The zellij session of its recorded tab.
    pub session: Option<String>,
}

impl CarnetInfo {
    pub fn new(carnet: &Carnet, workspace: String, session: Option<String>) -> Self {
        Self {
            path: carnet.path.clone(),
            date: carnet.date.clone(),
            name: carnet.name.clone(),
            links: carnet.links.clone(),
            closed: carnet.closed,
            summary: carnet.summary.clone(),
            workspace,
            session,
        }
    }
}

/// An open review linking an issue key, as the cache holds it.
#[derive(Debug, Serialize)]
pub struct ReviewInfo {
    pub provider: Provider,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: String,
    /// `owner/repo`, or a GitLab project's full path.
    pub project: String,
    /// The project's web page.
    pub project_url: String,
    pub issue_keys: IssueKeys,
    /// The worktree listed here that has its branch checked out, in its project's repo.
    pub worktree: Option<PathBuf>,
    /// Whether it is the review of the worktree holding the directory.
    pub here: bool,
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

    /// The worktrees whose links `linked` picks.
    fn worktrees(&self, linked: impl Fn(&Links) -> bool) -> Vec<&Item> {
        (self.items.iter())
            .filter(|item| item.kind == ItemKind::Worktree && linked(&item.links))
            .collect()
    }
}

/// The item holding a directory and its links as `describe` reads them: a carnet's from its
/// folder.
pub struct Located {
    pub item: Item,
    pub links: Links,
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
    let carnets = match item.is_carnet() {
        true => Carnets::new(config)?.scan()?,
        false => Vec::new(),
    };
    let carnet = carnets.into_iter().find(|carnet| carnet.path == item.path);
    Ok(Some(Located {
        links: links(&item, carnet.as_ref()),
        item,
        carnet,
    }))
}

/// An item's links: a carnet's as its folder records them, else the recorded ones.
fn links(item: &Item, carnet: Option<&Carnet>) -> Links {
    match carnet {
        Some(carnet) => carnet.links.clone(),
        None => item.links.clone(),
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

/// Each repo's worktrees and forge from one `wt list`, with canonical paths as the database
/// records them, or why the listing failed.
struct Listings(HashMap<PathBuf, Result<Listing, String>>);

impl Listings {
    /// Lists each of the items' repos once, without `--full`.
    fn of<'a>(runner: &dyn Runner, items: impl IntoIterator<Item = &'a Item>) -> Self {
        let mut listings = HashMap::new();
        for repo in items.into_iter().filter_map(|item| item.repo.as_ref()) {
            if !listings.contains_key(repo) {
                let listing = (worktrunk::list(runner, repo, false))
                    .map(|mut listing| {
                        for tree in &mut listing.worktrees {
                            tree.path = canonical(&tree.path);
                        }
                        listing
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
        let listing = (self.0.get(repo))
            .ok_or("not listed")?
            .as_ref()
            .map_err(Clone::clone)?;
        (listing.worktrees.iter())
            .find(|tree| tree.path == item.path)
            .ok_or_else(|| "not listed by wt list".to_owned())
    }

    /// Where a listed repo is hosted.
    fn forge(&self, repo: &Path) -> Option<&Forge> {
        self.0.get(repo)?.as_ref().ok()?.forge.as_ref()
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
    let carnets = Carnets::new(config)?.scan()?;
    let records = Records::read(state)?;
    let (current, links) = match target {
        Target::IssueKey(key) => {
            let issue_keys = IssueKeys::resolve([key], &config.tracker);
            let links = Links {
                group: None,
                issue_keys,
            };
            (None, links)
        }
        Target::Dir(dir) => match records.containing(dir) {
            Some(item) => {
                let carnet = (carnets.iter()).find(|carnet| carnet.path == item.path);
                (
                    Some(item),
                    self::links(item, carnet.filter(|_| item.is_carnet())),
                )
            }
            None => (None, Links::default()),
        },
    };
    let group = links.group.as_ref();
    let in_group = |other: &Links| group.is_some() && other.group.as_ref() == group;
    let linked = |other: &Links| in_group(other) || other.issue_keys.shares(&links.issue_keys);
    let trees = records.worktrees(linked);
    let tree = current
        .filter(|item| item.kind == ItemKind::Worktree)
        .map(|item| Current {
            path: &item.path,
            repo: item.repo.as_deref(),
            statusline: current_tree(runner, item),
        });
    let listings = Listings::of(
        runner,
        (trees.iter().copied())
            .filter(|item| current.is_none_or(|current| current.path != item.path)),
    );
    let issues = (links.issue_keys.iter())
        .map(|key| {
            let cached = issues::cached(state, &config.tracker, key)?;
            Ok(match cached {
                Some((issue, cached_at)) => {
                    IssueInfo::Cached(Box::new(CachedIssue { issue, cached_at }))
                }
                None => IssueInfo::Uncached { key: key.clone() },
            })
        })
        .collect::<Result<_>>()?;
    let listed: Vec<&Carnet> = (carnets.iter())
        .filter(|carnet| linked(&carnet.links))
        .collect();
    let worktrees = worktrees(&records, &listings, &trees, tree.as_ref());
    let forge = |repo: &Path| match &tree {
        Some(current) if current.repo == Some(repo) => {
            current.statusline.as_ref().ok()?.forge.as_ref()
        }
        _ => listings.forge(repo),
    };
    let reviews = (reviews::cached(state)?.into_iter())
        .filter(|review| review.issue_keys.shares(&links.issue_keys))
        .map(|review| {
            let worktree = (worktrees.iter()).find(|info| {
                info.branch.as_deref() == Some(review.branch.as_str())
                    && forge(&info.repo_path).is_some_and(|forge| {
                        worktrunk::same_project(&forge.url, &review.project_url)
                    })
            });
            ReviewInfo {
                provider: review.provider,
                number: review.number,
                url: review.url,
                title: review.title,
                author: review.author,
                project: review.project,
                project_url: review.project_url,
                issue_keys: review.issue_keys,
                here: worktree.is_some_and(|info| info.holds),
                worktree: worktree.map(|info| info.path.clone()),
            }
        })
        .collect();
    Ok(Context {
        item: current.map(|item| ItemInfo {
            path: item.path.clone(),
            kind: item.kind,
            repo: item.repo.clone(),
            branch: (tree.as_ref())
                .and_then(|current| current.statusline.as_ref().ok()?.tree.branch.clone()),
        }),
        workspace: current.map(|item| Workspace {
            current: here == Some(item.workspace.as_str()),
            name: item.workspace.clone(),
            session: records.session(&item.path),
        }),
        carnets: carnet_infos(&records, &listed),
        carnet: (listed.iter())
            .filter(|carnet| !carnet.closed && in_group(&carnet.links))
            .max_by(|a, b| a.path.cmp(&b.path))
            .map(|carnet| carnet.path.clone()),
        worktrees,
        reviews,
        links,
        issues,
    })
}

/// The worktree holding the directory, as `wt list statusline` reported it.
struct Current<'a> {
    path: &'a Path,
    repo: Option<&'a Path>,
    statusline: Result<Statusline, String>,
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
                Some(current) => (current.statusline.as_ref())
                    .map(|statusline| &statusline.tree)
                    .map_err(Clone::clone),
                None => listings.find(item),
            };
            WorktreeInfo {
                path: item.path.clone(),
                repo: (records.repos.get(&repo_path).cloned())
                    .unwrap_or_else(|| dir_name(&repo_path)),
                repo_path,
                branch: tree.as_ref().ok().and_then(|tree| tree.branch.clone()),
                links: item.links.clone(),
                workspace: item.workspace.clone(),
                session: records.session(&item.path),
                holds: holding.is_some(),
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

/// A listed item's group and issue keys, keys shown short: `LOGIN · ABC-1, ORD-7`.
fn tags(links: &Links, tracker: &TrackerConfig) -> String {
    let keys = links.issue_keys.display(tracker, ", ");
    let tags: Vec<&str> = [group_text(links.group.as_ref()), keys.as_str()]
        .into_iter()
        .filter(|tag| !tag.is_empty())
        .collect();
    tags.join(" · ")
}

/// Columns joined by two spaces, the empty ones left out.
fn columns(columns: &[&str]) -> String {
    let filled: Vec<&str> = (columns.iter().copied())
        .filter(|column| !column.is_empty())
        .collect();
    filled.join("  ")
}

/// The context as lines for a person to read, issue keys shown short.
pub fn render(context: &Context, tracker: &TrackerConfig) -> String {
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
        None if context.links.issue_keys.is_empty() => {
            line("item", "not in an atelier item".into())
        }
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
    if let Some(group) = &context.links.group {
        line("group", group.to_string());
    }
    for (index, info) in context.issues.iter().enumerate() {
        let label = if index == 0 { "issues" } else { "" };
        let key = info.key().display(tracker);
        let text = match info {
            IssueInfo::Cached(cached) => columns(&[
                &key,
                &cached.issue.title,
                &format!("[{}]", cached.issue.status),
                cached.issue.url.as_deref().unwrap_or_default(),
            ]),
            IssueInfo::Uncached { .. } => key,
        };
        line(label, text);
    }
    let mark = |marked: bool| if marked { '*' } else { ' ' };
    if !context.worktrees.is_empty() {
        let _ = writeln!(out, "worktrees");
        for tree in &context.worktrees {
            let branch = tree.branch.as_deref().unwrap_or("?");
            let state = match (&tree.status, &tree.error) {
                (Some(status), _) => status.symbols.clone(),
                (None, Some(error)) => format!("({error})"),
                (None, None) => String::new(),
            };
            let text = columns(&[
                &format!("{}:{branch}", tree.repo),
                &tree.path.to_string_lossy(),
                &tree.workspace,
                &state,
                &tags(&tree.links, tracker),
            ]);
            let _ = writeln!(out, "  {} {text}", mark(tree.holds));
        }
    }
    if !context.carnets.is_empty() {
        let _ = writeln!(out, "carnets");
        for carnet in &context.carnets {
            let open = if carnet.closed { "closed" } else { "open" };
            let text = columns(&[
                &carnet.path.to_string_lossy(),
                open,
                &tags(&carnet.links, tracker),
                &carnet.summary,
            ]);
            let here = context.carnet.as_ref() == Some(&carnet.path);
            let _ = writeln!(out, "  {} {text}", mark(here));
        }
    }
    if !context.reviews.is_empty() {
        let _ = writeln!(out, "reviews");
        for review in &context.reviews {
            let worktree = (review.worktree.as_ref())
                .map_or(String::new(), |path| path.to_string_lossy().into_owned());
            let text = columns(&[
                &format!(
                    "{}{}",
                    review.project,
                    review.provider.reference(review.number)
                ),
                &review.title,
                &review.author,
                &review.url,
                &worktree,
            ]);
            let _ = writeln!(out, "  {} {text}", mark(review.here));
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
    use crate::links::tests::{group, key, keys, links};
    use crate::process::fake::Fake;
    use crate::reviews::Role;
    use crate::reviews::tests::review;
    use crate::state::Tab;

    /// `wt list` reporting `path` on `branch`, dirty, one ahead, in a repo hosted at `forge`.
    fn listing(path: &Path, branch: &str, forge: &str) -> String {
        format!(
            r#"{{"repo": {{"forge": {{"url": "{forge}", "provider": "github"}}}},
                "items": [{{"branch": "{branch}",
                "head": {{"short_sha": "abc1234", "subject": "Fix it", "committed_at": "2026-10-01T00:00:00Z"}},
                "worktree": {{"path": "{}", "main": false, "detached": false,
                    "changes": {{"modified": true, "diff": {{"added": 3, "deleted": 1}}}}}},
                "upstream": {{"ahead": 1, "behind": 0}},
                "display": {{"symbols": "!↑"}}}}]}}"#,
            path.display()
        )
    }

    const API: &str = "https://github.com/o/api";
    const B: &str = "https://github.com/o/b";

    struct Setup {
        dir: tempfile::TempDir,
        state: State,
        config: Config,
        tree: PathBuf,
        other: PathBuf,
        shared: PathBuf,
        older: PathBuf,
        notes: PathBuf,
        closed: PathBuf,
    }

    /// Repos `/a` (aliased `api`, at `o/api`) and `/b` (at `o/b`). Worktrees: `tree` of `/a`
    /// in `LOGIN` linking `ABC-1`, `other` of `/b` in `LOGIN`, `shared` of `/b` in no group
    /// linking `ORD-7` then `ABC-1`, and two linked to neither. Carnets: `older` and `closed`
    /// in `LOGIN`, `notes` linking `ORD-7` then `ABC-1`, and one linking only `ORD-7`. `ABC-1`
    /// is cached; reviews #31 on `tree`'s branch linking `ABC-1`, #32 on `shared`'s linking
    /// `ORD-7`, and #33 linking `DEF-2`.
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
        let shared = dir.path().join("b.ORD-7");
        std::fs::create_dir_all(&shared).unwrap();
        let shared = shared.canonicalize().unwrap();
        let other = tree.with_file_name("b.ABC-1");
        let add = |path: &Path, repo: &str, links: Links, workspace: &str| {
            let repo = Some(Path::new(repo));
            (state.add_item(path, ItemKind::Worktree, repo, &links, workspace)).unwrap();
        };
        add(&tree, "/a", self::links("LOGIN", &["ABC-1"]), "w");
        add(&other, "/b", self::links("LOGIN", &[]), "default");
        add(
            &shared,
            "/b",
            self::links("", &["ORD-7", "ABC-1"]),
            "default",
        );
        let plain = tree.with_file_name("a.plain");
        add(&plain, "/a", self::links("", &[]), "w");
        let elsewhere = tree.with_file_name("a.DEF-2");
        add(&elsewhere, "/a", self::links("OTHER", &["DEF-2"]), "w");
        state
            .set_tab(&Tab {
                path: tree.clone(),
                session: "w".into(),
                tab_id: 1,
                pane_id: "terminal_1".into(),
            })
            .unwrap();
        let older = repo(
            &root,
            "2026-01-01-ABC-1-older",
            Some("+++\ngroup = \"login\"\n+++\n"),
        );
        let notes = repo(
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
        let mut reviews = Vec::new();
        for (number, project, branch, linked) in [
            (31, API, "ABC-1-fix", "ABC-1"),
            (32, B, "ORD-7-x", "ORD-7"),
            (33, API, "DEF-2-x", "DEF-2"),
        ] {
            let mut review = review(Provider::GitHub, Role::ToReview, number, project);
            review.branch = branch.into();
            review.issue_keys = keys(&[linked]);
            reviews.push(review);
        }
        let reviews = serde_json::to_string(&reviews).unwrap();
        state
            .store_cache("gh", "github.com to-review", &reviews)
            .unwrap();
        Setup {
            dir,
            state,
            config,
            tree,
            other,
            shared,
            older,
            notes,
            closed,
        }
    }

    /// The paths of `infos`.
    fn paths<'a, T>(infos: &'a [T], path: impl Fn(&'a T) -> &'a PathBuf) -> Vec<&'a PathBuf> {
        infos.iter().map(path).collect()
    }

    fn numbers(context: &Context) -> Vec<u64> {
        context.reviews.iter().map(|review| review.number).collect()
    }

    #[test]
    fn a_worktree_with_its_group_issues_linked_work_and_reviews() {
        let setup = setup();
        // worktrunk may report a path through a symlink, as macOS's `/var` is.
        let link = setup.dir.path().join("link");
        std::os::unix::fs::symlink(&setup.tree, &link).unwrap();
        let statusline = listing(&link, "ABC-1-fix", API).replace(
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
            .always("wt -C /b", Some(&listing(&setup.shared, "ORD-7-x", B)));
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
        assert_eq!(context.links, links("LOGIN", &["ABC-1"]));
        let [IssueInfo::Cached(cached)] = &context.issues[..] else {
            panic!("{:?}", context.issues);
        };
        assert_eq!(cached.issue.title, "Issue ABC-1");
        assert!(!cached.cached_at.is_empty());

        assert_eq!(
            paths(&context.worktrees, |tree| &tree.path),
            [&setup.tree, &setup.other, &setup.shared],
            "the group's in every workspace, and one sharing a key in no group"
        );
        let [tree, other, shared] = &context.worktrees[..] else {
            unreachable!()
        };
        assert_eq!((tree.repo.as_str(), tree.holds), ("api", true));
        let status = tree.status.as_ref().unwrap();
        assert!(status.dirty && !status.main);
        assert_eq!((status.added, status.deleted), (3, 1));
        assert_eq!(status.head.subject, "Fix it");
        assert_eq!(
            (other.repo.as_str(), other.workspace.as_str()),
            ("b", "default")
        );
        assert!(!other.holds && other.status.is_none() && other.branch.is_none());
        assert_eq!(other.error.as_deref(), Some("not listed by wt list"));
        assert_eq!(shared.links, links("", &["ORD-7", "ABC-1"]));
        assert_eq!(shared.branch.as_deref(), Some("ORD-7-x"));

        assert_eq!(
            paths(&context.carnets, |carnet| &carnet.path),
            [&setup.closed, &setup.notes, &setup.older],
            "the group's, and one sharing a key; ORD-7's own does not"
        );
        assert!(context.carnets[0].closed);
        assert_eq!(
            context.carnet,
            Some(setup.older.clone()),
            "the newest open one in the group, never one only sharing a key"
        );

        assert_eq!(numbers(&context), [31], "only those linking ABC-1");
        let review = &context.reviews[0];
        assert_eq!(review.worktree.as_ref(), Some(&setup.tree));
        assert!(review.here);

        let json = serde_json::to_value(&context).unwrap();
        assert_eq!(json["group"], "LOGIN");
        assert_eq!(json["issue_keys"], serde_json::json!(["ABC-1"]));
        assert_eq!(json["issues"][0]["key"], "ABC-1", "the issue's fields");
        assert_eq!(json["issues"][0]["status"], "open");
        assert_eq!(json["worktrees"][0]["holds"], true);
        assert_eq!(json["worktrees"][0]["group"], "LOGIN");
        assert_eq!(json["worktrees"][2]["group"], serde_json::Value::Null);
        assert_eq!(
            json["worktrees"][2]["issue_keys"],
            serde_json::json!(["ORD-7", "ABC-1"])
        );
        assert_eq!(
            json["carnets"][1]["issue_keys"],
            serde_json::json!(["ORD-7", "ABC-1"])
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
        assert_eq!(
            json["reviews"][0],
            serde_json::json!({
                "provider": "github", "number": 31, "url": "https://github.com/o/api/pull/31",
                "title": "Change 31", "author": "alice", "project": "org/api", "project_url": API,
                "issue_keys": ["ABC-1"], "worktree": setup.tree, "here": true,
            })
        );

        let text = render(&context, &setup.config.tracker);
        let tree = setup.tree.display();
        assert!(
            text.starts_with(&format!(
                "item       {tree} (worktree on ABC-1-fix)\n\
                 workspace  w (current session, tab in w)\n\
                 group      LOGIN\n\
                 issues     ABC-1  Issue ABC-1  [open]  https://forge/api/issues/ABC-1\n\
                 worktrees\n  * api:ABC-1-fix  {tree}  w  !↑  LOGIN · ABC-1\n"
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "    b:ORD-7-x  {}  default  !↑  ORD-7, ABC-1\n",
                setup.shared.display()
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "carnets\n    {}  closed  LOGIN\n    {}  open  ORD-7, ABC-1  Notes\n  * {}  open  LOGIN\n",
                setup.closed.display(),
                setup.notes.display(),
                setup.older.display()
            )),
            "{text}"
        );
        assert!(
            text.ends_with(&format!(
                "reviews\n  * org/api#31  Change 31  alice  https://github.com/o/api/pull/31  {tree}\n"
            )),
            "{text}"
        );
    }

    #[test]
    fn an_item_in_no_group_still_gets_its_issues_reviews_and_the_work_sharing_its_keys() {
        let setup = setup();
        let fake = Fake::default()
            .always(
                &format!("wt -C {}", setup.shared.display()),
                Some(&listing(&setup.shared, "ORD-7-x", B)),
            )
            .always("wt -C /a", Some(&listing(&setup.tree, "ABC-1-fix", API)));
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::Dir(&setup.shared),
        )
        .unwrap();
        assert_eq!(context.links, links("", &["ORD-7", "ABC-1"]));
        let issues: Vec<_> = (context.issues.iter())
            .map(|info| (info.key().as_str(), matches!(info, IssueInfo::Cached(_))))
            .collect();
        assert_eq!(issues, [("ORD-7", false), ("ABC-1", true)], "in order");
        assert_eq!(
            serde_json::to_value(&context.issues[0]).unwrap(),
            serde_json::json!({"key": "ORD-7"}),
            "just its key when it is not cached"
        );
        assert_eq!(
            paths(&context.worktrees, |tree| &tree.path),
            [&setup.tree, &setup.shared],
            "not the LOGIN worktree linking neither key"
        );
        let names: Vec<_> = (context.carnets.iter())
            .map(|carnet| carnet.name.as_str())
            .collect();
        assert_eq!(names, ["ORD-7-other", "notes"]);
        assert_eq!(context.carnet, None, "no group, no carnet to write in");
        assert_eq!(numbers(&context), [32, 31], "most recently updated first");
        let worktrees: Vec<_> = (context.reviews.iter())
            .map(|review| (review.worktree.as_ref(), review.here))
            .collect();
        assert_eq!(
            worktrees,
            [(Some(&setup.shared), true), (Some(&setup.tree), false)]
        );
        let text = render(&context, &setup.config.tracker);
        assert!(
            text.contains("\nissues     ORD-7\n           ABC-1  Issue ABC-1  [open]  "),
            "{text}"
        );
    }

    #[test]
    fn a_carnet_takes_its_links_from_its_folder() {
        let setup = setup();
        let closed = &setup.closed;
        (setup.state)
            .add_item(closed, ItemKind::Carnet, None, &links("stale", &[]), "w")
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
            context.links,
            links("LOGIN", &[]),
            "the folder's, not the row's"
        );
        let item = context.item.unwrap();
        assert_eq!(
            (item.kind, item.repo, item.branch),
            (ItemKind::Carnet, None, None)
        );
        assert!(!context.workspace.unwrap().current);
        assert_eq!(
            paths(&context.carnets, |carnet| &carnet.path),
            [&setup.closed, &setup.older]
        );
        assert_eq!(context.carnets[0].workspace, "w");
        assert_eq!(context.carnets[1].workspace, "default", "never placed");
        assert_eq!(context.carnet, Some(setup.older.clone()));
        assert!(context.issues.is_empty() && context.reviews.is_empty());
    }

    #[test]
    fn an_issue_key_with_its_linked_work_in_any_group_and_reviews() {
        let setup = setup();
        let fake = Fake::default();
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::IssueKey("ORD-7"),
        )
        .unwrap();
        assert!(context.item.is_none() && context.workspace.is_none());
        assert_eq!(context.links, links("", &["ORD-7"]));
        assert_eq!(context.issues[0].key(), &key("ORD-7"));
        assert_eq!(
            paths(&context.worktrees, |tree| &tree.path),
            [&setup.shared]
        );
        assert_eq!(context.carnets.len(), 2);
        assert_eq!(context.carnet, None);
        assert_eq!(numbers(&context), [32]);
        assert!(!context.reviews[0].here);

        let fake = Fake::default().always("wt", None);
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::IssueKey(" ABC-1 "),
        )
        .unwrap();
        assert_eq!(
            paths(&context.worktrees, |tree| &tree.path),
            [&setup.tree, &setup.shared],
            "every worktree linking it, in any group"
        );
        assert_eq!(
            paths(&context.carnets, |carnet| &carnet.path),
            [&setup.notes]
        );
        assert_eq!(numbers(&context), [31]);
        assert_eq!(context.reviews[0].worktree, None, "wt list failed");
        let [IssueInfo::Cached(cached)] = &context.issues[..] else {
            panic!("{:?}", context.issues);
        };
        assert_eq!(cached.issue.key, key("ABC-1"));
        assert_eq!(context.links.group, group(""));
    }

    #[test]
    fn a_directory_in_no_item_or_linked_to_nothing_describes_nothing() {
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
                "item": null, "workspace": null, "group": null, "issue_keys": [], "issues": [],
                "worktrees": [], "carnets": [], "reviews": [], "carnet": null,
            })
        );
        let tracker = &setup.config.tracker;
        assert_eq!(
            render(&context, tracker),
            "item       not in an atelier item\n"
        );
        let keyed = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::IssueKey("api#3"),
        )
        .unwrap();
        assert_eq!(keyed.links.issue_keys, keys(&["o/api#3"]), "resolved");
        assert_eq!(render(&keyed, tracker), "issues     api#3\n", "shown short");

        let plain = setup.tree.with_file_name("a.plain");
        let context = describe(
            &setup.state,
            &setup.config,
            &fake,
            None,
            Target::Dir(&plain),
        )
        .unwrap();
        assert!(context.item.is_some() && context.links == Links::default());
        assert!(context.worktrees.is_empty() && context.carnets.is_empty());
        assert!(context.issues.is_empty() && context.reviews.is_empty());
        assert_eq!(fake.calls().len(), 1, "only its own repo, for its branch");
    }
}
