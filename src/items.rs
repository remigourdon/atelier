//! Items: the worktrees and carnets atelier records, and every operation on them, shared by
//! the TUI, the CLI and the hooks.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};
use regex::Regex;

use crate::carnet::{self, Carnets};
use crate::config::{Config, issue_keys};
#[cfg(test)]
use crate::finish::Signal;
use crate::finish::Step;
use crate::git;
use crate::hooks::Hints;
use crate::links::{Group, IssueKey, IssueKeys, Links};
use crate::process::{Logged, Runner};
use crate::reviews::Review;
use crate::state::{self, ItemKind, Repo, State, Tab};
use crate::worktrunk::{self, Forge, Worktree};
use crate::zellij::{Layouts, Zellij};

/// Everything loaded from the database, worktrunk and zellij.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// The zellij session the TUI runs in.
    pub here: Option<String>,
    /// The current session first.
    pub workspaces: Vec<String>,
    pub repos: Vec<Repo>,
    /// Worktrees and open carnets.
    pub work: Vec<Work>,
    /// Every carnet, closed ones included, newest first; none while carnets are disabled.
    pub carnets: Vec<Work>,
    /// Each repo's forge web page, by repo path.
    pub forges: HashMap<PathBuf, Forge>,
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

    pub fn group(&self) -> Option<&Group> {
        self.links.group.as_ref()
    }

    /// Whether it links the issue `key`.
    pub fn links_to(&self, key: &IssueKey) -> bool {
        self.links.links(key)
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
    issue_key: Regex,
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
            issue_key: config.issue_key_regex()?,
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
        let found: Vec<String> = (names.iter())
            .flat_map(|name| issue_keys(&self.issue_key, name))
            .collect();
        IssueKeys::resolve(found.iter().map(String::as_str), &self.config.tracker)
    }

    /// The keys a worktree on `branch` is recorded with: `extra`, then those in the branch.
    fn seeded(&self, extra: &IssueKeys, branch: &str) -> IssueKeys {
        let mut keys = extra.clone();
        keys.extend(self.keys(&[branch]).iter().cloned());
        keys
    }

    /// The one group among the items linking any of `keys`; items in no group do not count.
    /// Carnets are read from their folders, so a README edited since the last refresh counts as
    /// it is now.
    pub fn linked_group(&self, keys: &IssueKeys) -> Result<Option<Group>> {
        let worktrees = (self.state.items()?.into_iter())
            .filter(|item| !item.is_carnet())
            .map(|item| item.links);
        let carnets = (self.carnets.scan()?.into_iter()).map(|carnet| carnet.links);
        let mut groups: BTreeSet<Group> = (worktrees.chain(carnets))
            .filter(|links| links.issue_keys.shares(keys))
            .filter_map(|links| links.group)
            .collect();
        Ok(match groups.len() {
            1 => groups.pop_first(),
            _ => None,
        })
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
        each(paths, |path| {
            self.zellij.open_tab(self.state, path).map(drop)
        })
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
            self.zellij.open_tab(self.state, &path)?;
        }
        Ok(path)
    }

    /// The issue keys a review's checkout links beyond its branch's: those in its branch, then
    /// its title.
    pub fn review_keys(&self, review: &Review) -> IssueKeys {
        self.keys(&[&review.branch, &review.title])
    }

    /// Checks out a review's branch through worktrunk (`pr:N` or `mr:N`) and focuses its tab,
    /// whether the worktree is new or was there already. A new one is in `group` and links the
    /// review's keys.
    pub fn checkout(
        &self,
        repo: &Path,
        workspace: &str,
        review: &Review,
        group: Option<&Group>,
    ) -> Result<()> {
        let links = Links {
            group: group.cloned(),
            issue_keys: self.review_keys(review),
        };
        let target = review.provider.shortcut(review.number);
        let path = self.switch(repo, &[&target], &review.branch, workspace, &links)?;
        self.zellij.open_tab(self.state, &path).map(drop)
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
        let mut linked = self.state.require_item(&path)?.links.issue_keys;
        if !linked.links(issue_key) {
            linked.push(issue_key.clone());
            self.state.set_issue_keys(&path, &linked)?;
        }
        if self.state.tab(&path)?.is_none() {
            self.zellij.open_tab(self.state, &path)?;
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
        (self.state).add_item(&path, ItemKind::Worktree, Some(repo), &links, workspace)?;
        Ok(path)
    }

    /// Creates a carnet in `workspace` and in `group`, linking the issue key its name starts
    /// with. Returns its path.
    pub fn create_carnet(
        &self,
        name: &str,
        workspace: &str,
        group: Option<&Group>,
    ) -> Result<PathBuf> {
        (self.carnets).create(self.state, self.runner, name, workspace, group)
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
                self.zellij.open_tab(self.state, path)?;
            }
            Ok(())
        })
    }

    /// Edits an item's group and issue keys with `edit`: a worktree's in the database, a carnet's
    /// in its front matter, in one commit, and in its cached row. Returns the item, as it was.
    fn relink(&self, path: &Path, edit: impl FnOnce(&mut Links)) -> Result<state::Item> {
        let item = self.state.require_item(path)?;
        let links = if item.is_carnet() {
            self.carnets.relink(self.runner, path, edit)?
        } else {
            let mut links = item.links.clone();
            edit(&mut links);
            links
        };
        if links.group != item.links.group {
            self.state.set_group(path, links.group.as_ref())?;
        }
        if links.issue_keys != item.links.issue_keys {
            self.state.set_issue_keys(path, &links.issue_keys)?;
        }
        Ok(item)
    }

    /// Replaces the issue keys an item links.
    pub fn set_issue_keys(&self, path: &Path, keys: &IssueKeys) -> Result<()> {
        self.relink(path, |links| links.issue_keys = keys.clone())
            .map(drop)
    }

    /// Puts each item in `group`, or in none, and renames the tabs of those it moves. A carnet's
    /// group is written to its front matter.
    pub fn regroup(&self, paths: &[PathBuf], group: Option<&Group>) -> Result<()> {
        let mut repos = BTreeSet::new();
        let regrouped = each(paths, |path| {
            let item = self.relink(path, |links| links.group = group.cloned())?;
            if item.links.group.as_ref() == group {
                return Ok(());
            }
            if item.is_carnet() {
                return self.zellij.rename_tab(self.state, path);
            }
            repos.extend(item.repo);
            Ok(())
        });
        let repos: Vec<PathBuf> = repos.into_iter().collect();
        let renamed = each(&repos, |repo| self.zellij.sync_names(self.state, repo));
        match (regrouped, renamed) {
            (Err(regrouped), Err(renamed)) => Err(eyre!("{regrouped}; {renamed}")),
            (regrouped, renamed) => regrouped.and(renamed),
        }
    }

    /// Sets a repo's alias, empty to clear it, and renames its tabs.
    pub fn set_alias(&self, repo: &Path, alias: &str) -> Result<()> {
        self.update_repo(repo, Some(alias), None)?;
        self.rename_repo_tabs(repo)
    }

    /// Sets the workspace a repo's new worktrees go to.
    pub fn set_repo_workspace(&self, repo: &Path, workspace: &str) -> Result<()> {
        self.update_repo(repo, None, Some(workspace))
    }

    /// Changes a repo's alias (`Some("")` clears it) and/or the workspace its new worktrees go
    /// to, checking both before writing either. Its tabs keep their names until
    /// [`Self::rename_repo_tabs`].
    pub fn update_repo(
        &self,
        repo: &Path,
        alias: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<()> {
        (self.state).update_repo(&repo.to_string_lossy(), alias, workspace)
    }

    /// Renames a repo's open tabs, as after its alias changed.
    pub fn rename_repo_tabs(&self, repo: &Path) -> Result<()> {
        self.zellij.sync_names(self.state, repo)
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

    /// Syncs the recorded items with worktrunk and gathers what the panels show, with the
    /// carnets when they are enabled, and the repos that could not be listed.
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
        let tabs: HashSet<PathBuf> = (self.state.tabs()?.into_iter())
            .map(|tab| tab.path)
            .collect();
        let mut work = Vec::new();
        for (repo, item, tree) in synced.worktrees {
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
        let carnets = self.scan_carnets(&tabs)?;
        work.extend(carnets.iter().filter(|carnet| !carnet.closed()).cloned());
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
            carnets,
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
                state.add_item(
                    &tree.path,
                    ItemKind::Worktree,
                    Some(&repo.path),
                    &links,
                    &repo.default_workspace,
                )?;
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

    /// Scans the carnet root when carnets are enabled: records each carnet found, a new one in
    /// the default workspace, caches its group (to name its tab) and issue keys, and deletes the
    /// rows of carnets no longer found, when it finds any. Returns every carnet, newest first.
    fn scan_carnets(&self, tabs: &HashSet<PathBuf>) -> Result<Vec<Work>> {
        let state = self.state;
        let found = self.carnets.scan()?;
        let mut recorded: HashMap<PathBuf, state::Item> = (state.items()?.into_iter())
            .map(|item| (item.path.clone(), item))
            .collect();
        let mut carnets = Vec::new();
        for carnet in found {
            let (path, links) = (&carnet.path, &carnet.links);
            let default = state.default_workspace();
            let workspace = match recorded.remove(path) {
                Some(item) if !item.is_carnet() => continue,
                Some(item) => {
                    if item.links.issue_keys != links.issue_keys {
                        state.set_issue_keys(path, &links.issue_keys)?;
                    }
                    if item.links.group != links.group {
                        state.set_group(path, links.group.as_ref())?;
                        // As with reconcile, zellij not running should not hide the carnets.
                        let _ = self.zellij.rename_tab(state, path);
                    }
                    item.workspace
                }
                None => {
                    state.add_item(path, ItemKind::Carnet, None, links, default)?;
                    default.to_owned()
                }
            };
            carnets.push(Work {
                workspace,
                tab: tabs.contains(path),
                links: carnet.links,
                path: carnet.path,
                kind: WorkKind::Carnet {
                    closed: carnet.closed,
                    summary: carnet.summary,
                    readme: carnet.readme,
                },
            });
        }
        // A root that shows no carnet is more likely unmounted than emptied: its rows stay
        // until a scan finds one again.
        if !carnets.is_empty() {
            for item in recorded.into_values().filter(|item| item.is_carnet()) {
                state.remove_item(&item.path)?;
            }
        }
        Ok(carnets)
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
        state.add_item(path, ItemKind::Worktree, Some(repo), &links, &workspace)?;
        self.zellij.open_tab(state, path).map(Some)
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
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::links::group_text;
    use crate::links::tests::{group, key, keys, links};
    use crate::process::fake::Fake;
    use crate::reviews::{Provider, Role};
    use crate::zellij::layouts;

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
        let repo = Some(Path::new("/r"));
        (state.add_item(
            path,
            ItemKind::Worktree,
            repo,
            &links(group, &[]),
            workspace,
        ))
        .unwrap();
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
                let keys = item.links.issue_keys.join(",");
                (
                    item.workspace.as_str(),
                    group_text(item.links.group.as_ref()),
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
                group_text(item.links.group.as_ref()),
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
        state
            .add_item(&placed, ItemKind::Carnet, None, &links("OLD", &[]), "side")
            .unwrap();
        (state.add_item(
            "/data/2026-01-01-gone",
            ItemKind::Carnet,
            None,
            &links("", &[]),
            "side",
        ))
        .unwrap();
        tab(&state, "/data/2026-01-01-gone", "side", 4);
        let fake = Fake::default().always("wt", Some(r#"{"items":[]}"#));
        let (snapshot, _) = items(&state, &fake).snapshot(false).unwrap();
        assert!(snapshot.work.is_empty() && snapshot.carnets.is_empty());
        assert!(
            state.item("/data/2026-01-01-gone").unwrap().is_some(),
            "disabled, carnet rows are left alone"
        );
        let (snapshot, _) = with_root(&state, &fake, &root).snapshot(false).unwrap();
        let listed = |work: &[Work]| -> Vec<(String, String, String, String)> {
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
            listed(&snapshot.carnets),
            [
                row("2026-10-02-G-2-bare", "", "G-3", "default"),
                row("2026-10-01-ABC-1-notes", "LOGIN", "ABC-1", "side"),
                row("2026-09-01-done", "", "web#3", "default"),
            ],
            "newest first; a known carnet keeps its workspace, a new one goes to the default"
        );
        assert_eq!(
            listed(&snapshot.work),
            [
                row("2026-10-02-G-2-bare", "", "G-3", "default"),
                row("2026-10-01-ABC-1-notes", "LOGIN", "ABC-1", "side"),
            ],
            "open carnets only"
        );
        let cached = state.require_item(&placed).unwrap();
        assert_eq!(
            cached.links,
            links("LOGIN", &["ABC-1"]),
            "the group and keys are cached"
        );
        assert_eq!(
            state.require_item(&new).unwrap().links.issue_keys,
            keys(&["G-3"])
        );
        assert!(state.item(&done).unwrap().is_some());
        assert_eq!(state.item("/data/2026-01-01-gone").unwrap(), None);
        assert_eq!(
            state.tabs().unwrap(),
            [],
            "a vanished carnet's tab row goes with it"
        );
    }

    #[test]
    fn a_root_showing_no_carnet_keeps_their_rows() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let away = "/data/2026-01-01-away";
        (state.add_item(away, ItemKind::Carnet, None, &links("", &[]), "side")).unwrap();
        let fake = Fake::default().always("wt", Some(r#"{"items":[]}"#));
        for root in [dir.path().join("unmounted"), dir.path().to_owned()] {
            let (snapshot, _) = with_root(&state, &fake, &root).snapshot(false).unwrap();
            assert!(snapshot.carnets.is_empty());
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
        let linked = links("A", &["A-1", "B-2"]);
        (state.add_item(&path, ItemKind::Carnet, None, &linked, "default")).unwrap();
        tab(&state, &path, "default", 4);
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
            state.require_item(&path).unwrap().links.group,
            group("LOGIN REWRITE")
        );
        let calls = fake.calls();
        assert!(
            (calls.iter())
                .any(|call| call.ends_with("commit -m Set group LOGIN REWRITE -- README.md"))
        );
        assert!(
            calls.contains(
                &"zellij --session default action rename-tab-by-id 4 LOGIN REWRITE·2026-10-01-notes"
                    .into()
            ),
            "{calls:?}"
        );
        items
            .regroup(std::slice::from_ref(&path), group("").as_ref())
            .unwrap();
        assert_eq!(state.require_item(&path).unwrap().links.group, group(""));
        assert!((fake.calls().iter()).any(|call| call.ends_with("commit -m Ungroup -- README.md")));
    }

    #[test]
    fn set_issue_keys_replaces_an_items_keys_wherever_it_keeps_them() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ngroup = \"A\"\nissues = [\"A-1\"]\n+++\n";
        let notes = carnet::tests::repo(dir.path(), "2026-10-01-notes", Some(readme));
        (state.add_item(
            &notes,
            ItemKind::Carnet,
            None,
            &links("A", &["A-1"]),
            "default",
        ))
        .unwrap();
        tab(&state, &notes, "default", 4);
        worktree(&state, "/r.a", "B", "default");
        tab(&state, "/r.a", "default", 5);
        let fake = Fake::default();
        let items = with_root(&state, &fake, dir.path());
        items
            .set_issue_keys(&notes, &keys(&["B-2", "A-1"]))
            .unwrap();
        items
            .set_issue_keys(Path::new("/r.a"), &keys(&["C-3"]))
            .unwrap();
        assert_eq!(
            state.require_item(&notes).unwrap().links,
            links("A", &["B-2", "A-1"]),
            "the cached row follows"
        );
        assert!(
            std::fs::read_to_string(notes.join("README.md"))
                .unwrap()
                .contains("issues = [\"B-2\", \"A-1\"]")
        );
        assert_eq!(
            state.require_item("/r.a").unwrap().links,
            links("B", &["C-3"])
        );
        let calls = fake.calls();
        assert!(
            (calls.iter()).any(|call| call.ends_with("commit -m Link B-2 -- README.md")),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.contains("rename-tab")),
            "keys never name a tab: {calls:?}"
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
        state
            .add_item(&path, ItemKind::Carnet, None, &links("", &[]), "default")
            .unwrap();
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
        assert!(snapshot.work.is_empty() && snapshot.carnets[0].closed());
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
        (state.add_item(
            "/data/2026-01-01-x",
            ItemKind::Carnet,
            None,
            &links("", &[]),
            "gone",
        ))
        .unwrap();
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
                group_text(item.links.group.as_ref())
            ),
            ("side", "LOGIN")
        );
        assert_eq!(item.links.issue_keys, keys(&["ABC-1"]));
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
            state.require_item("/r.ABC-1-x").unwrap().links.group,
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
        assert_eq!(item.links.group, group("SLOW PAGES"));
        assert!(item.links.issue_keys.is_empty());
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
            "env ATELIER_WORKSPACE=default ATELIER_GROUP= ATELIER_ISSUE_KEYS=ABC-1 \
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
    fn checkout_links_the_keys_in_the_branch_then_the_title_in_the_group_given() {
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
            title: "DEF-4: fix it".into(),
            ..review(12, "ABC-1-x")
        };
        assert_eq!(items.review_keys(&review), keys(&["ABC-1", "DEF-4"]));
        (items.checkout(Path::new("/r"), "side", &review, group("login").as_ref())).unwrap();
        assert!(
            fake.calls()[0].contains("ATELIER_GROUP=LOGIN ATELIER_ISSUE_KEYS=ABC-1,DEF-4 "),
            "{:?}",
            fake.calls()
        );
        let item = state.require_item("/r.ABC-1-x").unwrap();
        assert_eq!(item.links, links("LOGIN", &["ABC-1", "DEF-4"]));
    }

    #[test]
    fn the_linked_group_is_the_one_group_among_the_items_linking_the_keys() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let linked = |path: &str, group: &str, linking: &[&str]| {
            let repo = Some(Path::new("/r"));
            let links = links(group, linking);
            (state.add_item(path, ItemKind::Worktree, repo, &links, "side")).unwrap();
        };
        linked("/r.a", "LOGIN", &["DEF-4"]);
        linked("/r.b", "", &["ABC-1"]);
        let fake = Fake::default();
        let items = with_root(&state, &fake, dir.path());
        let group_of = |linking: &[&str]| items.linked_group(&keys(linking)).unwrap();
        assert_eq!(
            group_of(&["ABC-1", "DEF-4"]),
            group("LOGIN"),
            "no group does not count"
        );
        assert_eq!(group_of(&["ABC-1"]), None);
        assert_eq!(group_of(&["XYZ-9"]), None);
        linked("/r.c", "OTHER", &["ABC-1"]);
        assert_eq!(group_of(&["ABC-1", "DEF-4"]), None, "two groups: none");
        let notes = carnet::tests::repo(dir.path(), "2026-10-01-notes", Some("+++\n+++\n"));
        // Its row is stale: the lookup reads the folder.
        (state.add_item(
            &notes,
            ItemKind::Carnet,
            None,
            &links("OLD", &["Z-1"]),
            "side",
        ))
        .unwrap();
        let readme = "+++\ngroup = \"notes\"\nissues = [\"Y-1\"]\n+++\n";
        std::fs::write(notes.join("README.md"), readme).unwrap();
        assert_eq!(group_of(&["Z-1"]), None);
        assert_eq!(group_of(&["Y-1"]), group("NOTES"));
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
        assert_eq!(item.links, links("LOGIN", &["o/r#5", "ABC-1"]));
    }

    #[test]
    fn start_on_an_existing_worktree_adds_the_key_and_keeps_its_group() {
        let state = state();
        let repo = Some(Path::new("/r"));
        let mine = links("MINE", &["ABC-1"]);
        (state.add_item("/r.5-fix", ItemKind::Worktree, repo, &mine, "default")).unwrap();
        tab(&state, "/r.5-fix", "side", 4);
        (state.add_item(
            "/r.other",
            ItemKind::Worktree,
            repo,
            &links("THEIRS", &[]),
            "default",
        ))
        .unwrap();
        state.set_issue_keys("/r.other", &keys(&["o/r#5"])).unwrap();
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
        assert_eq!(item.links.group, group("MINE"));
        assert_eq!(item.links.issue_keys, keys(&["ABC-1", "o/r#5"]));
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
        (state.add_item(
            &notes,
            ItemKind::Carnet,
            None,
            &links("G-1", &[]),
            "default",
        ))
        .unwrap();
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
        (state.add_item(&path, ItemKind::Carnet, None, &links("A", &[]), "default")).unwrap();
        tab(&state, &path, "default", 4);
        worktree(&state, "/r.a", "A", "default");
        tab(&state, "/r.a", "default", 5);
        let fake = Fake::default();
        let paths = [path, "/r.a".into()];
        (with_root(&state, &fake, dir.path()).regroup(&paths, group("a").as_ref())).unwrap();
        assert_eq!(fake.calls(), Vec::<String>::new(), "no commit, no rename");
    }

    #[test]
    fn regroup_normalises_the_group_and_renames_tabs() {
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
                state.require_item(path).unwrap().links.group,
                group("SLOW PAGES")
            );
        }
        assert!(
            fake.calls().contains(
                &"zellij --session default action rename-tab-by-id 4 SLOW PAGES·r".into()
            )
        );
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
            state.require_item("/r.a").unwrap().links.group,
            group("ABC-1")
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("zellij --session default action rename-tab-by-id 4")),
            "{:?}",
            fake.calls()
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

    #[test]
    fn set_alias_renames_the_repos_tabs() {
        let state = state();
        worktree(&state, "/r.a", "", "default");
        tab(&state, "/r.a", "default", 4);
        let fake = Fake::default();
        items(&state, &fake)
            .set_alias(Path::new("/r"), "rr")
            .unwrap();
        assert_eq!(state.repo("rr").unwrap().path, Path::new("/r"));
        assert!(
            fake.calls()
                .contains(&"zellij --session default action rename-tab-by-id 4 rr:r.a".into())
        );
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
        (state.add_item("/notes", ItemKind::Carnet, None, &links("", &[]), "default")).unwrap();
        let fake = Fake::default();
        (items(&state, &fake))
            .pull(&["/r.a".into(), "/notes".into()])
            .unwrap();
        assert_eq!(fake.calls(), ["git -C /r.a pull --ff-only --prune"]);
    }
}
