//! The sqlite database: workspaces, repos, items, tabs and the remote cache.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail, eyre};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// Ordered and append-only: each runs once, tracked by `PRAGMA user_version`.
/// Migration 1 is the baseline schema, a no-op on databases that already have it.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE IF NOT EXISTS workspaces (
        name TEXT PRIMARY KEY
    );
    CREATE TABLE IF NOT EXISTS repos (
        path TEXT PRIMARY KEY,
        alias TEXT,
        default_workspace TEXT NOT NULL REFERENCES workspaces(name),
        last_used TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
    CREATE TABLE IF NOT EXISTS items (
        path TEXT PRIMARY KEY,
        kind TEXT NOT NULL CHECK (kind IN ('worktree', 'carnet')),
        repo TEXT REFERENCES repos(path),
        group_key TEXT NOT NULL DEFAULT '',
        workspace TEXT NOT NULL DEFAULT 'vrac' REFERENCES workspaces(name)
    );
    CREATE TABLE IF NOT EXISTS tabs (
        path TEXT PRIMARY KEY REFERENCES items(path),
        session TEXT NOT NULL,
        tab_id INTEGER NOT NULL,
        pane_id TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS cache (
        source TEXT NOT NULL,
        key TEXT NOT NULL,
        json TEXT NOT NULL,
        fetched_at TEXT NOT NULL,
        PRIMARY KEY (source, key)
    );
"];

#[derive(Debug, Clone, PartialEq)]
pub struct Repo {
    pub path: String,
    pub alias: Option<String>,
    pub default_workspace: String,
}

impl Repo {
    /// The alias, else the directory name.
    pub fn name(&self) -> String {
        self.alias.clone().unwrap_or_else(|| dir_name(&self.path))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub path: String,
    pub repo: Option<String>,
    pub group: String,
    pub workspace: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub path: String,
    pub session: String,
    pub tab_id: u64,
    pub pane_id: String,
}

pub struct State {
    db: Connection,
    default_workspace: String,
}

pub fn db_path() -> PathBuf {
    crate::config::state_home().join("atelier/atelier.db")
}

pub fn dir_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

impl State {
    pub fn open(path: &Path, default_workspace: &str) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::from_connection(Connection::open(path)?, default_workspace)
    }

    pub fn from_connection(mut db: Connection, default_workspace: &str) -> Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&mut db)?;
        db.execute(
            "INSERT OR IGNORE INTO workspaces(name) VALUES (?)",
            [default_workspace],
        )?;
        Ok(Self {
            db,
            default_workspace: default_workspace.to_owned(),
        })
    }

    pub fn default_workspace(&self) -> &str {
        &self.default_workspace
    }

    pub fn workspaces(&self) -> Result<Vec<String>> {
        let mut statement = self
            .db
            .prepare("SELECT name FROM workspaces ORDER BY name")?;
        let names = statement.query_map([], |row| row.get(0))?;
        Ok(names.collect::<rusqlite::Result<_>>()?)
    }

    pub fn has_workspace(&self, name: &str) -> Result<bool> {
        Ok(self.workspaces()?.iter().any(|known| known == name))
    }

    pub fn require_workspace(&self, name: &str) -> Result<()> {
        if !self.has_workspace(name)? {
            bail!("unknown workspace: {name}");
        }
        Ok(())
    }

    pub fn add_workspace(&self, name: &str) -> Result<()> {
        if name.is_empty() {
            bail!("workspace name must not be empty");
        }
        if self.has_workspace(name)? {
            bail!("workspace already exists: {name}");
        }
        self.db
            .execute("INSERT INTO workspaces(name) VALUES (?)", [name])?;
        Ok(())
    }

    pub fn remove_workspace(&self, name: &str) -> Result<()> {
        if name == self.default_workspace {
            bail!("{name} is the default workspace and cannot be removed");
        }
        self.require_workspace(name)?;
        let used = |sql: &str| -> Result<bool> {
            Ok(self
                .db
                .query_row(sql, [name], |_| Ok(()))
                .optional()?
                .is_some())
        };
        if used("SELECT 1 FROM repos WHERE default_workspace = ?")? {
            bail!("{name} is a repo default");
        }
        if used("SELECT 1 FROM tabs WHERE session = ?")? {
            bail!("{name} has open tabs");
        }
        if used("SELECT 1 FROM items WHERE workspace = ?")? {
            bail!("{name} owns items");
        }
        self.db
            .execute("DELETE FROM workspaces WHERE name = ?", [name])?;
        Ok(())
    }

    pub fn repos(&self) -> Result<Vec<Repo>> {
        let mut statement = self.db.prepare(
            "SELECT path, alias, default_workspace FROM repos ORDER BY last_used DESC, path",
        )?;
        let repos = statement.query_map([], |row| {
            Ok(Repo {
                path: row.get(0)?,
                alias: row.get(1)?,
                default_workspace: row.get(2)?,
            })
        })?;
        Ok(repos.collect::<rusqlite::Result<_>>()?)
    }

    pub fn repo_by_path(&self, path: &str) -> Result<Option<Repo>> {
        Ok(self.repos()?.into_iter().find(|repo| repo.path == path))
    }

    /// The one repo whose path, alias or directory name is `name`.
    pub fn repo(&self, name: &str) -> Result<Repo> {
        let mut matches: Vec<_> = self
            .repos()?
            .into_iter()
            .filter(|repo| {
                repo.path == name
                    || repo.alias.as_deref() == Some(name)
                    || dir_name(&repo.path) == name
            })
            .collect();
        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => bail!("repo not found: {name}"),
            _ => bail!("repo ambiguous: {name}"),
        }
    }

    fn check_alias(&self, alias: Option<&str>, path: &str) -> Result<()> {
        if let Some(alias) = alias
            && self
                .repos()?
                .iter()
                .any(|repo| repo.alias.as_deref() == Some(alias) && repo.path != path)
        {
            bail!("alias already in use: {alias}");
        }
        Ok(())
    }

    pub fn add_repo(&self, path: &str, alias: Option<&str>, workspace: &str) -> Result<()> {
        self.require_workspace(workspace)?;
        if self.repo_by_path(path)?.is_some() {
            bail!("repo already registered: {path}");
        }
        self.check_alias(alias, path)?;
        self.db.execute(
            "INSERT INTO repos(path, alias, default_workspace) VALUES (?, ?, ?)",
            params![path, alias, workspace],
        )?;
        Ok(())
    }

    /// Changes a repo's alias (`Some("")` clears it) and/or default workspace.
    pub fn update_repo(
        &self,
        name: &str,
        alias: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<()> {
        let repo = self.repo(name)?;
        let workspace = workspace.unwrap_or(&repo.default_workspace);
        self.require_workspace(workspace)?;
        let alias = match alias {
            Some("") => None,
            Some(alias) => Some(alias),
            None => repo.alias.as_deref(),
        };
        self.check_alias(alias, &repo.path)?;
        self.db.execute(
            "UPDATE repos SET alias = ?, default_workspace = ?, last_used = CURRENT_TIMESTAMP \
             WHERE path = ?",
            params![alias, workspace, repo.path],
        )?;
        Ok(())
    }

    pub fn touch_repo(&self, path: &str) -> Result<()> {
        self.db.execute(
            "UPDATE repos SET last_used = CURRENT_TIMESTAMP WHERE path = ?",
            [path],
        )?;
        Ok(())
    }

    /// Forgets a repo with its items and tabs. Returns the tabs that were recorded.
    pub fn remove_repo(&mut self, name: &str) -> Result<Vec<Tab>> {
        let path = self.repo(name)?.path;
        let items: Vec<String> = self
            .repo_items(&path)?
            .into_iter()
            .map(|item| item.path)
            .collect();
        let tabs: Vec<Tab> = self
            .tabs()?
            .into_iter()
            .filter(|tab| items.contains(&tab.path))
            .collect();
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM tabs WHERE path IN (SELECT path FROM items WHERE repo = ?)",
            [&path],
        )?;
        tx.execute("DELETE FROM items WHERE repo = ?", [&path])?;
        tx.execute("DELETE FROM repos WHERE path = ?", [&path])?;
        tx.commit()?;
        Ok(tabs)
    }

    /// Records an item unless it already exists. Returns whether it was new.
    pub fn add_item(
        &self,
        path: &str,
        kind: &str,
        repo: Option<&str>,
        group: &str,
        workspace: &str,
    ) -> Result<bool> {
        self.require_workspace(workspace)?;
        let added = self.db.execute(
            "INSERT OR IGNORE INTO items(path, kind, repo, group_key, workspace) \
             VALUES (?, ?, ?, ?, ?)",
            params![path, kind, repo, group, workspace],
        )?;
        Ok(added > 0)
    }

    pub fn item(&self, path: &str) -> Result<Option<Item>> {
        Ok(self
            .db
            .query_row(
                "SELECT path, repo, group_key, workspace FROM items WHERE path = ?",
                [path],
                |row| {
                    Ok(Item {
                        path: row.get(0)?,
                        repo: row.get(1)?,
                        group: row.get(2)?,
                        workspace: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn require_item(&self, path: &str) -> Result<Item> {
        self.item(path)?
            .ok_or_else(|| eyre!("unknown item: {path}"))
    }

    pub fn repo_items(&self, repo: &str) -> Result<Vec<Item>> {
        let mut statement = self
            .db
            .prepare("SELECT path FROM items WHERE repo = ? ORDER BY path")?;
        let paths: Vec<String> = statement
            .query_map([repo], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        paths.iter().map(|path| self.require_item(path)).collect()
    }

    pub fn remove_item(&self, path: &str) -> Result<()> {
        self.db.execute("DELETE FROM tabs WHERE path = ?", [path])?;
        self.db
            .execute("DELETE FROM items WHERE path = ?", [path])?;
        Ok(())
    }

    pub fn tabs(&self) -> Result<Vec<Tab>> {
        let mut statement = self
            .db
            .prepare("SELECT path, session, tab_id, pane_id FROM tabs ORDER BY path")?;
        let tabs = statement.query_map([], |row| {
            Ok(Tab {
                path: row.get(0)?,
                session: row.get(1)?,
                tab_id: row.get::<_, i64>(2)? as u64,
                pane_id: row.get(3)?,
            })
        })?;
        Ok(tabs.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tab(&self, path: &str) -> Result<Option<Tab>> {
        Ok(self.tabs()?.into_iter().find(|tab| tab.path == path))
    }

    pub fn set_tab(&self, tab: &Tab) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO tabs(path, session, tab_id, pane_id) VALUES (?, ?, ?, ?)",
            params![tab.path, tab.session, tab.tab_id as i64, tab.pane_id],
        )?;
        Ok(())
    }

    pub fn remove_tab(&self, path: &str) -> Result<()> {
        self.db.execute("DELETE FROM tabs WHERE path = ?", [path])?;
        Ok(())
    }
}

fn migrate(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let version = version as usize;
    if version > MIGRATIONS.len() {
        // A newer binary migrated this database; migrations only add, so carry on.
        return Ok(());
    }
    apply(&tx, version)?;
    tx.commit()?;
    Ok(())
}

fn apply(tx: &Transaction, from: usize) -> Result<()> {
    for migration in &MIGRATIONS[from..] {
        tx.execute_batch(migration)?;
    }
    tx.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema the prototype wrote, with no `user_version`.
    const BASELINE_FIXTURE: &str = include_str!("../tests/fixtures/baseline.sql");

    fn fresh() -> State {
        State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap()
    }

    fn version(state: &State) -> usize {
        let version: i64 = state
            .db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        version as usize
    }

    fn schema(db: &Connection) -> Vec<String> {
        let mut statement = db
            .prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn fresh_database_is_migrated_with_default_workspace() {
        let state = fresh();
        assert_eq!(version(&state), MIGRATIONS.len());
        assert_eq!(state.workspaces().unwrap(), ["default"]);
    }

    #[test]
    fn baseline_database_keeps_its_data_and_schema() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(BASELINE_FIXTURE).unwrap();
        let before = schema(&db);
        let state = State::from_connection(db, "default").unwrap();
        assert_eq!(version(&state), MIGRATIONS.len());
        assert_eq!(schema(&state.db), before);
        assert_eq!(state.workspaces().unwrap(), ["conf", "default", "vrac"]);
        let repo = state.repo("configue").unwrap();
        assert_eq!(repo.default_workspace, "conf");
        assert_eq!(state.tabs().unwrap().len(), 1);
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let path = tempfile::tempdir().unwrap();
        let path = path.path().join("atelier.db");
        State::open(&path, "default")
            .unwrap()
            .add_workspace("x")
            .unwrap();
        let state = State::open(&path, "default").unwrap();
        assert_eq!(state.workspaces().unwrap(), ["default", "x"]);
    }

    #[test]
    fn newer_database_is_left_alone() {
        let db = Connection::open_in_memory().unwrap();
        db.pragma_update(None, "user_version", 99).unwrap();
        db.execute_batch(MIGRATIONS[0]).unwrap();
        let state = State::from_connection(db, "default").unwrap();
        assert_eq!(version(&state), 99);
    }

    #[test]
    fn default_workspace_cannot_be_removed() {
        let state = fresh();
        assert!(state.remove_workspace("default").is_err());
        state.add_workspace("w").unwrap();
        state.remove_workspace("w").unwrap();
        assert!(state.remove_workspace("w").is_err());
    }

    #[test]
    fn workspace_in_use_cannot_be_removed() {
        let state = fresh();
        state.add_workspace("w").unwrap();
        state.add_repo("/r", None, "w").unwrap();
        assert!(
            state
                .remove_workspace("w")
                .unwrap_err()
                .to_string()
                .contains("repo default")
        );
        state.update_repo("r", None, Some("default")).unwrap();
        state
            .add_item("/r/x", "worktree", Some("/r"), "", "w")
            .unwrap();
        assert!(
            state
                .remove_workspace("w")
                .unwrap_err()
                .to_string()
                .contains("owns items")
        );
    }

    #[test]
    fn repos_resolve_by_path_alias_or_dir_name() {
        let state = fresh();
        state.add_repo("/a/proj", Some("p"), "default").unwrap();
        state.add_repo("/b/proj", None, "default").unwrap();
        assert_eq!(state.repo("p").unwrap().path, "/a/proj");
        assert_eq!(state.repo("/b/proj").unwrap().path, "/b/proj");
        assert!(
            state
                .repo("proj")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(state.add_repo("/c", Some("p"), "default").is_err());
        state.update_repo("p", Some(""), None).unwrap();
        assert_eq!(state.repo("/a/proj").unwrap().alias, None);
    }

    #[test]
    fn removing_a_repo_forgets_its_items_and_tabs() {
        let mut state = fresh();
        state.add_repo("/r", None, "default").unwrap();
        state
            .add_item("/r", "worktree", Some("/r"), "", "default")
            .unwrap();
        let tab = Tab {
            path: "/r".into(),
            session: "default".into(),
            tab_id: 3,
            pane_id: "4".into(),
        };
        state.set_tab(&tab).unwrap();
        assert_eq!(state.remove_repo("r").unwrap(), [tab]);
        assert!(state.item("/r").unwrap().is_none());
        assert!(state.tabs().unwrap().is_empty());
    }

    #[test]
    fn items_keep_their_first_workspace() {
        let state = fresh();
        state.add_workspace("w").unwrap();
        assert!(state.add_item("/x", "carnet", None, "", "w").unwrap());
        assert!(!state.add_item("/x", "carnet", None, "", "default").unwrap());
        assert_eq!(state.require_item("/x").unwrap().workspace, "w");
        assert!(state.add_item("/y", "carnet", None, "", "nope").is_err());
    }
}
