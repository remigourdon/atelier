//! Items: the worktrees and carnets atelier records, and every operation on them, shared by
//! the TUI, the CLI and the hooks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail, eyre};

use crate::carnet::{self, Carnet, Carnets};
use crate::config::Config;
#[cfg(test)]
use crate::finish::Signal;
use crate::finish::Step;
use crate::git;
use crate::hooks::Hints;
use crate::linked::{self, LinkedWork};
use crate::links::{Group, IssueKey, IssueKeys, KeyFinder, Links};
use crate::process::{Logged, Runner};
use crate::reviews::Review;
use crate::state::{self, ItemKind, Repo, State, Tab};
use crate::worktrunk::{self, Forge, Worktree};
use crate::zellij::{self, Layouts, Naming, Zellij};

/// Everything loaded from the database, worktrunk and zellij.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// The zellij session the TUI runs in.
    pub here: Option<String>,
    /// The current session first.
    pub workspaces: Vec<String>,
    pub repos: Vec<Repo>,
    /// Every item once: the worktrees, then every carnet, closed ones included, newest first;
    /// no carnet while carnets are disabled.
    pub work: Vec<Work>,
    /// Each repo's forge web page, by repo path.
    pub forges: HashMap<PathBuf, Forge>,
}

impl Snapshot {
    /// Its items' linked work, with `reviews` placed on its worktrees.
    pub fn linked<'a>(&'a self, reviews: &'a [Review]) -> LinkedWork<'a, Work> {
        LinkedWork::new(&self.work, &self.repos, &self.forges, reviews)
    }

    /// Its carnets, closed ones included, newest first.
    pub fn carnets(&self) -> Vec<&Work> {
        self.work.iter().filter(|work| work.is_carnet()).collect()
    }

    /// Its carnets, newest first, for tests that change them.
    #[cfg(test)]
    pub fn carnets_mut(&mut self) -> Vec<&mut Work> {
        (self.work.iter_mut())
            .filter(|work| work.is_carnet())
            .collect()
    }
}

/// A worktree or a carnet, with what atelier records about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Work {
    pub path: PathBuf,
    pub workspace: String,
    pub links: Links,
    pub tab: bool,
    pub kind: WorkKind,
}

/// What only a worktree or only a carnet has.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkKind {
    Worktree {
        repo: PathBuf,
        repo_name: String,
        tree: Box<Worktree>,
    },
    /// What its README's front matter records.
    Carnet {
        closed: bool,
        summary: String,
        /// Its README's stamp, so a stale loaded one can be told apart.
        readme: Option<carnet::Stamp>,
    },
}

impl Work {
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn is_carnet(&self) -> bool {
        matches!(self.kind, WorkKind::Carnet { .. })
    }

    /// A closed carnet; a worktree is never closed.
    pub fn closed(&self) -> bool {
        matches!(self.kind, WorkKind::Carnet { closed: true, .. })
    }

    /// A carnet's one-line summary; empty for a worktree.
    pub fn summary(&self) -> &str {
        match &self.kind {
            WorkKind::Carnet { summary, .. } => summary,
            WorkKind::Worktree { .. } => "",
        }
    }

    pub fn group(&self) -> Option<&Group> {
        self.links.group.as_ref()
    }

    /// A worktree's repo.
    pub fn repo(&self) -> Option<&PathBuf> {
        match &self.kind {
            WorkKind::Worktree { repo, .. } => Some(repo),
            WorkKind::Carnet { .. } => None,
        }
    }

    /// A worktree's listing.
    pub fn tree(&self) -> Option<&Worktree> {
        match &self.kind {
            WorkKind::Worktree { tree, .. } => Some(tree),
            WorkKind::Carnet { .. } => None,
        }
    }

    /// A worktree's listing, for tests that change it.
    #[cfg(test)]
    pub fn tree_mut(&mut self) -> &mut Worktree {
        match &mut self.kind {
            WorkKind::Worktree { tree, .. } => tree,
            WorkKind::Carnet { .. } => panic!("a carnet has no worktree"),
        }
    }

    /// A worktree's branch, else its directory name: detached, or a carnet's folder.
    pub fn branch(&self) -> String {
        (self.tree().and_then(|tree| tree.branch.clone()))
            .unwrap_or_else(|| state::dir_name(&self.path))
    }

    /// A worktree that is not its repo's main one, so it can be removed. A carnet is closed
    /// instead.
    pub fn removable(&self) -> bool {
        self.tree().is_some_and(|tree| !tree.main)
    }

    /// `repo:branch`, or a carnet's folder name.
    pub fn title(&self) -> String {
        match &self.kind {
            WorkKind::Worktree { repo_name, .. } => format!("{repo_name}:{}", self.branch()),
            WorkKind::Carnet { .. } => state::dir_name(&self.path),
        }
    }
}

impl linked::Item for Work {
    fn links(&self) -> &Links {
        &self.links
    }

    fn repo(&self) -> Option<&Path> {
        Work::repo(self).map(PathBuf::as_path)
    }

    fn branch(&self) -> Option<&str> {
        self.tree()?.branch.as_deref()
    }
}

/// A worktree to remove through worktrunk, and whether it has changes that will be discarded.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub branch: Option<String>,
    pub force: bool,
}

impl Removal {
    /// A removable worktree's removal.
    pub fn of(work: &Work) -> Option<Self> {
        match &work.kind {
            WorkKind::Worktree { repo, tree, .. } if work.removable() => Some(Self {
                path: work.path.clone(),
                repo: repo.clone(),
                branch: tree.branch.clone(),
                force: tree.dirty,
            }),
            _ => None,
        }
    }
}

/// The canonical path when it exists, so paths match the ones hooks record.
pub fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

/// A recorded item with its links as they are now: a worktree's from its row, a carnet's from
/// its folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub path: PathBuf,
    pub workspace: String,
    pub links: Links,
    pub kind: RecordedKind,
}

/// What only a worktree or only a carnet has.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordedKind {
    Worktree {
        repo: PathBuf,
    },
    /// Its folder, as read.
    Carnet(Carnet),
}

impl Recorded {
    /// `item` with its links: a carnet's from `carnet`, its folder; `None` for a carnet whose
    /// folder is not there to read.
    pub fn new(item: state::Item, carnet: Option<Carnet>) -> Option<Self> {
        let (links, kind) = match item.record {
            state::Record::Worktree { repo, links } => (links, RecordedKind::Worktree { repo }),
            state::Record::Carnet => {
                let carnet = carnet?;
                (carnet.links.clone(), RecordedKind::Carnet(carnet))
            }
        };
        Some(Self {
            path: item.path,
            workspace: item.workspace,
            links,
            kind,
        })
    }

    pub fn is_carnet(&self) -> bool {
        matches!(self.kind, RecordedKind::Carnet(_))
    }

    /// Its `items.kind`.
    pub fn item_kind(&self) -> ItemKind {
        match self.kind {
            RecordedKind::Worktree { .. } => ItemKind::Worktree,
            RecordedKind::Carnet(_) => ItemKind::Carnet,
        }
    }

    /// A worktree's repo.
    pub fn repo(&self) -> Option<&Path> {
        match &self.kind {
            RecordedKind::Worktree { repo } => Some(repo),
            RecordedKind::Carnet(_) => None,
        }
    }
}

/// Its branch is not recorded.
impl linked::Item for Recorded {
    fn links(&self) -> &Links {
        &self.links
    }

    fn repo(&self) -> Option<&Path> {
        Recorded::repo(self)
    }

    fn branch(&self) -> Option<&str> {
        None
    }
}

/// Every recorded item with its current links: worktrees from their rows, carnets from their
/// folders under the carnet root, closed ones included. A carnet's folder is its record
/// ([ADR 0001]): one without a row yet is in the default workspace, where the next scan records
/// it; a row whose folder is not found is left out. Never writes.
///
/// [ADR 0001]: ../docs/adr/0001-carnet-folder-is-the-record.md
pub fn read(state: &State, carnets: &Carnets) -> Result<Vec<Recorded>> {
    recorded(state, carnets.scan()?)
}

/// The recorded items, each carnet's links from `found`, its folder as scanned. Worktrees by
/// path, then carnets as found, newest first.
fn recorded(state: &State, found: Vec<Carnet>) -> Result<Vec<Recorded>> {
    let mut items: HashMap<PathBuf, state::Item> = (state.items()?.into_iter())
        .map(|item| (item.path.clone(), item))
        .collect();
    let mut carnets = Vec::new();
    for carnet in found {
        let item = match items.remove(&carnet.path) {
            Some(item) if !item.is_carnet() => continue,
            Some(item) => item,
            None => state::Item {
                path: carnet.path.clone(),
                workspace: state.default_workspace().to_owned(),
                record: state::Record::Carnet,
            },
        };
        carnets.extend(Recorded::new(item, Some(carnet)));
    }
    let mut recorded: Vec<Recorded> = (items.into_values())
        .filter(|item| !item.is_carnet())
        .filter_map(|item| Recorded::new(item, None))
        .collect();
    recorded.sort_by(|a, b| a.path.cmp(&b.path));
    recorded.extend(carnets);
    Ok(recorded)
}

/// Runs `action` on every value, then joins the errors.
fn each<T>(values: &[T], mut action: impl FnMut(&T) -> Result<()>) -> Result<()> {
    let errors: Vec<String> = (values.iter())
        .filter_map(|value| action(value).err().map(|err| err.to_string()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(eyre!(errors.join("; ")))
    }
}

/// The item operations, over one database connection and one view of zellij.
pub struct Items<'a> {
    state: &'a State,
    runner: &'a dyn Runner,
    config: &'a Config,
    zellij: Zellij<'a>,
    keys: KeyFinder<'a>,
    carnets: Carnets<'a>,
}

impl<'a> Items<'a> {
    /// Items as seen from the current zellij session, if any.
    pub fn new(
        state: &'a State,
        runner: &'a dyn Runner,
        config: &'a Config,
        layouts: Layouts,
    ) -> Result<Self> {
        Ok(Self {
            state,
            runner,
            config,
            zellij: Zellij::new(runner, config, layouts),
            keys: KeyFinder::new(config)?,
            carnets: Carnets::new(config)?,
        })
    }

    /// The same, as seen from `here`.
    #[cfg(test)]
    pub fn in_session(self, here: Option<&str>) -> Self {
        Self {
            zellij: self.zellij.in_session(here),
            ..self
        }
    }

    /// Every issue key in `names`, in order, without duplicates.
    fn keys(&self, names: &[&str]) -> IssueKeys {
        self.keys.find(names)
    }

    /// The keys a worktree on `branch` is recorded with: `extra`, then those in the branch.
    fn seeded(&self, extra: &IssueKeys, branch: &str) -> IssueKeys {
        let mut keys = extra.clone();
        keys.extend(self.keys(&[branch]).iter().cloned());
        keys
    }

    /// The workspace a new item goes to: `explicit` when it is a workspace, else the current
    /// session when it is one, else `fallback`.
    pub fn workspace(&self, explicit: Option<&str>, fallback: &str) -> String {
        let known = |name: &&str| self.state.has_workspace(name).unwrap_or(false);
        (explicit.filter(known))
            .or(self.zellij.here().filter(known))
            .unwrap_or(fallback)
            .to_owned()
    }

    /// Opens each item's tab in its workspace, or focuses the one already open.
    pub fn open(&self, paths: &[PathBuf]) -> Result<()> {
        each(paths, |path| self.open_tab(path).map(drop))
    }

    pub fn close(&self, paths: &[PathBuf]) -> Result<()> {
        each(paths, |path| self.zellij.close_tab(self.state, path))
    }

    /// Fast-forwards each worktree; carnets are skipped.
    pub fn pull(&self, paths: &[PathBuf]) -> Result<()> {
        each(paths, |path| {
            if self.state.item(path)?.is_some_and(|item| item.is_carnet()) {
                return Ok(());
            }
            git::pull_ff_only(self.runner, path)
        })
    }

    /// Creates a worktree on `branch` (or switches to the existing one) and opens its tab. A new
    /// one is in `group` and links the issue keys in its branch. Returns its path.
    pub fn create(
        &self,
        repo: &Path,
        branch: &str,
        workspace: &str,
        group: Option<&Group>,
    ) -> Result<PathBuf> {
        let links = Links {
            group: group.cloned(),
            issue_keys: IssueKeys::default(),
        };
        let path = self.switch_branch(repo, branch, workspace, &links)?;
        if self.state.tab(&path)?.is_none() {
            self.open_tab(&path)?;
        }
        Ok(path)
    }

    /// Checks out a review's branch through worktrunk (`pr:N` or `mr:N`) and focuses its tab,
    /// whether the worktree is new or was there already. A new one is in `group` and links the
    /// review's keys, then those in its branch.
    pub fn checkout(
        &self,
        repo: &Path,
        workspace: &str,
        review: &Review,
        group: Option<&Group>,
    ) -> Result<()> {
        let links = Links {
            group: group.cloned(),
            issue_keys: review.issue_keys.clone(),
        };
        let target = review.provider.shortcut(review.number);
        let path = self.switch(repo, &[&target], &review.branch, workspace, &links)?;
        self.open_tab(&path).map(drop)
    }

    /// Creates a worktree on `branch` for an issue, in `group`, linking the issue's key, then the
    /// keys in the branch. A worktree that already existed gains the key and keeps its group.
    pub fn start(
        &self,
        repo: &Path,
        branch: &str,
        workspace: &str,
        issue_key: &IssueKey,
        group: Option<&Group>,
    ) -> Result<()> {
        let links = Links {
            group: group.cloned(),
            issue_keys: [issue_key.clone()].into_iter().collect(),
        };
        let path = self.switch_branch(repo, branch, workspace, &links)?;
        let item = self.state.require_item(&path)?;
        if let Some(links) = item.links().filter(|links| !links.links(issue_key)) {
            let mut links = links.clone();
            links.issue_keys.push(issue_key.clone());
            self.state.set_links(&path, &links)?;
        }
        if self.state.tab(&path)?.is_none() {
            self.open_tab(&path)?;
        }
        Ok(())
    }

    /// Switches to `branch`, creating it when it does not exist yet.
    fn switch_branch(
        &self,
        repo: &Path,
        branch: &str,
        workspace: &str,
        links: &Links,
    ) -> Result<PathBuf> {
        let target: &[&str] = if git::branch_exists(self.runner, repo, branch)? {
            &[branch]
        } else {
            &["--create", branch]
        };
        self.switch(repo, target, branch, workspace, links)
    }

    /// Runs `wt switch` with the workspace, the group and the issue keys beyond the branch's
    /// for atelier's hooks, then records the worktree on `branch` as the hook does, in case the
    /// hooks are not installed. A worktree already recorded keeps its links. Returns its path.
    fn switch(
        &self,
        repo: &Path,
        target: &[&str],
        branch: &str,
        workspace: &str,
        links: &Links,
    ) -> Result<PathBuf> {
        worktrunk::switch(self.runner, repo, target, workspace, links)?;
        let listing = worktrunk::list(self.runner, repo, false)?;
        let tree = (listing.worktrees.iter())
            .find(|tree| tree.branch.as_deref() == Some(branch))
            .ok_or_else(|| {
                eyre!(
                    "wt switch {} left no worktree on {branch}",
                    target.join(" ")
                )
            })?;
        let path = canonical(&tree.path);
        let links = Links {
            group: links.group.clone(),
            issue_keys: self.seeded(&links.issue_keys, branch),
        };
        (self.state).add_worktree(&path, repo, &links, workspace)?;
        Ok(path)
    }

    /// Creates a carnet with `links` and `summary` and records it in `workspace`, all or
    /// nothing: a carnet that cannot be recorded is deleted. Returns it.
    pub fn create_carnet(
        &self,
        name: &str,
        workspace: &str,
        links: &Links,
        summary: &str,
    ) -> Result<Carnet> {
        self.state.require_workspace(workspace)?;
        let date = self.state.today()?;
        let carnet = (self.carnets).create(self.runner, &date, name, links, summary)?;
        if let Err(err) = self.state.add_carnet(&carnet.path, workspace) {
            // Unrecorded, it would block retrying under the same name.
            let _ = std::fs::remove_dir_all(&carnet.path);
            return Err(err);
        }
        Ok(carnet)
    }

    /// Closes or reopens each carnet. Closing also closes its tab.
    pub fn set_carnets_closed(&self, paths: &[PathBuf], closed: bool) -> Result<()> {
        each(paths, |path| {
            self.carnets.set_closed(self.runner, path, closed)?;
            if closed {
                self.zellij.close_tab(self.state, path)?;
            }
            Ok(())
        })
    }

    /// Removes each worktree through worktrunk and forgets it.
    pub fn remove(&self, removals: &[Removal]) -> Result<()> {
        each(removals, |removal| {
            let path = removal.path.to_string_lossy();
            let target = removal.branch.as_deref().unwrap_or(&path);
            worktrunk::remove(self.runner, &removal.repo, target, removal.force)?;
            self.forget(&removal.path)
        })
    }

    /// Fetches each repo, pruning gone branches, and returns the repos whose fetch failed; the
    /// runner records why.
    pub fn fetch(&self, repos: &[PathBuf]) -> Vec<PathBuf> {
        (repos.iter())
            .filter(|repo| git::fetch_prune(self.runner, repo).is_err())
            .cloned()
            .collect()
    }

    /// Runs a finish plan's checked steps: the removals, then the carnet closes, then the
    /// pulls, each whatever the others did.
    pub fn finish(&self, steps: &[Step]) -> Result<()> {
        let mut steps = steps.to_vec();
        steps.sort_by_key(Step::order);
        each(&steps, |step| match step {
            Step::Remove { removal, .. } => self.remove(std::slice::from_ref(removal)),
            Step::CloseCarnet(path) => self.set_carnets_closed(std::slice::from_ref(path), true),
            Step::Pull(path) => git::pull_ff_only(self.runner, path),
        })
    }

    /// Moves each item to `workspace`, reopening its tab there if one was open.
    pub fn move_to(&self, paths: &[PathBuf], workspace: &str) -> Result<()> {
        each(paths, |path| {
            let had_tab = self.state.tab(path)?.is_some();
            self.zellij.close_tab(self.state, path)?;
            self.state.set_workspace(path, workspace)?;
            if had_tab {
                self.open_tab(path)?;
            }
            Ok(())
        })
    }

    /// Edits an item's group and issue keys with `edit`: a carnet's in its front matter, in one
    /// commit, a worktree's in its row.
    fn relink(&self, path: &Path, edit: impl FnOnce(&mut Links)) -> Result<()> {
        match self.state.require_item(path)?.record {
            state::Record::Carnet => self.carnets.relink(self.runner, path, edit).map(drop),
            state::Record::Worktree { links, .. } => {
                let mut edited = links.clone();
                edit(&mut edited);
                match edited == links {
                    true => Ok(()),
                    false => self.state.set_links(path, &edited),
                }
            }
        }
    }

    /// Edits a carnet's group, issue keys and summary with `edit`, in one commit.
    pub fn amend_carnet(
        &self,
        path: &Path,
        edit: impl FnOnce(&mut Links, &mut String),
    ) -> Result<()> {
        if !self.state.require_item(path)?.is_carnet() {
            bail!("{} is not a carnet", path.display());
        }
        self.carnets.amend(self.runner, path, edit).map(drop)
    }

    /// Replaces the issue keys an item links.
    pub fn set_issue_keys(&self, path: &Path, keys: &IssueKeys) -> Result<()> {
        self.relink(path, |links| links.issue_keys = keys.clone())
    }

    /// Puts each item in `group`, or in none. A carnet's group is written to its front matter.
    pub fn regroup(&self, paths: &[PathBuf], group: Option<&Group>) -> Result<()> {
        each(paths, |path| {
            self.relink(path, |links| links.group = group.cloned())
        })
    }

    /// Sets a repo's alias, empty to clear it.
    pub fn set_alias(&self, repo: &Path, alias: &str) -> Result<()> {
        self.update_repo(repo, Some(alias), None)
    }

    /// Sets the workspace a repo's new worktrees go to.
    pub fn set_repo_workspace(&self, repo: &Path, workspace: &str) -> Result<()> {
        self.update_repo(repo, None, Some(workspace))
    }

    /// Changes a repo's alias (`Some("")` clears it) and/or the workspace its new worktrees go
    /// to, checking both before writing either.
    pub fn update_repo(
        &self,
        repo: &Path,
        alias: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<()> {
        (self.state).update_repo(&repo.to_string_lossy(), alias, workspace)
    }

    /// Forgets a repo and closes its tabs. Its worktrees stay on disk.
    pub fn forget_repo(&self, repo: &Path) -> Result<()> {
        self.zellij.close_repo_tabs(self.state, repo)?;
        self.state.remove_repo(repo)
    }

    pub fn add_workspace(&self, name: &str) -> Result<()> {
        self.state.add_workspace(name)
    }

    /// Removes a workspace that owns no worktree, moving its carnets to the default one.
    pub fn remove_workspace(&self, name: &str) -> Result<()> {
        self.state.remove_workspace(name)
    }

    /// Syncs the recorded items with worktrunk and the carnet root, names the open tabs, and
    /// gathers what the panels show, with the carnets when they are enabled, and the repos that
    /// could not be listed.
    pub fn snapshot(&self, full: bool) -> Result<(Snapshot, Vec<Logged>)> {
        // A reconcile failure (zellij not running) should not hide the worktrees.
        let _ = self.zellij.reconcile(self.state);
        let synced = self.sync(full)?;
        let problems = (synced.failures.iter())
            .map(|(repo, err)| Logged {
                command: format!("wt list in {}", repo.name()),
                error: Some(format!("{err:#}")),
            })
            .collect();
        let found = self.carnets.scan()?;
        self.record_carnets(&found)?;
        let recorded = recorded(self.state, found)?;
        let branches: HashMap<&Path, Option<String>> = (synced.worktrees.iter())
            .map(|(_, _, tree)| (tree.path.as_path(), tree.branch.clone()))
            .collect();
        // A worktree whose repo could not be listed asks git for its branch. As with reconcile,
        // zellij not running should not hide the worktrees.
        let branch = |path: &Path| match branches.get(path) {
            Some(branch) => branch.clone(),
            None => git::branch(self.runner, path),
        };
        let _ = self.rename_tabs(&recorded, branch);
        let tabs: HashSet<PathBuf> = (self.state.tabs()?.into_iter())
            .map(|tab| tab.path)
            .collect();
        let mut by_path: HashMap<PathBuf, Recorded> = (recorded.iter().cloned())
            .map(|item| (item.path.clone(), item))
            .collect();
        let mut work = Vec::new();
        for (repo, _, tree) in synced.worktrees {
            let Some(item) = by_path.remove(&tree.path) else {
                continue;
            };
            work.push(Work {
                tab: tabs.contains(&tree.path),
                path: tree.path.clone(),
                workspace: item.workspace,
                links: item.links,
                kind: WorkKind::Worktree {
                    repo_name: repo.name(),
                    repo: repo.path,
                    tree: Box::new(tree),
                },
            });
        }
        let carnets: Vec<Work> = (recorded.into_iter())
            .filter_map(|item| match item.kind {
                RecordedKind::Carnet(carnet) => Some(Work {
                    tab: tabs.contains(&item.path),
                    path: item.path,
                    workspace: item.workspace,
                    links: item.links,
                    kind: WorkKind::Carnet {
                        closed: carnet.closed,
                        summary: carnet.summary,
                        readme: carnet.readme,
                    },
                }),
                RecordedKind::Worktree { .. } => None,
            })
            .collect();
        work.extend(carnets);
        let here = self.zellij.here().map(str::to_owned);
        let mut workspaces = self.state.workspaces()?;
        if let Some(here) = &here
            && let Some(index) = workspaces.iter().position(|name| name == here)
        {
            let here = workspaces.remove(index);
            workspaces.insert(0, here);
        }
        let snapshot = Snapshot {
            here,
            workspaces,
            repos: self.state.repos()?,
            work,
            forges: synced.forges,
        };
        Ok((snapshot, problems))
    }

    /// Lists every repo's worktrees, records the unknown ones in their repo's default
    /// workspace, in no group, linking the issue keys in their branch, and forgets the worktrees
    /// a listing no longer names.
    fn sync(&self, full: bool) -> Result<Synced> {
        let state = self.state;
        let mut synced = Synced::default();
        for repo in state.repos()? {
            // Only items recorded before the listing ran can be missing from it: one a hook
            // records meanwhile is kept.
            let recorded = state.repo_items(&repo.path)?;
            let listing = match worktrunk::list(self.runner, &repo.path, full) {
                Ok(listing) => listing,
                Err(err) => {
                    synced.failures.push((repo, err));
                    continue;
                }
            };
            let mut listed = HashSet::new();
            if let Some(forge) = listing.forge {
                synced.forges.insert(repo.path.clone(), forge);
            }
            // Only a sign of finished work, so a failure leaves every branch not gone.
            let gone = git::gone_branches(self.runner, &repo.path).unwrap_or_default();
            for mut tree in listing.worktrees {
                tree.path = canonical(&tree.path);
                tree.gone = (tree.branch.as_ref()).is_some_and(|branch| gone.contains(branch));
                let name = (tree.branch.clone()).unwrap_or_else(|| state::dir_name(&tree.path));
                let links = Links {
                    group: None,
                    issue_keys: self.keys(&[&name]),
                };
                state.add_worktree(&tree.path, &repo.path, &links, &repo.default_workspace)?;
                listed.insert(tree.path.clone());
                let item = state.require_item(&tree.path)?;
                synced.worktrees.push((repo.clone(), item, tree));
            }
            for item in recorded {
                if !listed.contains(&item.path) {
                    state.remove_item(&item.path)?;
                }
            }
        }
        Ok(synced)
    }

    /// Records each carnet `found` that is not yet, in the default workspace, and deletes the
    /// rows of carnets no longer found, when any is.
    fn record_carnets(&self, found: &[Carnet]) -> Result<()> {
        let state = self.state;
        let mut recorded: HashMap<PathBuf, state::Item> = (state.items()?.into_iter())
            .map(|item| (item.path.clone(), item))
            .collect();
        for carnet in found {
            if recorded.remove(&carnet.path).is_none() {
                state.add_carnet(&carnet.path, state.default_workspace())?;
            }
        }
        // A root that shows no carnet is more likely unmounted than emptied: its rows stay
        // until a scan finds one again.
        if !found.is_empty() {
            for item in recorded.into_values().filter(|item| item.is_carnet()) {
                state.remove_item(&item.path)?;
            }
        }
        Ok(())
    }

    /// Opens an item's tab, named from its current facts, or focuses the one already open.
    fn open_tab(&self, path: &Path) -> Result<Tab> {
        self.zellij.open_tab(self.state, path, || {
            let items = read(self.state, &self.carnets)?;
            let namings = self.namings(&items, Some(path))?;
            Ok(match namings.iter().find(|naming| naming.path == path) {
                Some(naming) => naming.tab_name(&namings, || self.branch(path)),
                // A carnet whose folder is gone: its folder's name.
                None => zellij::tab_name(None, &state::dir_name(path), "", true, false),
            })
        })
    }

    /// Renames every open tab whose live name is not the one its item's current facts give: its
    /// group, its repo's name, its branch and its open siblings. Tabs renamed by hand are
    /// renamed back.
    pub fn name_tabs(&self) -> Result<()> {
        self.zellij.reconcile(self.state)?;
        let items = read(self.state, &self.carnets)?;
        self.rename_tabs(&items, |path| git::branch(self.runner, path))
    }

    /// Renames the open tabs of `items` as [`Self::name_tabs`] does, `branch` giving a
    /// worktree's branch when it has one.
    fn rename_tabs(
        &self,
        items: &[Recorded],
        branch: impl Fn(&Path) -> Option<String>,
    ) -> Result<()> {
        let namings = self.namings(items, None)?;
        let tabs = self.state.tabs()?;
        let wanted: Vec<(&Tab, String)> = (tabs.iter())
            .filter_map(|tab| {
                let naming = namings.iter().find(|naming| naming.path == tab.path)?;
                let branch = || branch(&tab.path).unwrap_or_else(|| state::dir_name(&tab.path));
                Some((tab, naming.tab_name(&namings, branch)))
            })
            .collect();
        self.zellij.rename_tabs(&wanted)
    }

    /// What names the tab of each of `items` that is open, or is `opening`.
    fn namings(&self, items: &[Recorded], opening: Option<&Path>) -> Result<Vec<Naming>> {
        let mut open: HashSet<PathBuf> = (self.state.tabs()?.into_iter())
            .map(|tab| tab.path)
            .collect();
        open.extend(opening.map(Path::to_owned));
        let repos: HashMap<PathBuf, String> = (self.state.repos()?.into_iter())
            .map(|repo| (repo.path.clone(), repo.name()))
            .collect();
        Ok((items.iter())
            .filter(|item| open.contains(&item.path))
            .map(|item| Naming {
                path: item.path.clone(),
                workspace: item.workspace.clone(),
                group: item.links.group.clone(),
                repo: item.repo().map(|repo| {
                    let name = repos.get(repo).cloned();
                    (
                        repo.to_owned(),
                        name.unwrap_or_else(|| state::dir_name(repo)),
                    )
                }),
            })
            .collect())
    }

    /// A worktree's branch, else its directory name.
    fn branch(&self, path: &Path) -> String {
        git::branch(self.runner, path).unwrap_or_else(|| state::dir_name(path))
    }

    /// Records a worktree a hook reports, registering its repo when it is new, and opens its
    /// tab. Its workspace is the hinted one when it is one; its group the hinted one, else none;
    /// its issue keys the hinted ones, then those in its branch. A worktree of a carnet is not
    /// tracked: returns `None`.
    pub fn record(
        &self,
        path: &Path,
        repo: &Path,
        branch: &str,
        hints: &Hints,
    ) -> Result<Option<Tab>> {
        let workspace = hints.workspace.as_deref();
        let state = self.state;
        // A carnet is never a repo: worktrees made of it are not tracked.
        if state.item(repo)?.is_some_and(|item| item.is_carnet()) {
            return Ok(None);
        }
        let default_workspace = match state.repo_by_path(repo)? {
            Some(registered) => registered.default_workspace,
            None => {
                let placed = self.workspace(workspace, state.default_workspace());
                state.add_repo(repo, None, &placed)?;
                eprintln!("atelier: registered {} in {placed}", state::dir_name(repo));
                placed
            }
        };
        let hinted = (hints.issue_keys.iter().flatten()).map(String::as_str);
        let links = Links {
            group: hints.group.clone(),
            issue_keys: self.seeded(&IssueKeys::resolve(hinted, &self.config.tracker), branch),
        };
        let workspace = self.workspace(workspace, &default_workspace);
        state.add_worktree(path, repo, &links, &workspace)?;
        self.open_tab(path).map(Some)
    }

    /// Closes an item's tab and forgets it.
    pub fn forget(&self, path: &Path) -> Result<()> {
        self.zellij.close_tab(self.state, path)?;
        self.state.remove_item(path)
    }
}

/// Each repo's listed worktrees with their recorded items, and what failed.
#[derive(Default)]
struct Synced {
    worktrees: Vec<(Repo, state::Item, Worktree)>,
    /// Each repo's forge web page, by repo path.
    forges: HashMap<PathBuf, Forge>,
    /// Repos whose listing failed; their items are kept as they were.
    failures: Vec<(Repo, color_eyre::Report)>,
}

#[cfg(test)]
pub mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::links::group_text;
    use crate::links::tests::{group, key, keys, links};
    use crate::process::fake::Fake;
    use crate::reviews::{Provider, Role};
    use crate::zellij::layouts;

    /// A worktree of `/src/<repo>`, its main one on `main`, with `links`.
    pub fn tree_work(repo: &str, branch: &str, links: Links, workspace: &str) -> Work {
        let main = branch == "main";
        let path = if main {
            PathBuf::from(format!("/src/{repo}"))
        } else {
            PathBuf::from(format!("/src/{repo}.{branch}"))
        };
        Work {
            path: path.clone(),
            workspace: workspace.into(),
            links,
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

    /// An open carnet `/data/<name>` with `links`.
    pub fn carnet_work(name: &str, links: Links, workspace: &str) -> Work {
        Work {
            path: PathBuf::from(format!("/data/{name}")),
            workspace: workspace.into(),
            links,
            tab: false,
            kind: WorkKind::Carnet {
                closed: false,
                summary: String::new(),
                readme: None,
            },
        }
    }

    const LISTING: &str = r#"{"repo":{"forge":{"url":"https://forge/r"}},"items":[
        {"branch":"main","worktree":{"path":"/r","main":true}},
        {"branch":"ABC-1-x","worktree":{"path":"/r.ABC-1-x"}}]}"#;

    /// Workspaces `default` and `side`, and the repo `/r` placed in `default`.
    fn state() -> State {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("side").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        state
    }

    /// Items as seen from the `side` session.
    fn items<'a>(state: &'a State, fake: &'a Fake) -> Items<'a> {
        configured(state, fake, "")
    }

    fn configured<'a>(state: &'a State, fake: &'a Fake, config: &str) -> Items<'a> {
        let config = Box::leak(Box::new(Config::parse(config).unwrap()));
        (Items::new(state, fake, config, layouts()).unwrap()).in_session(Some("side"))
    }

    fn worktree(state: &State, path: impl AsRef<Path>, group: &str, workspace: &str) {
        let repo = Path::new("/r");
        (state.add_worktree(path, repo, &links(group, &[]), workspace)).unwrap();
    }

    fn tab(state: &State, path: impl AsRef<Path>, session: &str, tab_id: u64) {
        state
            .set_tab(&Tab {
                path: path.as_ref().to_owned(),
                session: session.into(),
                tab_id,
                pane_id: "7".into(),
            })
            .unwrap();
    }

    fn review(number: u64, branch: &str) -> Review {
        Review {
            provider: Provider::GitHub,
            role: Role::ToReview,
            number,
            title: String::new(),
            url: String::new(),
            project: "o/r".into(),
            project_url: String::new(),
            author: String::new(),
            branch: branch.into(),
            base: "main".into(),
            draft: false,
            updated_at: String::new(),
            issue_keys: IssueKeys::default(),
        }
    }

    #[test]
    fn sync_records_new_worktrees_and_forgets_vanished_ones() {
        let state = state();
        worktree(&state, "/r.gone", "", "default");
        worktree(&state, "/r.ABC-1-x", "", "side");
        let listing = r#"{"repo":{"forge":{"url":"https://forge/r"}},"items":[
            {"branch":"main","worktree":{"path":"/r","main":true}},
            {"branch":"ABC-1-x","worktree":{"path":"/r.ABC-1-x"}},
            {"branch":"DEF-2-y","worktree":{"path":"/r.DEF-2-y"}}]}"#;
        let fake = Fake::default().always("wt -C /r", Some(listing));
        let items = items(&state, &fake);
        let synced = items.sync(false).unwrap();
        assert!(synced.failures.is_empty());
        assert_eq!(synced.forges[Path::new("/r")].url, "https://forge/r");
        let placed: Vec<_> = (synced.worktrees.iter())
            .map(|(_, item, _)| {
                let keys = item.links().unwrap().issue_keys.join(",");
                (
                    item.workspace.as_str(),
                    group_text(item.links().unwrap().group.as_ref()),
                    keys,
                )
            })
            .collect();
        assert_eq!(
            placed,
            [
                ("default", "", String::new()),
                ("side", "", String::new()),
                ("default", "", "DEF-2".into())
            ],
            "a recorded item keeps what it has; a new one has no group and its branch's keys"
        );
        assert!(state.item("/r.gone").unwrap().is_none());
        items.sync(true).unwrap();
        assert!(fake.calls().iter().any(|call| call.ends_with("--full")));
    }

    #[test]
    fn sync_forgets_unlisted_worktrees_whose_folder_remains() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        worktree(&state, dir.path(), "", "default");
        let fake = Fake::default().always("wt -C /r", Some(LISTING));
        items(&state, &fake).sync(false).unwrap();
        assert!(state.item(dir.path()).unwrap().is_none());
    }

    /// Records a worktree, as a hook does, while `wt list` runs.
    struct HookDuringListing<'a> {
        state: &'a State,
        fake: Fake,
    }

    impl Runner for HookDuringListing<'_> {
        fn output(&self, program: &str, args: &[&str]) -> Result<String> {
            if program == "wt" {
                worktree(self.state, "/r.new", "ABC-9", "side");
            }
            self.fake.output(program, args)
        }

        fn interactive(&self, program: &str, args: &[&str]) -> Result<()> {
            self.fake.interactive(program, args)
        }

        fn spawn(&self, program: &str, args: &[&str]) -> Result<()> {
            self.fake.spawn(program, args)
        }
    }

    #[test]
    fn sync_keeps_a_worktree_recorded_while_the_listing_ran() {
        let state = state();
        let runner = HookDuringListing {
            state: &state,
            fake: Fake::default().always("wt -C /r", Some(LISTING)),
        };
        let config = Config::parse("").unwrap();
        let items = Items::new(&state, &runner, &config, layouts()).unwrap();
        items.sync(false).unwrap();
        let item = state.require_item("/r.new").unwrap();
        assert_eq!(
            (
                group_text(item.links().unwrap().group.as_ref()),
                item.workspace.as_str()
            ),
            ("ABC-9", "side")
        );
        items.sync(false).unwrap();
        assert!(
            state.item("/r.new").unwrap().is_none(),
            "the next listing, which ran after it was recorded, forgets it"
        );
    }

    #[test]
    fn a_failed_listing_keeps_the_repos_items() {
        let state = state();
        worktree(&state, "/r.gone", "", "default");
        let fake = Fake::default().always("wt", None);
        let synced = items(&state, &fake).sync(false).unwrap();
        assert!(synced.worktrees.is_empty());
        assert_eq!(synced.failures[0].0.name(), "r");
        assert!(state.item("/r.gone").unwrap().is_some());
    }

    #[test]
    fn snapshot_puts_the_current_session_first_and_logs_failed_listings() {
        let state = state();
        state.add_repo("/broken", None, "default").unwrap();
        let fake = Fake::default()
            .always("wt -C /r ", Some(LISTING))
            .always("wt -C /broken", None);
        let (snapshot, problems) = items(&state, &fake).snapshot(false).unwrap();
        assert_eq!(snapshot.workspaces, ["side", "default"]);
        let titles: Vec<_> = snapshot.work.iter().map(Work::title).collect();
        assert_eq!(titles, ["r:main", "r:ABC-1-x"]);
        assert_eq!(snapshot.forges[Path::new("/r")].url, "https://forge/r");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].command, "wt list in broken");
    }

    /// A recorded item's links as the reader gives them.
    fn current(items: &Items, path: &Path) -> Links {
        let read = read(items.state, &items.carnets).unwrap();
        read.into_iter()
            .find(|item| item.path == path)
            .unwrap()
            .links
    }

    /// The commands committing a carnet's README.
    fn carnet_commit(path: &Path, message: &str) -> [String; 2] {
        let path = path.display();
        [
            format!("git -C {path} add README.md"),
            format!("git -C {path} commit -m {message} -- README.md"),
        ]
    }

    /// Carnets enabled, under `root`.
    fn with_root<'a>(state: &'a State, fake: &'a Fake, root: &Path) -> Items<'a> {
        configured(
            state,
            fake,
            &format!("[carnets]\nroot = {:?}", root.display().to_string()),
        )
    }

    #[test]
    fn snapshot_scans_the_carnet_root_when_enabled() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let closed = "+++\nissues = [\"web#3\"]\nclosed = true\n+++\n";
        let placed = carnet::tests::repo(
            &root,
            "2026-10-01-ABC-1-notes",
            Some("+++\ngroup = \"login\"\nissues = [\"ABC-1\"]\n+++\n"),
        );
        let new = carnet::tests::repo(
            &root,
            "2026-10-02-G-2-bare",
            Some("+++\nissues = [\"G-3\"]\n+++\n"),
        );
        let done = carnet::tests::repo(&root, "2026-09-01-done", Some(closed));
        state.add_carnet(&placed, "side").unwrap();
        state.add_carnet("/data/2026-01-01-gone", "side").unwrap();
        tab(&state, "/data/2026-01-01-gone", "side", 4);
        let fake = Fake::default().always("wt", Some(r#"{"items":[]}"#));
        let (snapshot, _) = items(&state, &fake).snapshot(false).unwrap();
        assert!(snapshot.work.is_empty());
        assert!(
            state.item("/data/2026-01-01-gone").unwrap().is_some(),
            "disabled, carnet rows are left alone"
        );
        let (snapshot, _) = with_root(&state, &fake, &root).snapshot(false).unwrap();
        let listed = |work: Vec<&Work>| -> Vec<(String, String, String, String)> {
            (work.iter())
                .map(|work| {
                    let keys = work.links.issue_keys.join(",");
                    (
                        work.title(),
                        group_text(work.group()).to_owned(),
                        keys,
                        work.workspace.clone(),
                    )
                })
                .collect()
        };
        let row = |title: &str, group: &str, keys: &str, workspace: &str| {
            let owned = |text: &str| text.to_owned();
            (owned(title), owned(group), owned(keys), owned(workspace))
        };
        assert_eq!(
            listed(snapshot.carnets()),
            [
                row("2026-10-02-G-2-bare", "", "G-3", "default"),
                row("2026-10-01-ABC-1-notes", "LOGIN", "ABC-1", "side"),
                row("2026-09-01-done", "", "web#3", "default"),
            ],
            "newest first; a known carnet keeps its workspace, a new one goes to the default"
        );
        assert_eq!(
            snapshot.work.len(),
            3,
            "every item once: no worktree here, and every carnet"
        );
        for carnet in [&placed, &new, &done] {
            let record = state.require_item(carnet).unwrap().record;
            assert_eq!(
                record,
                state::Record::Carnet,
                "a carnet's row holds no links"
            );
        }
        assert_eq!(state.item("/data/2026-01-01-gone").unwrap(), None);
        assert_eq!(
            state.tabs().unwrap(),
            [],
            "a vanished carnet's tab row goes with it"
        );
    }

    #[test]
    fn the_reader_gives_each_item_its_current_links_and_never_writes() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        worktree(&state, "/r.a", "LOGIN", "side");
        let closed = "+++\ngroup = \"g\"\nclosed = true\n+++\n";
        let placed = carnet::tests::repo(&root, "2026-10-01-placed", Some(closed));
        state.add_carnet(&placed, "side").unwrap();
        let new = carnet::tests::repo(
            &root,
            "2026-10-02-new",
            Some("+++\nissues = [\"N-1\"]\n+++\n"),
        );
        state.add_carnet("/gone/2026-01-01-x", "side").unwrap();
        let fake = Fake::default();
        let items = with_root(&state, &fake, &root);
        let read: Vec<_> = (read(&state, &items.carnets).unwrap().into_iter())
            .map(|item| (item.is_carnet(), item.path, item.workspace, item.links))
            .collect();
        assert_eq!(
            read,
            [
                (false, "/r.a".into(), "side".into(), links("LOGIN", &[])),
                (true, new.clone(), "default".into(), links("", &["N-1"])),
                (true, placed, "side".into(), links("G", &[])),
            ],
            "worktrees, then carnets newest first; a carnet with no row yet in the default \
             workspace, a closed one included, a row whose folder is gone left out"
        );
        assert!(state.item(&new).unwrap().is_none(), "nothing recorded");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_root_showing_no_carnet_keeps_their_rows() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let away = "/data/2026-01-01-away";
        (state.add_carnet(away, "side")).unwrap();
        let fake = Fake::default().always("wt", Some(r#"{"items":[]}"#));
        for root in [dir.path().join("unmounted"), dir.path().to_owned()] {
            let (snapshot, _) = with_root(&state, &fake, &root).snapshot(false).unwrap();
            assert!(snapshot.work.is_empty());
            let workspace = state.require_item(away).unwrap().workspace;
            assert_eq!(workspace, "side", "{}", root.display());
        }
    }

    #[test]
    fn regroup_sets_a_carnets_group_in_its_front_matter() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ngroup = \"A\"\nissues = [\"A-1\", \"B-2\"]\n+++\n";
        let path = carnet::tests::repo(dir.path(), "2026-10-01-notes", Some(readme));
        (state.add_carnet(&path, "default")).unwrap();
        let fake = Fake::default();
        let items = with_root(&state, &fake, dir.path());
        items
            .regroup(
                std::slice::from_ref(&path),
                group(" login rewrite ").as_ref(),
            )
            .unwrap();
        let readme = std::fs::read_to_string(path.join("README.md")).unwrap();
        assert_eq!(
            readme,
            "+++\ngroup = \"LOGIN REWRITE\"\nissues = [\"A-1\", \"B-2\"]\nsummary = \"\"\nclosed = false\n+++\n",
            "its keys stay"
        );
        assert_eq!(
            current(&items, &path),
            links("LOGIN REWRITE", &["A-1", "B-2"])
        );
        assert_eq!(
            state.require_item(&path).unwrap().record,
            state::Record::Carnet
        );
        let calls = fake.calls();
        assert_eq!(
            calls,
            carnet_commit(&path, "Set group LOGIN REWRITE"),
            "the folder alone is written"
        );
        items
            .regroup(std::slice::from_ref(&path), group("").as_ref())
            .unwrap();
        assert_eq!(current(&items, &path).group, None);
        assert!((fake.calls().iter()).any(|call| call.ends_with("commit -m Ungroup -- README.md")));
    }

    #[test]
    fn set_issue_keys_replaces_an_items_keys_wherever_it_keeps_them() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ngroup = \"A\"\nissues = [\"A-1\"]\n+++\n";
        let notes = carnet::tests::repo(dir.path(), "2026-10-01-notes", Some(readme));
        state.add_carnet(&notes, "default").unwrap();
        worktree(&state, "/r.a", "B", "default");
        let fake = Fake::default();
        let items = with_root(&state, &fake, dir.path());
        items
            .set_issue_keys(&notes, &keys(&["B-2", "A-1"]))
            .unwrap();
        items
            .set_issue_keys(Path::new("/r.a"), &keys(&["C-3"]))
            .unwrap();
        assert_eq!(current(&items, &notes), links("A", &["B-2", "A-1"]));
        assert!(
            std::fs::read_to_string(notes.join("README.md"))
                .unwrap()
                .contains("issues = [\"B-2\", \"A-1\"]")
        );
        assert_eq!(
            state.require_item("/r.a").unwrap().links().unwrap().clone(),
            links("B", &["C-3"])
        );
        let calls = fake.calls();
        assert!(
            (calls.iter()).any(|call| call.ends_with("commit -m Link B-2 -- README.md")),
            "{calls:?}"
        );
        assert!(
            items
                .set_issue_keys(Path::new("/r.unknown"), &keys(&[]))
                .is_err()
        );
    }

    #[test]
    fn closing_a_carnet_records_it_and_closes_its_tab() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let path = carnet::tests::repo(dir.path(), "2026-10-01-notes", None);
        state.add_carnet(&path, "default").unwrap();
        tab(&state, &path, "default", 4);
        let fake = Fake::default()
            .always(
                "zellij --session default action list-tabs",
                Some(r#"[{"tab_id":4,"position":1}]"#),
            )
            .always("zellij --session default action list-panes", Some("[]"));
        let items = with_root(&state, &fake, dir.path());
        items
            .set_carnets_closed(std::slice::from_ref(&path), true)
            .unwrap();
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.ends_with("commit -m Close -- README.md"))
        );
        assert!(calls.contains(&"zellij --session default action close-tab-by-id 4".into()));
        assert_eq!(state.tab(&path).unwrap(), None);
        let (snapshot, _) = items.snapshot(false).unwrap();
        assert!(snapshot.work[0].closed() && !snapshot.work[0].tab);
        tab(&state, &path, "default", 4);
        let (snapshot, _) = items.snapshot(false).unwrap();
        assert!(snapshot.work[0].closed() && snapshot.work[0].tab);
        items
            .set_carnets_closed(std::slice::from_ref(&path), false)
            .unwrap();
        assert!(
            fake.calls()
                .last()
                .unwrap()
                .ends_with("commit -m Reopen -- README.md")
        );
    }

    #[test]
    fn removing_a_workspace_moves_its_carnets_to_the_default_one() {
        let state = state();
        state.add_workspace("gone").unwrap();
        (state.add_carnet("/data/2026-01-01-x", "gone")).unwrap();
        let fake = Fake::default();
        items(&state, &fake).remove_workspace("gone").unwrap();
        assert_eq!(
            state.require_item("/data/2026-01-01-x").unwrap().workspace,
            "default"
        );
    }

    #[test]
    fn the_workspace_is_the_one_given_else_the_session_else_the_fallback() {
        let state = state();
        let fake = Fake::default();
        let items = items(&state, &fake);
        assert_eq!(items.workspace(Some("default"), "x"), "default");
        assert_eq!(items.workspace(Some("nope"), "x"), "side");
        assert_eq!(items.workspace(None, "x"), "side");
        let elsewhere = items.in_session(Some("other"));
        assert_eq!(elsewhere.workspace(Some("nope"), "x"), "x");
        assert_eq!(elsewhere.in_session(None).workspace(None, "x"), "x");
    }

    #[test]
    fn create_passes_the_workspace_group_and_keys_to_the_hooks() {
        let state = state();
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(LISTING))
            .always("zellij --session side action list-tabs", Some("[]"))
            .always("zellij --session side action new-tab", Some("4"))
            .always(
                "zellij --session side action list-panes",
                Some(r#"[{"id":7,"tab_id":4,"title":"editor","pane_cwd":"/r.ABC-1-x"}]"#),
            );
        let path = (items(&state, &fake))
            .create(Path::new("/r"), "ABC-1-x", "side", group("login").as_ref())
            .unwrap();
        assert_eq!(path, Path::new("/r.ABC-1-x"));
        assert_eq!(
            state.tab("/r.ABC-1-x").unwrap().map(|tab| tab.session),
            Some("side".into()),
            "without hooks, create opens the tab itself"
        );
        assert!(
            fake.calls().contains(
                &"env ATELIER_WORKSPACE=side ATELIER_GROUP=LOGIN ATELIER_ISSUE_KEYS= \
                  wt -C /r switch --create ABC-1-x --no-cd --yes"
                    .into()
            ),
            "the selected group, and no keys beyond the branch's: {:?}",
            fake.calls()
        );
        let item = state.require_item("/r.ABC-1-x").unwrap();
        assert_eq!(
            (
                item.workspace.as_str(),
                group_text(item.links().unwrap().group.as_ref())
            ),
            ("side", "LOGIN")
        );
        assert_eq!(item.links().unwrap().issue_keys, keys(&["ABC-1"]));
        let fake = Fake::default()
            .always("git -C /r branch", Some("  ABC-1-x"))
            .always("wt -C /r --config-set", Some(LISTING));
        (items(&state, &fake))
            .create(Path::new("/r"), "ABC-1-x", "side", group("").as_ref())
            .unwrap();
        assert!(fake.calls()[1].ends_with("switch ABC-1-x --no-cd --yes"));
        assert!(
            !fake.calls().iter().any(|call| call.contains("new-tab")),
            "the first create opened the tab"
        );
        assert_eq!(
            state
                .require_item("/r.ABC-1-x")
                .unwrap()
                .links()
                .unwrap()
                .group,
            group("LOGIN")
        );
    }

    #[test]
    fn create_without_a_key_links_nothing() {
        let state = state();
        let listing = r#"{"items":[{"branch":"fix","worktree":{"path":"/r.fix"}}]}"#;
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(listing))
            .always("zellij --session side action list-tabs", Some("[]"))
            .always("zellij --session side action new-tab", Some("4"))
            .always(
                "zellij --session side action list-panes",
                Some(r#"[{"id":7,"tab_id":4,"title":"editor","pane_cwd":"/r.fix"}]"#),
            );
        (items(&state, &fake))
            .create(Path::new("/r"), "fix", "side", group("slow pages").as_ref())
            .unwrap();
        assert!(fake.calls()[1].starts_with(
            "env ATELIER_WORKSPACE=side ATELIER_GROUP=SLOW PAGES ATELIER_ISSUE_KEYS= wt"
        ));
        let item = state.require_item("/r.fix").unwrap();
        assert_eq!(item.links().unwrap().group, group("SLOW PAGES"));
        assert!(item.links().unwrap().issue_keys.is_empty());
        let fake = Fake::default().always("wt -C /r --config-set", Some(r#"{"items":[]}"#));
        let error = (items(&state, &fake))
            .create(Path::new("/r"), "gone", "side", group("").as_ref())
            .unwrap_err();
        assert!(error.to_string().contains("no worktree on gone"));
    }

    #[test]
    fn checkout_switches_to_the_review_and_focuses_its_tab() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().canonicalize().unwrap();
        let state = state();
        worktree(&state, &tree, "", "side");
        tab(&state, &tree, "side", 4);
        let listing = format!(
            r#"{{"items":[{{"branch":"ABC-1-x","worktree":{{"path":"{}"}}}}]}}"#,
            tree.display()
        );
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(&listing))
            .always(
                "zellij --session side action list-tabs",
                Some(r#"[{"tab_id":4,"position":1,"name":"x"}]"#),
            )
            .always("zellij --session side action list-panes", Some("[]"));
        let items = items(&state, &fake);
        items
            .checkout(Path::new("/r"), "default", &review(12, "ABC-1-x"), None)
            .unwrap();
        let calls = fake.calls();
        assert_eq!(
            calls[0],
            "env ATELIER_WORKSPACE=default ATELIER_GROUP= ATELIER_ISSUE_KEYS= \
             wt -C /r switch pr:12 --no-cd --yes"
        );
        assert!(
            calls.contains(&"zellij --session side action go-to-tab-by-id 4".into()),
            "an existing worktree's tab is focused: {calls:?}"
        );
        let error =
            (items.checkout(Path::new("/r"), "default", &review(13, "gone"), None)).unwrap_err();
        assert!(error.to_string().contains("no worktree on gone"));
    }

    #[test]
    fn checkout_links_the_reviews_keys_then_the_branchs_in_the_group_given() {
        let state = state();
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(LISTING))
            .always("zellij --session side action list-tabs", Some("[]"))
            .always("zellij --session side action new-tab", Some("4"))
            .always(
                "zellij --session side action list-panes",
                Some(r#"[{"id":7,"tab_id":4,"title":"editor","pane_cwd":"/r.ABC-1-x"}]"#),
            );
        let items = items(&state, &fake);
        let review = Review {
            issue_keys: keys(&["DEF-4"]),
            ..review(12, "ABC-1-x")
        };
        (items.checkout(Path::new("/r"), "side", &review, group("login").as_ref())).unwrap();
        assert!(
            fake.calls()[0].contains("ATELIER_GROUP=LOGIN ATELIER_ISSUE_KEYS=DEF-4 "),
            "{:?}",
            fake.calls()
        );
        let item = state.require_item("/r.ABC-1-x").unwrap();
        assert_eq!(
            item.links().unwrap().clone(),
            links("LOGIN", &["DEF-4", "ABC-1"])
        );
    }

    #[test]
    fn start_links_the_issue_then_the_branchs_keys_in_the_group_given() {
        let state = state();
        let listing = r#"{"items":[{"branch":"5-fix-ABC-1","worktree":{"path":"/r.5-fix"}}]}"#;
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(listing))
            .always("zellij --session default action list-tabs", Some("[]"))
            .always("zellij --session default action new-tab", Some("4"))
            .always(
                "zellij --session default action list-panes",
                Some(r#"[{"id":7,"tab_id":4,"title":"editor","pane_cwd":"/r.5-fix"}]"#),
            );
        let login = group("login");
        (items(&state, &fake))
            .start(
                Path::new("/r"),
                "5-fix-ABC-1",
                "default",
                &key("o/r#5"),
                login.as_ref(),
            )
            .unwrap();
        assert!(
            fake.calls()[1].contains("ATELIER_GROUP=LOGIN ATELIER_ISSUE_KEYS=o/r#5 "),
            "only the issue's key: the hook adds the branch's; {:?}",
            fake.calls()
        );
        let item = state.require_item("/r.5-fix").unwrap();
        assert_eq!(
            item.links().unwrap().clone(),
            links("LOGIN", &["o/r#5", "ABC-1"])
        );
    }

    #[test]
    fn start_on_an_existing_worktree_adds_the_key_and_keeps_its_group() {
        let state = state();
        let repo = Path::new("/r");
        let mine = links("MINE", &["ABC-1"]);
        (state.add_worktree("/r.5-fix", repo, &mine, "default")).unwrap();
        tab(&state, "/r.5-fix", "side", 4);
        (state.add_worktree("/r.other", repo, &links("THEIRS", &[]), "default")).unwrap();
        (state.set_links("/r.other", &links("THEIRS", &["o/r#5"]))).unwrap();
        let listing = r#"{"items":[{"branch":"5-fix","worktree":{"path":"/r.5-fix"}}]}"#;
        let fake = Fake::default()
            .always("git -C /r branch", Some("  5-fix"))
            .always("wt -C /r --config-set", Some(listing));
        (items(&state, &fake))
            .start(
                Path::new("/r"),
                "5-fix",
                "default",
                &key("o/r#5"),
                group("x").as_ref(),
            )
            .unwrap();
        let item = state.require_item("/r.5-fix").unwrap();
        assert_eq!(item.links().unwrap().group, group("MINE"));
        assert_eq!(item.links().unwrap().issue_keys, keys(&["ABC-1", "o/r#5"]));
        assert!(
            !fake.calls().iter().any(|call| call.contains("rename-tab")),
            "its group, so its tab name, stays: {:?}",
            fake.calls()
        );
    }

    #[test]
    fn remove_forces_dirty_worktrees_and_forgets_them() {
        let state = state();
        worktree(&state, "/r.x", "", "default");
        let fake = Fake::default();
        let removal = Removal {
            path: "/r.x".into(),
            repo: "/r".into(),
            branch: Some("x".into()),
            force: true,
        };
        items(&state, &fake).remove(&[removal]).unwrap();
        assert_eq!(
            fake.calls(),
            ["wt -C /r remove --foreground --yes --force x"]
        );
        assert!(state.item("/r.x").unwrap().is_none());
    }

    #[test]
    fn finish_removes_then_closes_then_pulls_whatever_fails() {
        let state = state();
        worktree(&state, "/r.a", "G-1", "default");
        worktree(&state, "/r.b", "G-1", "default");
        let dir = tempfile::tempdir().unwrap();
        let notes = carnet::tests::repo(dir.path(), "2026-10-01-G-1-notes", None);
        (state.add_carnet(&notes, "default")).unwrap();
        let fake = Fake::default().always("wt -C /r remove --foreground --yes a", None);
        let removal = |branch: &str, force| Step::Remove {
            removal: Removal {
                path: format!("/r.{branch}").into(),
                repo: "/r".into(),
                branch: Some(branch.into()),
                force,
            },
            signal: Signal::Integrated,
        };
        let steps = [
            Step::Pull("/r".into()),
            Step::CloseCarnet(notes.clone()),
            removal("a", false),
            removal("b", true),
        ];
        let err = items(&state, &fake).finish(&steps).unwrap_err();
        assert!(err.to_string().contains("remove"), "{err}");
        let calls = fake.calls();
        let at = |command: &str| {
            (calls.iter().position(|call| call.contains(command)))
                .unwrap_or_else(|| panic!("no {command} in {calls:?}"))
        };
        assert_eq!(calls[0], "wt -C /r remove --foreground --yes a");
        assert_eq!(calls[1], "wt -C /r remove --foreground --yes --force b");
        assert!(at("commit -m Close") > 1);
        assert_eq!(calls.last().unwrap(), "git -C /r pull --ff-only --prune");
        assert!(!calls.iter().any(|call| call.contains(" -D")), "{calls:?}");
        assert!(
            state.item("/r.a").unwrap().is_some(),
            "kept: its removal failed"
        );
        assert!(state.item("/r.b").unwrap().is_none());
    }

    #[test]
    fn sync_marks_branches_whose_upstream_is_gone() {
        let state = state();
        let fake = Fake::default()
            .always("wt -C /r", Some(LISTING))
            .always("git -C /r for-each-ref", Some("main\0\nABC-1-x\0[gone]"));
        let synced = items(&state, &fake).sync(false).unwrap();
        let gone: Vec<bool> = (synced.worktrees.iter())
            .map(|(_, _, tree)| tree.gone)
            .collect();
        assert_eq!(gone, [false, true]);
        let fake = Fake::default()
            .always("wt -C /r", Some(LISTING))
            .always("git", None);
        let synced = items(&state, &fake).sync(false).unwrap();
        assert!(
            synced.failures.is_empty(),
            "a failed check is no failed listing"
        );
        assert!(synced.worktrees.iter().all(|(_, _, tree)| !tree.gone));
    }

    #[test]
    fn fetch_returns_the_repos_whose_fetch_failed() {
        let state = state();
        let fake = Fake::default().always("git -C /b fetch", None);
        let failed = items(&state, &fake).fetch(&["/a".into(), "/b".into()]);
        assert_eq!(
            fake.calls(),
            ["git -C /a fetch --prune", "git -C /b fetch --prune"]
        );
        assert_eq!(failed, [Path::new("/b")]);
    }

    #[test]
    fn move_to_reopens_a_tab_only_if_one_was_open() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let (open, closed) = (dir.path().join("open"), dir.path().join("closed"));
        for path in [&open, &closed] {
            std::fs::create_dir(path).unwrap();
            worktree(&state, path, "", "default");
        }
        tab(&state, &open, "default", 4);
        let panes = format!(
            r#"[{{"id":8,"tab_id":5,"title":"editor","pane_cwd":{}}}]"#,
            serde_json::to_string(&open).unwrap()
        );
        let fake = Fake::default()
            .always(
                "zellij --session default action list-tabs",
                Some(r#"[{"tab_id":4,"position":1}]"#),
            )
            .always("zellij --session default action list-panes", Some("[]"))
            .always("zellij --session side action list-tabs", Some("[]"))
            .always("zellij --session side action new-tab", Some("5"))
            .always("zellij --session side action list-panes", Some(&panes));
        (items(&state, &fake))
            .move_to(&[open.clone(), closed.clone()], "side")
            .unwrap();
        let calls = fake.calls();
        assert!(calls.contains(&"zellij --session default action close-tab-by-id 4".into()));
        assert_eq!(
            calls.iter().filter(|call| call.contains("new-tab")).count(),
            1
        );
        assert_eq!(state.tab(&open).unwrap().unwrap().session, "side");
        assert_eq!(state.tab(&closed).unwrap(), None);
        assert_eq!(state.require_item(&closed).unwrap().workspace, "side");
    }

    #[test]
    fn regroup_leaves_the_items_already_in_the_group_and_their_tabs_alone() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ngroup = \"A\"\nissues = []\nsummary = \"\"\nclosed = false\n+++\n";
        let path = carnet::tests::repo(dir.path(), "2026-10-01-notes", Some(readme));
        (state.add_carnet(&path, "default")).unwrap();
        tab(&state, &path, "default", 4);
        worktree(&state, "/r.a", "A", "default");
        tab(&state, "/r.a", "default", 5);
        let fake = Fake::default();
        let paths = [path, "/r.a".into()];
        (with_root(&state, &fake, dir.path()).regroup(&paths, group("a").as_ref())).unwrap();
        assert_eq!(fake.calls(), Vec::<String>::new(), "no commit, no rename");
    }

    #[test]
    fn regroup_normalises_the_group() {
        let state = state();
        worktree(&state, "/r.a", "", "default");
        worktree(&state, "/r.b", "", "default");
        tab(&state, "/r.a", "default", 4);
        let fake = Fake::default();
        let paths = ["/r.a".into(), "/r.b".into()];
        items(&state, &fake)
            .regroup(&paths, group(" slow pages").as_ref())
            .unwrap();
        for path in &paths {
            assert_eq!(
                state.require_item(path).unwrap().links().unwrap().group,
                group("SLOW PAGES")
            );
        }
        assert_eq!(fake.calls(), Vec::<String>::new(), "tabs are named apart");
    }

    #[test]
    fn regroup_regroups_every_item_when_one_fails() {
        let state = state();
        worktree(&state, "/r.a", "", "default");
        tab(&state, "/r.a", "default", 4);
        let fake = Fake::default();
        let paths = ["/r.unknown".into(), "/r.a".into()];
        let err = items(&state, &fake)
            .regroup(&paths, group("ABC-1").as_ref())
            .unwrap_err();
        assert!(err.to_string().contains("unknown item"), "{err}");
        assert_eq!(
            state.require_item("/r.a").unwrap().links().unwrap().group,
            group("ABC-1")
        );
    }

    #[test]
    fn forget_repo_closes_its_tabs() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        worktree(&state, dir.path(), "", "default");
        tab(&state, dir.path(), "default", 4);
        let fake = Fake::default()
            .always(
                "zellij --session default action list-tabs",
                Some(r#"[{"tab_id":4,"position":1}]"#),
            )
            .always("zellij --session default action list-panes", Some("[]"));
        items(&state, &fake).forget_repo(Path::new("/r")).unwrap();
        assert!(
            fake.calls()
                .contains(&"zellij --session default action close-tab-by-id 4".into())
        );
        assert_eq!(state.repos().unwrap(), []);
        assert_eq!(state.tabs().unwrap(), []);
        assert!(dir.path().exists());
    }

    /// The tab names `fake` was asked to set since its `from`th call.
    fn renamed(fake: &Fake, from: usize) -> Vec<String> {
        (fake.calls().into_iter().skip(from))
            .filter_map(|call| {
                let rest =
                    call.strip_prefix("zellij --session default action rename-tab-by-id ")?;
                Some(rest.to_owned())
            })
            .collect()
    }

    #[test]
    fn every_edit_leaves_each_open_tab_with_its_computed_name() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (a, b) = (root.join("a"), root.join("b"));
        for path in [&a, &b] {
            std::fs::create_dir(path).unwrap();
            worktree(&state, path, "", "default");
        }
        let readme = "+++\ngroup = \"A\"\n+++\n";
        let notes = carnet::tests::repo(&root, "2026-10-01-notes", Some(readme));
        state.add_carnet(&notes, "default").unwrap();
        for (path, id) in [(&a, 1), (&b, 2), (&notes, 3)] {
            tab(&state, path, "default", id);
        }
        // Every live tab has a name typed by hand, so each is renamed every time, by path.
        let live = r#"[{"tab_id":1,"position":1,"name":"x"},{"tab_id":2,"position":2,"name":"x"},
            {"tab_id":3,"position":3,"name":"x"}]"#;
        let fake = Fake::default()
            .always("zellij --session default action list-tabs", Some(live))
            .always("zellij --session default action list-panes", Some("[]"));
        let items = with_root(&state, &fake, &root);
        let named = |edit: &dyn Fn()| {
            let from = fake.calls().len();
            edit();
            items.name_tabs().unwrap();
            renamed(&fake, from)
        };
        assert_eq!(
            named(&|| ()),
            ["3 A·2026-10-01-notes", "1 r:a", "2 r:b"],
            "tabs renamed by hand are renamed back"
        );
        let both = [a.clone(), b.clone()];
        assert_eq!(
            named(&|| items.regroup(&both, group("g").as_ref()).unwrap()),
            ["3 A·2026-10-01-notes", "1 G·r:a", "2 G·r:b"],
            "a regroup: siblings in a group show their branch"
        );
        assert_eq!(
            named(&|| items.set_alias(Path::new("/r"), "rr").unwrap()),
            ["3 A·2026-10-01-notes", "1 G·rr:a", "2 G·rr:b"],
            "an alias change"
        );
        assert_eq!(
            named(&|| items.close(std::slice::from_ref(&b)).unwrap()),
            ["3 A·2026-10-01-notes", "1 G·rr"],
            "a close: the sibling left alone drops its branch"
        );
        let edited = "+++\ngroup = \"B\"\n+++\n";
        assert_eq!(
            named(&|| std::fs::write(notes.join("README.md"), edited).unwrap()),
            ["3 B·2026-10-01-notes", "1 G·rr"],
            "a README edited by hand"
        );
    }

    #[test]
    fn opening_a_tab_names_it_among_its_open_siblings() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (a, b) = (root.join("a"), root.join("b"));
        for path in [&a, &b] {
            std::fs::create_dir(path).unwrap();
            worktree(&state, path, "G", "default");
        }
        tab(&state, &a, "default", 1);
        let panes = format!(
            r#"[{{"id":8,"tab_id":2,"title":"editor","pane_cwd":{}}}]"#,
            serde_json::to_string(&b).unwrap()
        );
        let fake = Fake::default()
            .always(
                "zellij --session default action list-tabs",
                Some(r#"[{"tab_id":1,"position":1,"name":"G·r"}]"#),
            )
            .always("zellij --session default action new-tab", Some("2"))
            .always("zellij --session default action list-panes", Some(&panes))
            .always(&format!("git -C {}", b.display()), Some("feat"));
        let items = items(&state, &fake);
        items.open(std::slice::from_ref(&b)).unwrap();
        let opened = (fake.calls().into_iter()).find(|call| call.contains("new-tab"));
        assert!(
            opened.as_ref().unwrap().ends_with("--name G·r:feat"),
            "{opened:?}"
        );
        items.name_tabs().unwrap();
        assert_eq!(
            renamed(&fake, 0),
            [format!("1 G·r:{}", state::dir_name(&a))]
        );
    }

    #[test]
    fn a_readme_edited_by_hand_shows_in_the_next_refresh() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let notes =
            carnet::tests::repo(&root, "2026-10-01-notes", Some("+++\ngroup = \"A\"\n+++\n"));
        state.add_carnet(&notes, "default").unwrap();
        tab(&state, &notes, "default", 3);
        let fake = Fake::default()
            .always("wt", Some(r#"{"items":[]}"#))
            .always(
                "zellij --session default action list-tabs",
                Some(r#"[{"tab_id":3,"position":1,"name":"A·2026-10-01-notes"}]"#),
            )
            .always("zellij --session default action list-panes", Some("[]"));
        let readme = "+++\ngroup = \"b\"\nissues = [\"B-1\"]\n+++\n";
        std::fs::write(notes.join("README.md"), readme).unwrap();
        let items = with_root(&state, &fake, &root);
        assert_eq!(
            read(&state, &items.carnets).unwrap()[0].links,
            links("B", &["B-1"]),
            "the reader reads the folder"
        );
        let (snapshot, _) = items.snapshot(false).unwrap();
        assert_eq!(snapshot.carnets()[0].links, links("B", &["B-1"]));
        assert_eq!(renamed(&fake, 0), ["3 B·2026-10-01-notes"]);
    }

    #[test]
    fn create_carnet_records_it_in_the_workspace_given() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let items = with_root(&state, &fake, dir.path());
        let carnet = (items.create_carnet("notes", "side", &links("g", &["A-1"]), "")).unwrap();
        let item = state.require_item(&carnet.path).unwrap();
        assert_eq!(
            (item.record, item.workspace.as_str()),
            (state::Record::Carnet, "side")
        );
        assert!(
            items
                .create_carnet("other", "nope", &links("", &[]), "")
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "nothing made"
        );
    }

    #[test]
    fn create_carnet_whose_recording_fails_leaves_no_folder_behind() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("atelier.db");
        State::open(&db, "default").unwrap();
        let state = State::open_read_only(&db, "default").unwrap();
        let root = dir.path().join("carnets");
        std::fs::create_dir(&root).unwrap();
        let fake = Fake::default();
        let items = with_root(&state, &fake, &root);
        let err = items
            .create_carnet("notes", "default", &links("", &[]), "")
            .unwrap_err();
        assert!(err.to_string().contains("readonly"), "{err}");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn update_repo_checks_both_values_before_writing_either() {
        let state = state();
        let fake = Fake::default();
        let items = items(&state, &fake);
        let repo = Path::new("/r");
        assert!(items.update_repo(repo, Some("rr"), Some("nope")).is_err());
        assert_eq!(state.repo_by_path(repo).unwrap().unwrap().alias, None);
        items.update_repo(repo, Some("rr"), Some("side")).unwrap();
        assert_eq!(state.repo("rr").unwrap().default_workspace, "side");
        assert!(fake.calls().is_empty(), "tabs are renamed apart");
    }

    #[test]
    fn pull_skips_carnets() {
        let state = state();
        worktree(&state, "/r.a", "", "default");
        (state.add_carnet("/notes", "default")).unwrap();
        let fake = Fake::default();
        (items(&state, &fake))
            .pull(&["/r.a".into(), "/notes".into()])
            .unwrap();
        assert_eq!(fake.calls(), ["git -C /r.a pull --ff-only --prune"]);
    }
}
