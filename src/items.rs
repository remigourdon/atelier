//! Items: the worktrees and carnets atelier records, and every operation on them, shared by
//! the TUI, the CLI and the hooks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail, eyre};
use regex::Regex;

use crate::config::{Config, group_from_name};
use crate::process::{Logged, Runner};
use crate::reviews::Review;
use crate::state::{self, ItemKind, Repo, State, Tab};
use crate::worktrunk::{self, Forge, Worktree};
use crate::zellij::{Layouts, Zellij};
use crate::{carnet, git};

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
    pub group: String,
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
        tickets: Vec<String>,
        closed: bool,
        summary: String,
        /// The README as the snapshot read it, so a stale loaded one can be told apart.
        readme: Option<String>,
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

    /// A carnet's tickets; a worktree has none.
    pub fn tickets(&self) -> &[String] {
        match &self.kind {
            WorkKind::Carnet { tickets, .. } => tickets,
            WorkKind::Worktree { .. } => &[],
        }
    }

    /// An ungrouped carnet, listed in the `Carnets` group.
    pub fn in_carnets_group(&self) -> bool {
        self.group.is_empty() && self.is_carnet()
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
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

/// Runs `action` on every value, then joins the errors.
fn each<T>(values: &[T], action: impl Fn(&T) -> Result<()>) -> Result<()> {
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
    ticket: Regex,
    names: carnet::Names,
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
            ticket: config.ticket_regex()?,
            names: carnet::Names::new(config.ticket_pattern())?,
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

    /// The ticket key in `name`, or `""`.
    fn key(&self, name: &str) -> String {
        group_from_name(&self.ticket, name)
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

    /// Creates a worktree on `branch` (or switches to the existing one) and opens its tab. Its
    /// group is the ticket key in the branch, else `group`. Returns its path.
    pub fn create(
        &self,
        repo: &Path,
        branch: &str,
        workspace: &str,
        group: &str,
    ) -> Result<PathBuf> {
        let mut placed = self.key(branch);
        if placed.is_empty() {
            placed = group.to_owned();
        }
        let path = self.switch_branch(repo, branch, workspace, &placed)?;
        if self.state.tab(&path)?.is_none() {
            self.zellij.open_tab(self.state, &path)?;
        }
        Ok(path)
    }

    /// Checks out a review's branch through worktrunk (`pr:N` or `mr:N`) and focuses its tab,
    /// whether the worktree is new or was there already. Its group is the ticket key in the
    /// branch, else in the review's title.
    pub fn checkout(&self, repo: &Path, workspace: &str, review: &Review) -> Result<()> {
        let mut group = self.key(&review.branch);
        if group.is_empty() {
            group = self.key(&review.title);
        }
        let target = review.provider.shortcut(review.number);
        let path = self.switch(repo, &[&target], &review.branch, workspace, &group)?;
        self.zellij.open_tab(self.state, &path).map(drop)
    }

    /// Creates a worktree on `branch` for an issue, in the issue's group, which links it to the
    /// issue. A worktree that already existed in another group moves to the issue's.
    pub fn start(&self, repo: &Path, branch: &str, workspace: &str, issue_key: &str) -> Result<()> {
        let path = self.switch_branch(repo, branch, workspace, issue_key)?;
        if self.state.require_item(&path)?.group != issue_key {
            self.state.set_group(&path, issue_key)?;
            self.zellij.sync_names(self.state, repo)?;
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
        group: &str,
    ) -> Result<PathBuf> {
        let target: &[&str] = if git::branch_exists(self.runner, repo, branch)? {
            &[branch]
        } else {
            &["--create", branch]
        };
        self.switch(repo, target, branch, workspace, group)
    }

    /// Runs `wt switch` with the workspace and group for atelier's hooks, then records the
    /// worktree on `branch` in case the hooks are not installed. Returns its path.
    fn switch(
        &self,
        repo: &Path,
        target: &[&str],
        branch: &str,
        workspace: &str,
        group: &str,
    ) -> Result<PathBuf> {
        worktrunk::switch(self.runner, repo, target, workspace, group)?;
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
        (self.state).add_item(&path, ItemKind::Worktree, Some(repo), group, workspace)?;
        Ok(path)
    }

    /// Creates a carnet in `workspace`, its ticket the key its name starts with, else `group`
    /// when it is a ticket key. Returns its path.
    pub fn create_carnet(&self, name: &str, workspace: &str, group: &str) -> Result<PathBuf> {
        let root = self.config.require_carnet_root()?;
        carnet::create(
            self.state,
            self.runner,
            &self.names,
            &root,
            name,
            workspace,
            group,
        )
    }

    /// Closes or reopens each carnet. Closing also closes its tab.
    pub fn set_carnets_closed(&self, paths: &[PathBuf], closed: bool) -> Result<()> {
        each(paths, |path| {
            carnet::set_closed(self.runner, path, closed)?;
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

    /// Puts each worktree in `group`, as given, and renames the tabs it changes. A carnet's
    /// first ticket becomes `group`, its others kept; an empty `group` removes it.
    pub fn regroup(&self, paths: &[PathBuf], group: &str) -> Result<()> {
        let mut repos = HashSet::new();
        for path in paths {
            let item = self.state.require_item(path)?;
            if item.is_carnet() {
                let tickets = carnet::set_first_ticket(self.runner, &self.names, path, group)?;
                (self.state).set_group(path, tickets.first().map_or("", String::as_str))?;
                self.zellij.rename_tab(self.state, path)?;
            } else {
                self.state.set_group(path, group)?;
                repos.extend(item.repo);
            }
        }
        for repo in repos {
            self.zellij.sync_names(self.state, &repo)?;
        }
        Ok(())
    }

    /// Sets a repo's alias, empty to clear it, and renames its tabs.
    pub fn set_alias(&self, repo: &Path, alias: &str) -> Result<()> {
        (self.state).update_repo(&repo.to_string_lossy(), Some(alias), None)?;
        self.zellij.sync_names(self.state, repo)
    }

    /// Sets the workspace a repo's new worktrees go to.
    pub fn set_repo_workspace(&self, repo: &Path, workspace: &str) -> Result<()> {
        (self.state).update_repo(&repo.to_string_lossy(), None, Some(workspace))
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
        let mut work = Vec::new();
        for (repo, item, tree) in synced.worktrees {
            work.push(Work {
                tab: self.state.tab(&tree.path)?.is_some(),
                path: tree.path.clone(),
                workspace: item.workspace,
                group: item.group,
                kind: WorkKind::Worktree {
                    repo_name: repo.name(),
                    repo: repo.path,
                    tree: Box::new(tree),
                },
            });
        }
        let carnets = self.scan_carnets()?;
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
    /// workspace with the group their branch names, and forgets the worktrees a listing no
    /// longer names.
    fn sync(&self, full: bool) -> Result<Synced> {
        let state = self.state;
        let mut synced = Synced::default();
        let mut listed = HashSet::new();
        for repo in state.repos()? {
            let listing = match worktrunk::list(self.runner, &repo.path, full) {
                Ok(listing) => listing,
                Err(err) => {
                    listed.extend(state.repo_items(&repo.path)?.into_iter().map(|i| i.path));
                    synced.failures.push((repo, err));
                    continue;
                }
            };
            if let Some(forge) = listing.forge {
                synced.forges.insert(repo.path.clone(), forge);
            }
            for mut tree in listing.worktrees {
                tree.path = canonical(&tree.path);
                let name = (tree.branch.clone()).unwrap_or_else(|| state::dir_name(&tree.path));
                state.add_item(
                    &tree.path,
                    ItemKind::Worktree,
                    Some(&repo.path),
                    &self.key(&name),
                    &repo.default_workspace,
                )?;
                listed.insert(tree.path.clone());
                let item = state.require_item(&tree.path)?;
                synced.worktrees.push((repo.clone(), item, tree));
            }
        }
        for item in state.items()? {
            if item.kind == ItemKind::Worktree && !listed.contains(&item.path) {
                state.remove_item(&item.path)?;
            }
        }
        Ok(synced)
    }

    /// Scans the carnet root when carnets are enabled: records each carnet found, a new one in
    /// the default workspace, caches its first ticket as its group, and deletes the rows of
    /// carnets no longer found. Returns every carnet, newest first.
    fn scan_carnets(&self) -> Result<Vec<Work>> {
        let state = self.state;
        let Some(root) = self.config.carnet_root() else {
            return Ok(Vec::new());
        };
        let found = carnet::scan(&root, &self.names)?;
        let mut carnets = Vec::new();
        for carnet in found {
            let group = carnet.group().to_owned();
            if state
                .item(&carnet.path)?
                .is_some_and(|item| !item.is_carnet())
            {
                continue;
            }
            let path = &carnet.path;
            let default = state.default_workspace();
            if !state.add_item(path, ItemKind::Carnet, None, &group, default)?
                && state.require_item(path)?.group != group
            {
                state.set_group(path, &group)?;
                // As with reconcile, zellij not running should not hide the carnets.
                let _ = self.zellij.rename_tab(state, path);
            }
            carnets.push(Work {
                workspace: state.require_item(path)?.workspace,
                tab: state.tab(path)?.is_some(),
                group,
                path: carnet.path,
                kind: WorkKind::Carnet {
                    tickets: carnet.tickets,
                    closed: carnet.closed,
                    summary: carnet.summary,
                    readme: carnet.readme,
                },
            });
        }
        let found: HashSet<&Path> = carnets.iter().map(|work| work.path.as_path()).collect();
        for item in state.carnets()? {
            if !found.contains(item.path.as_path()) {
                state.remove_item(&item.path)?;
            }
        }
        Ok(carnets)
    }

    /// The newest open carnet listing `key` among its tickets, else the group of the recorded
    /// item containing `dir`. Reads the folders only: no git or network command.
    pub fn carnet_path(&self, key: Option<&str>, dir: &Path) -> Result<PathBuf> {
        let root = self.config.require_carnet_root()?;
        let key = match key {
            Some(key) => key.to_owned(),
            None => {
                let dir = canonical(dir);
                let item = (self.state.items()?.into_iter())
                    .filter(|item| dir.starts_with(&item.path))
                    .max_by_key(|item| item.path.as_os_str().len())
                    .ok_or_else(|| {
                        eyre!(
                            "{} is in no worktree or carnet atelier knows",
                            dir.display()
                        )
                    })?;
                if item.group.is_empty() {
                    bail!("{} is in no group: pass a ticket key", item.path.display());
                }
                item.group
            }
        };
        let carnets = carnet::scan(&root, &self.names)?;
        match carnet::newest_open(&carnets, &key) {
            Some(carnet) => Ok(carnet.path.clone()),
            None => bail!("no open carnet for {key}: create one with `atelier carnet new`"),
        }
    }

    /// Records a worktree a hook reports, registering its repo when it is new, and opens its
    /// tab. Its workspace is `workspace` when it is one, its group `group` as given, else the
    /// ticket key in its branch. A worktree of a carnet is not tracked: returns `None`.
    pub fn record(
        &self,
        path: &Path,
        repo: &Path,
        branch: &str,
        workspace: Option<&str>,
        group: Option<&str>,
    ) -> Result<Option<Tab>> {
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
        let group = group.map_or_else(|| self.key(branch), str::to_owned);
        let workspace = self.workspace(workspace, &default_workspace);
        state.add_item(path, ItemKind::Worktree, Some(repo), &group, &workspace)?;
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
        (state.add_item(path, ItemKind::Worktree, repo, group, workspace)).unwrap();
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
            .map(|(_, item, _)| (item.workspace.as_str(), item.group.as_str()))
            .collect();
        assert_eq!(
            placed,
            [("default", ""), ("side", ""), ("default", "DEF-2")],
            "a recorded item keeps its workspace and group; a new one takes its branch's"
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
        let closed = "+++\ntickets = [\"web#3\"]\nclosed = true\n+++\n";
        let placed = carnet::tests::repo(&root, "2026-10-01-ABC-1-notes", None);
        let new = carnet::tests::repo(
            &root,
            "2026-10-02-G-2-bare",
            Some("+++\ntickets = [\"G-3\"]\n+++\n"),
        );
        let done = carnet::tests::repo(&root, "2026-09-01-done", Some(closed));
        state
            .add_item(&placed, ItemKind::Carnet, None, "OLD-1", "side")
            .unwrap();
        (state.add_item("/data/2026-01-01-gone", ItemKind::Carnet, None, "", "side")).unwrap();
        tab(&state, "/data/2026-01-01-gone", "side", 4);
        let fake = Fake::default().always("wt", Some(r#"{"items":[]}"#));
        let (snapshot, _) = items(&state, &fake).snapshot(false).unwrap();
        assert!(snapshot.work.is_empty() && snapshot.carnets.is_empty());
        assert!(
            state.item("/data/2026-01-01-gone").unwrap().is_some(),
            "disabled, carnet rows are left alone"
        );
        let (snapshot, _) = with_root(&state, &fake, &root).snapshot(false).unwrap();
        let listed = |work: &[Work]| -> Vec<(String, String, String)> {
            (work.iter())
                .map(|work| (work.title(), work.group.clone(), work.workspace.clone()))
                .collect()
        };
        let row = |title: &str, group: &str, workspace: &str| {
            (title.to_owned(), group.to_owned(), workspace.to_owned())
        };
        assert_eq!(
            listed(&snapshot.carnets),
            [
                row("2026-10-02-G-2-bare", "G-3", "default"),
                row("2026-10-01-ABC-1-notes", "ABC-1", "side"),
                row("2026-09-01-done", "web#3", "default"),
            ],
            "newest first; a known carnet keeps its workspace, a new one goes to the default"
        );
        assert_eq!(
            listed(&snapshot.work),
            [
                row("2026-10-02-G-2-bare", "G-3", "default"),
                row("2026-10-01-ABC-1-notes", "ABC-1", "side"),
            ],
            "open carnets only"
        );
        assert_eq!(
            state.require_item(&placed).unwrap().group,
            "ABC-1",
            "the group is cached"
        );
        assert_eq!(state.require_item(&new).unwrap().group, "G-3");
        assert!(state.item(&done).unwrap().is_some());
        assert_eq!(state.item("/data/2026-01-01-gone").unwrap(), None);
        assert_eq!(
            state.tabs().unwrap(),
            [],
            "a vanished carnet's tab row goes with it"
        );
    }

    #[test]
    fn regroup_sets_a_carnets_first_ticket() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ntickets = [\"A-1\", \"B-2\"]\n+++\n";
        let path = carnet::tests::repo(dir.path(), "2026-10-01-notes", Some(readme));
        state
            .add_item(&path, ItemKind::Carnet, None, "A-1", "default")
            .unwrap();
        tab(&state, &path, "default", 4);
        let fake = Fake::default();
        let items = with_root(&state, &fake, dir.path());
        items
            .regroup(std::slice::from_ref(&path), "atelier#14")
            .unwrap();
        let readme = std::fs::read_to_string(path.join("README.md")).unwrap();
        assert!(
            readme.contains(r#"tickets = ["atelier#14", "B-2"]"#),
            "{readme}"
        );
        assert_eq!(state.require_item(&path).unwrap().group, "atelier#14");
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.ends_with("commit -m Link atelier#14 -- README.md"))
        );
        assert!(
            calls.contains(
                &"zellij --session default action rename-tab-by-id 4 atelier#14·2026-10-01-notes"
                    .into()
            ),
            "{calls:?}"
        );
        items.regroup(std::slice::from_ref(&path), "").unwrap();
        assert_eq!(
            state.require_item(&path).unwrap().group,
            "B-2",
            "the next ticket"
        );
    }

    #[test]
    fn closing_a_carnet_records_it_and_closes_its_tab() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let path = carnet::tests::repo(dir.path(), "2026-10-01-notes", None);
        state
            .add_item(&path, ItemKind::Carnet, None, "", "default")
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
        (state.add_item("/data/2026-01-01-x", ItemKind::Carnet, None, "", "gone")).unwrap();
        let fake = Fake::default();
        items(&state, &fake).remove_workspace("gone").unwrap();
        assert_eq!(
            state.require_item("/data/2026-01-01-x").unwrap().workspace,
            "default"
        );
    }

    #[test]
    fn carnet_path_finds_the_newest_open_carnet_of_a_key_or_of_the_current_group() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("carnets");
        let older = carnet::tests::repo(&root, "2026-01-01-ABC-1-a", None);
        let newer = carnet::tests::repo(&root, "2026-02-01-ABC-1-b", None);
        carnet::tests::repo(
            &root,
            "2026-03-01-ABC-1-c",
            Some("+++\nclosed = true\n+++\n"),
        );
        let tree = dir.path().join("r.ABC-1-x");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        let tree = tree.canonicalize().unwrap();
        worktree(&state, &tree, "ABC-1", "default");
        std::fs::create_dir(dir.path().join("r.plain")).unwrap();
        let plain = dir.path().join("r.plain").canonicalize().unwrap();
        worktree(&state, &plain, "", "default");
        state
            .add_item(&older, ItemKind::Carnet, None, "ABC-1", "default")
            .unwrap();
        let fake = Fake::default();
        let items = with_root(&state, &fake, &root);
        let found = |key: Option<&str>, dir: &Path| {
            items.carnet_path(key, dir).map_err(|err| err.to_string())
        };
        assert_eq!(
            found(Some("ABC-1"), dir.path()),
            Ok(newer.clone()),
            "the closed one is skipped"
        );
        assert_eq!(
            found(None, &tree.join("src")),
            Ok(newer.clone()),
            "the worktree's group"
        );
        assert_eq!(
            found(None, &older),
            Ok(newer),
            "inside a carnet, its own group"
        );
        assert!(
            found(None, dir.path())
                .unwrap_err()
                .contains("no worktree or carnet")
        );
        assert!(found(None, &plain).unwrap_err().contains("no group"));
        assert!(
            found(Some("XYZ-9"), dir.path())
                .unwrap_err()
                .contains("atelier carnet new")
        );
        assert!(fake.calls().is_empty(), "no command runs");
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
    fn create_passes_the_workspace_and_group_to_the_hooks() {
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
            .create(Path::new("/r"), "ABC-1-x", "side", "XYZ-9")
            .unwrap();
        assert_eq!(path, Path::new("/r.ABC-1-x"));
        assert_eq!(
            state.tab("/r.ABC-1-x").unwrap().map(|tab| tab.session),
            Some("side".into()),
            "without hooks, create opens the tab itself"
        );
        assert!(
            fake.calls().contains(
                &"env ATELIER_WORKSPACE=side ATELIER_GROUP=ABC-1 wt -C /r switch --create ABC-1-x --no-cd --yes"
                    .into()
            ),
            "the branch's key wins over the selected group"
        );
        let item = state.require_item("/r.ABC-1-x").unwrap();
        assert_eq!(
            (item.workspace.as_str(), item.group.as_str()),
            ("side", "ABC-1")
        );
        let fake = Fake::default()
            .always("git -C /r branch", Some("  ABC-1-x"))
            .always("wt -C /r --config-set", Some(LISTING));
        (items(&state, &fake))
            .create(Path::new("/r"), "ABC-1-x", "side", "")
            .unwrap();
        assert!(fake.calls()[1].ends_with("switch ABC-1-x --no-cd --yes"));
        assert!(
            !fake.calls().iter().any(|call| call.contains("new-tab")),
            "the first create opened the tab"
        );
    }

    #[test]
    fn create_without_a_key_takes_the_selected_group_as_given() {
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
            .create(Path::new("/r"), "fix", "side", "atelier#14")
            .unwrap();
        assert!(fake.calls()[1].starts_with("env ATELIER_WORKSPACE=side ATELIER_GROUP=atelier#14"));
        assert_eq!(state.require_item("/r.fix").unwrap().group, "atelier#14");
        let fake = Fake::default().always("wt -C /r --config-set", Some(r#"{"items":[]}"#));
        let error = (items(&state, &fake))
            .create(Path::new("/r"), "gone", "side", "")
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
            .checkout(Path::new("/r"), "default", &review(12, "ABC-1-x"))
            .unwrap();
        let calls = fake.calls();
        assert_eq!(
            calls[0],
            "env ATELIER_WORKSPACE=default ATELIER_GROUP=ABC-1 wt -C /r switch pr:12 --no-cd --yes"
        );
        assert!(
            calls.contains(&"zellij --session side action go-to-tab-by-id 4".into()),
            "an existing worktree's tab is focused: {calls:?}"
        );
        let error = (items.checkout(Path::new("/r"), "default", &review(13, "gone"))).unwrap_err();
        assert!(error.to_string().contains("no worktree on gone"));
    }

    #[test]
    fn checkout_takes_the_group_from_the_title_when_the_branch_has_none() {
        let state = state();
        let fake = Fake::default().always("wt -C /r --config-set", Some(LISTING));
        let review = Review {
            title: "DEF-4: fix it".into(),
            ..review(12, "main")
        };
        let _ = items(&state, &fake).checkout(Path::new("/r"), "default", &review);
        assert!(fake.calls()[0].contains("ATELIER_GROUP=DEF-4 "));
    }

    #[test]
    fn start_puts_the_worktree_in_the_issues_group() {
        let state = state();
        // Recorded earlier in no group.
        worktree(&state, "/r.5-fix", "", "default");
        tab(&state, "/r.5-fix", "side", 4);
        let listing = r#"{"items":[{"branch":"5-fix","worktree":{"path":"/r.5-fix"}}]}"#;
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(listing))
            .always(
                "zellij --session side action list-tabs",
                Some(r#"[{"tab_id":4,"position":1,"name":"r:5-fix"}]"#),
            );
        (items(&state, &fake))
            .start(Path::new("/r"), "5-fix", "default", "r#5")
            .unwrap();
        assert!(fake.calls()[1].contains("ATELIER_GROUP=r#5 "));
        assert_eq!(state.require_item("/r.5-fix").unwrap().group, "r#5");
        assert!(
            fake.calls().iter().any(|call| call.contains("rename-tab")),
            "the tab is renamed for its group: {:?}",
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
    fn regroup_renames_tabs_and_keeps_a_github_key() {
        let state = state();
        worktree(&state, "/r.a", "", "default");
        worktree(&state, "/r.b", "", "default");
        tab(&state, "/r.a", "default", 4);
        let fake = Fake::default();
        let paths = ["/r.a".into(), "/r.b".into()];
        items(&state, &fake).regroup(&paths, "atelier#14").unwrap();
        for path in &paths {
            assert_eq!(state.require_item(path).unwrap().group, "atelier#14");
        }
        assert!(
            fake.calls().contains(
                &"zellij --session default action rename-tab-by-id 4 atelier#14·r".into()
            )
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
    fn pull_skips_carnets() {
        let state = state();
        worktree(&state, "/r.a", "", "default");
        (state.add_item("/notes", ItemKind::Carnet, None, "", "default")).unwrap();
        let fake = Fake::default();
        (items(&state, &fake))
            .pull(&["/r.a".into(), "/notes".into()])
            .unwrap();
        assert_eq!(fake.calls(), ["git -C /r.a pull --ff-only"]);
    }
}
