//! The sqlite database: workspaces, repos, items, tabs and the remote cache.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail, eyre};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, Transaction, params};

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
    pub path: PathBuf,
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
    pub path: PathBuf,
    pub repo: Option<PathBuf>,
    pub group: String,
    pub workspace: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub path: PathBuf,
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

/// A path's last component, or the whole path when it has none.
pub fn dir_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Paths are stored as text.
fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn path_column(row: &Row, index: usize) -> rusqlite::Result<PathBuf> {
    row.get::<_, String>(index).map(PathBuf::from)
}

impl State {
    pub fn open(path: &Path, default_workspace: &str) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::from_connection(Connection::open(path)?, default_workspace)
    }

    /// Opens an existing database without creating or migrating it, as completions need.
    pub fn open_read_only(path: &Path, default_workspace: &str) -> Result<Self> {
        Ok(Self {
            db: Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?,
            default_workspace: default_workspace.to_owned(),
        })
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
                path: path_column(row, 0)?,
                alias: row.get(1)?,
                default_workspace: row.get(2)?,
            })
        })?;
        Ok(repos.collect::<rusqlite::Result<_>>()?)
    }

    pub fn repo_by_path(&self, path: impl AsRef<Path>) -> Result<Option<Repo>> {
        let path = path.as_ref();
        Ok(self.repos()?.into_iter().find(|repo| repo.path == path))
    }

    /// The one repo whose path, alias or directory name is `name`.
    pub fn repo(&self, name: &str) -> Result<Repo> {
        let mut matches: Vec<_> = self
            .repos()?
            .into_iter()
            .filter(|repo| {
                repo.path == Path::new(name)
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

    fn check_alias(&self, alias: Option<&str>, path: &Path) -> Result<()> {
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

    pub fn add_repo(
        &self,
        path: impl AsRef<Path>,
        alias: Option<&str>,
        workspace: &str,
    ) -> Result<()> {
        let path = path.as_ref();
        self.require_workspace(workspace)?;
        if self.repo_by_path(path)?.is_some() {
            bail!("repo already registered: {}", path.display());
        }
        self.check_alias(alias, path)?;
        self.db.execute(
            "INSERT INTO repos(path, alias, default_workspace) VALUES (?, ?, ?)",
            params![text(path), alias, workspace],
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
            params![alias, workspace, text(&repo.path)],
        )?;
        Ok(())
    }

    pub fn touch_repo(&self, path: impl AsRef<Path>) -> Result<()> {
        self.db.execute(
            "UPDATE repos SET last_used = CURRENT_TIMESTAMP WHERE path = ?",
            [text(path.as_ref())],
        )?;
        Ok(())
    }

    /// Forgets a repo with its items and tabs.
    pub fn remove_repo(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = text(path.as_ref());
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM tabs WHERE path IN (SELECT path FROM items WHERE repo = ?)",
            [&path],
        )?;
        tx.execute("DELETE FROM items WHERE repo = ?", [&path])?;
        tx.execute("DELETE FROM repos WHERE path = ?", [&path])?;
        tx.commit()?;
        Ok(())
    }

    /// Records an item unless it already exists. Returns whether it was new.
    pub fn add_item(
        &self,
        path: impl AsRef<Path>,
        kind: &str,
        repo: Option<&Path>,
        group: &str,
        workspace: &str,
    ) -> Result<bool> {
        self.require_workspace(workspace)?;
        let added = self.db.execute(
            "INSERT OR IGNORE INTO items(path, kind, repo, group_key, workspace) \
             VALUES (?, ?, ?, ?, ?)",
            params![text(path.as_ref()), kind, repo.map(text), group, workspace],
        )?;
        Ok(added > 0)
    }

    pub fn item(&self, path: impl AsRef<Path>) -> Result<Option<Item>> {
        Ok(self
            .db
            .query_row(
                "SELECT path, repo, group_key, workspace FROM items WHERE path = ?",
                [text(path.as_ref())],
                |row| {
                    Ok(Item {
                        path: path_column(row, 0)?,
                        repo: row.get::<_, Option<String>>(1)?.map(PathBuf::from),
                        group: row.get(2)?,
                        workspace: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn require_item(&self, path: impl AsRef<Path>) -> Result<Item> {
        let path = path.as_ref();
        self.item(path)?
            .ok_or_else(|| eyre!("unknown item: {}", path.display()))
    }

    pub fn items(&self) -> Result<Vec<Item>> {
        let mut statement = self.db.prepare("SELECT path FROM items ORDER BY path")?;
        let paths: Vec<PathBuf> = statement
            .query_map([], |row| path_column(row, 0))?
            .collect::<rusqlite::Result<_>>()?;
        paths.iter().map(|path| self.require_item(path)).collect()
    }

    pub fn set_group(&self, path: impl AsRef<Path>, group: &str) -> Result<()> {
        let path = path.as_ref();
        self.require_item(path)?;
        self.db.execute(
            "UPDATE items SET group_key = ? WHERE path = ?",
            params![group, text(path)],
        )?;
        Ok(())
    }

    /// Moves an item to another workspace. Its tab, if any, stays where it is until reopened.
    pub fn set_workspace(&self, path: impl AsRef<Path>, workspace: &str) -> Result<()> {
        let path = path.as_ref();
        self.require_item(path)?;
        self.require_workspace(workspace)?;
        self.db.execute(
            "UPDATE items SET workspace = ? WHERE path = ?",
            params![workspace, text(path)],
        )?;
        Ok(())
    }

    pub fn repo_items(&self, repo: impl AsRef<Path>) -> Result<Vec<Item>> {
        let mut statement = self
            .db
            .prepare("SELECT path FROM items WHERE repo = ? ORDER BY path")?;
        let paths: Vec<PathBuf> = statement
            .query_map([text(repo.as_ref())], |row| path_column(row, 0))?
            .collect::<rusqlite::Result<_>>()?;
        paths.iter().map(|path| self.require_item(path)).collect()
    }

    pub fn remove_item(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = text(path.as_ref());
        self.db
            .execute("DELETE FROM tabs WHERE path = ?", [&path])?;
        self.db
            .execute("DELETE FROM items WHERE path = ?", [&path])?;
        Ok(())
    }

    pub fn tabs(&self) -> Result<Vec<Tab>> {
        let mut statement = self
            .db
            .prepare("SELECT path, session, tab_id, pane_id FROM tabs ORDER BY path")?;
        let tabs = statement.query_map([], |row| {
            Ok(Tab {
                path: path_column(row, 0)?,
                session: row.get(1)?,
                tab_id: row.get::<_, i64>(2)? as u64,
                pane_id: row.get(3)?,
            })
        })?;
        Ok(tabs.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tab(&self, path: impl AsRef<Path>) -> Result<Option<Tab>> {
        let path = path.as_ref();
        Ok(self.tabs()?.into_iter().find(|tab| tab.path == path))
    }

    pub fn set_tab(&self, tab: &Tab) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO tabs(path, session, tab_id, pane_id) VALUES (?, ?, ?, ?)",
            params![text(&tab.path), tab.session, tab.tab_id as i64, tab.pane_id],
        )?;
        Ok(())
    }

    pub fn remove_tab(&self, path: impl AsRef<Path>) -> Result<()> {
        self.db
            .execute("DELETE FROM tabs WHERE path = ?", [text(path.as_ref())])?;
        Ok(())
    }

    /// A cached remote response no older than `max_age` seconds, or of any age with `None`.
    pub fn cached(&self, source: &str, key: &str, max_age: Option<u64>) -> Result<Option<String>> {
        let since = max_age.map(|seconds| format!("-{seconds} seconds"));
        Ok(self
            .db
            .query_row(
                "SELECT json FROM cache WHERE source = ? AND key = ? \
                 AND (?3 IS NULL OR fetched_at >= datetime('now', ?3))",
                params![source, key, since],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn store_cache(&self, source: &str, key: &str, json: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO cache(source, key, json, fetched_at) \
             VALUES (?, ?, ?, datetime('now'))",
            params![source, key, json],
        )?;
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
    fn items_can_be_regrouped_and_moved() {
        let state = fresh();
        state.add_workspace("w").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        for path in ["/r", "/r/a"] {
            state
                .add_item(path, "worktree", Some(Path::new("/r")), "", "default")
                .unwrap();
        }
        state.set_group("/r/a", "ABC-1").unwrap();
        state.set_workspace("/r/a", "w").unwrap();
        let item = state.require_item("/r/a").unwrap();
        assert_eq!(
            (item.group.as_str(), item.workspace.as_str()),
            ("ABC-1", "w")
        );
        assert_eq!(state.items().unwrap().len(), 2);
        assert!(state.set_workspace("/r/a", "nope").is_err());
        assert!(state.set_group("/missing", "x").is_err());
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
            .add_item("/r/x", "worktree", Some(Path::new("/r")), "", "w")
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
        assert_eq!(state.repo("p").unwrap().path, Path::new("/a/proj"));
        assert_eq!(state.repo("/b/proj").unwrap().path, Path::new("/b/proj"));
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
            .add_item("/r", "worktree", Some(Path::new("/r")), "", "default")
            .unwrap();
        let tab = Tab {
            path: "/r".into(),
            session: "default".into(),
            tab_id: 3,
            pane_id: "4".into(),
        };
        state.set_tab(&tab).unwrap();
        state.remove_repo("/r").unwrap();
        assert!(state.repo_by_path("/r").unwrap().is_none());
        assert!(state.item("/r").unwrap().is_none());
        assert!(state.tabs().unwrap().is_empty());
    }

    #[test]
    fn read_only_open_neither_creates_nor_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atelier.db");
        assert!(State::open_read_only(&path, "default").is_err());
        assert!(!path.exists());
        State::open(&path, "default")
            .unwrap()
            .add_workspace("w")
            .unwrap();
        let state = State::open_read_only(&path, "default").unwrap();
        assert_eq!(state.workspaces().unwrap(), ["default", "w"]);
        assert!(state.add_workspace("x").is_err());
    }

    #[test]
    fn cache_entries_expire_but_stay_readable() {
        let state = fresh();
        assert_eq!(state.cached("gh", "k", Some(300)).unwrap(), None);
        state.store_cache("gh", "k", "[1]").unwrap();
        assert_eq!(
            state.cached("gh", "k", Some(300)).unwrap().as_deref(),
            Some("[1]")
        );
        assert_eq!(state.cached("glab", "k", None).unwrap(), None);
        state
            .db
            .execute(
                "UPDATE cache SET fetched_at = datetime('now', '-301 seconds')",
                [],
            )
            .unwrap();
        assert_eq!(state.cached("gh", "k", Some(300)).unwrap(), None);
        assert_eq!(
            state.cached("gh", "k", None).unwrap().as_deref(),
            Some("[1]")
        );
        state.store_cache("gh", "k", "[2]").unwrap();
        assert_eq!(
            state.cached("gh", "k", Some(300)).unwrap().as_deref(),
            Some("[2]")
        );
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
