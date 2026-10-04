//! Zellij orchestration: sessions, tabs, the anchor pane, reconcile and tab naming.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};
use serde::Deserialize;

use crate::config::Config;
use crate::process::{self, Runner};
use crate::state::{State, Tab};

pub const MAX_TAB_NAME: usize = 30;
const ATELIER_TAB: &str = "atelier";

/// The session (or worktree) layout: a zellij layout name or a file path.
#[derive(Debug, Clone)]
pub struct Layouts {
    pub session: String,
    pub worktree: String,
}

impl Layouts {
    /// Configured layouts, else the built-ins written under `$XDG_CACHE_HOME/atelier/layouts`.
    pub fn resolve(config: &Config) -> Result<Self> {
        let dir = crate::config::cache_home().join("atelier/layouts");
        let built_in = |name: &str, body: String| -> Result<String> {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(name);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(body.as_str()) {
                std::fs::write(&path, body)?;
            }
            Ok(path.to_string_lossy().into_owned())
        };
        Ok(Self {
            session: match &config.zellij.session_layout {
                Some(layout) => expand(layout),
                None => built_in("session.kdl", session_layout())?,
            },
            worktree: match &config.zellij.worktree_layout {
                Some(layout) => expand(layout),
                None => built_in(
                    "worktree.kdl",
                    worktree_layout(config.editor().as_deref(), config.agent_command()),
                )?,
            },
        })
    }
}

fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => crate::config::home()
            .join(rest)
            .to_string_lossy()
            .into_owned(),
        None => path.to_owned(),
    }
}

fn kdl_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialise")
}

pub fn session_layout() -> String {
    format!(
        r#"layout {{
    default_tab_template {{
        pane size=1 borderless=true {{
            plugin location="zellij:tab-bar"
        }}
        children
        pane size=2 borderless=true {{
            plugin location="zellij:status-bar"
        }}
    }}
    tab name="{ATELIER_TAB}" focus=true {{
        pane name="{ATELIER_TAB}" command="atelier" {{
            args "tui"
        }}
    }}
}}
"#
    )
}

/// The worktree tab: the anchor pane named `editor`, a shell, and a suspended agent.
pub fn worktree_layout(editor: Option<&str>, agent_command: &str) -> String {
    let editor = match editor {
        Some(editor) => format!(
            "pane size=\"60%\" name=\"editor\" command=\"sh\" focus=true {{\n            args \"-c\" {}\n        }}",
            kdl_string(&format!("exec {editor}"))
        ),
        None => "pane size=\"60%\" name=\"editor\" focus=true".to_owned(),
    };
    format!(
        r#"layout {{
    pane size=1 borderless=true {{
        plugin location="zellij:tab-bar"
    }}
    pane split_direction="vertical" {{
        {editor}
        pane stacked=true {{
            pane name="shell" expanded=true
            pane name="agent" command="sh" start_suspended=true {{
                args "-lc" {agent}
            }}
        }}
    }}
    pane size=2 borderless=true {{
        plugin location="zellij:status-bar"
    }}
}}
"#,
        agent = kdl_string(agent_command)
    )
}

/// Shortens `value` to `limit` characters by replacing its middle with `…`.
pub fn middle_elide(value: &str, limit: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= limit {
        return value.to_owned();
    }
    if limit <= 1 {
        return "…".to_owned();
    }
    let left = limit / 2;
    let right = limit - left - 1;
    let mut out: String = chars[..left].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - right..]);
    out
}

fn repo_branch(repo: &str, branch: &str, limit: usize) -> String {
    let value = format!("{repo}:{branch}");
    if value.chars().count() <= limit {
        return value;
    }
    let repo_limit = repo.chars().count().min(limit.saturating_sub(10).max(5));
    let branch_limit = limit.saturating_sub(repo_limit + 1);
    format!(
        "{}:{}",
        middle_elide(repo, repo_limit),
        middle_elide(branch, branch_limit)
    )
}

/// A tab's name: `group·repo` when grouped (with the branch when the group has
/// several tabs of that repo), else `repo` for a main worktree or `repo:branch`.
pub fn tab_name(group: &str, repo: &str, branch: &str, main: bool, duplicate: bool) -> String {
    if !group.is_empty() {
        let group = middle_elide(group, 18);
        let available = MAX_TAB_NAME - group.chars().count() - 1;
        let suffix = if duplicate {
            repo_branch(repo, branch, available)
        } else {
            middle_elide(repo, available)
        };
        return format!("{group}·{suffix}");
    }
    if main {
        middle_elide(repo, MAX_TAB_NAME)
    } else {
        repo_branch(repo, branch, MAX_TAB_NAME)
    }
}

#[derive(Debug, Deserialize)]
pub struct LiveTab {
    pub tab_id: u64,
    pub position: u64,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct LivePane {
    pub id: u64,
    pub tab_id: u64,
    #[serde(default)]
    pub is_plugin: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pane_cwd: Option<PathBuf>,
}

/// The session this process runs in, if any.
pub fn current_session() -> Option<String> {
    std::env::var_os("ZELLIJ")?;
    std::env::var("ZELLIJ_SESSION_NAME").ok()
}

pub struct Zellij<'a> {
    pub runner: &'a dyn Runner,
    /// The session we are running in, if any.
    pub here: Option<String>,
    pub layouts: Layouts,
    pub anchor: String,
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

impl Zellij<'_> {
    fn action(&self, session: &str, args: &[&str]) -> Result<String> {
        let mut full = vec!["--session", session, "action"];
        full.extend_from_slice(args);
        self.runner.output("zellij", &full)
    }

    pub fn tabs(&self, session: &str) -> Result<Vec<LiveTab>> {
        Ok(serde_json::from_str(
            &self.action(session, &["list-tabs", "--json"])?,
        )?)
    }

    pub fn panes(&self, session: &str) -> Result<Vec<LivePane>> {
        Ok(serde_json::from_str(
            &self.action(session, &["list-panes", "--all", "--json"])?,
        )?)
    }

    pub fn ensure_session(&self, session: &str) -> Result<()> {
        if self.tabs(session).is_ok() {
            return Ok(());
        }
        self.runner.output(
            "zellij",
            &[
                "--layout",
                &self.layouts.session,
                "attach",
                "--create-background",
                session,
            ],
        )?;
        Ok(())
    }

    /// Switches to (or attaches) a workspace's session, on its atelier tab when it has one.
    pub fn open_session(&self, session: &str) -> Result<()> {
        let Some(here) = &self.here else {
            return self.runner.interactive(
                "zellij",
                &[
                    "--layout",
                    &self.layouts.session,
                    "attach",
                    "--create",
                    session,
                ],
            );
        };
        let atelier = self
            .tabs(session)
            .ok()
            .and_then(|tabs| tabs.into_iter().find(|tab| tab.name == ATELIER_TAB));
        if here == session {
            if let Some(tab) = atelier {
                self.action(session, &["go-to-tab-by-id", &tab.tab_id.to_string()])?;
            }
            return Ok(());
        }
        let position = atelier.map(|tab| tab.position.to_string());
        let mut args = vec!["switch-session", session];
        if let Some(position) = &position {
            args.extend(["--tab-position", position]);
        }
        args.extend(["--layout", &self.layouts.session]);
        self.action(here, &args)?;
        Ok(())
    }

    /// Focuses a recorded tab, switching session first when it lives elsewhere.
    pub fn focus(&self, tab: &Tab) -> Result<()> {
        match &self.here {
            Some(here) if *here != tab.session => {
                self.action(
                    here,
                    &[
                        "switch-session",
                        &tab.session,
                        "--pane-id",
                        &tab.pane_id,
                        "--layout",
                        &self.layouts.session,
                    ],
                )?;
            }
            _ => {
                self.action(&tab.session, &["go-to-tab-by-id", &tab.tab_id.to_string()])?;
            }
        }
        Ok(())
    }

    fn anchor_in<'p>(
        &self,
        panes: &'p [LivePane],
        tab_id: Option<u64>,
        path: &Path,
    ) -> Option<&'p LivePane> {
        panes.iter().find(|pane| {
            !pane.is_plugin
                && pane.title == self.anchor
                && tab_id.is_none_or(|id| pane.tab_id == id)
                && pane
                    .pane_cwd
                    .as_deref()
                    .is_none_or(|cwd| same_path(cwd, path))
        })
    }

    /// The anchor pane of a tab: the pane titled `anchor_pane`.
    pub fn find_anchor(&self, session: &str, tab_id: u64, path: &Path) -> Result<String> {
        let panes = self.panes(session)?;
        self.anchor_in(&panes, Some(tab_id), path)
            .map(|pane| pane.id.to_string())
            .ok_or_else(|| eyre!("no pane named {} in tab {tab_id} of {session}", self.anchor))
    }

    /// Repairs tab ids changed by session resurrection and forgets closed tabs.
    pub fn reconcile(&self, state: &State) -> Result<()> {
        let tabs = state.tabs()?;
        let sessions: HashSet<&str> = tabs.iter().map(|tab| tab.session.as_str()).collect();
        let mut live = HashMap::new();
        for session in sessions {
            if let (Ok(tabs), Ok(panes)) = (self.tabs(session), self.panes(session)) {
                live.insert(session, (tabs, panes));
            }
        }
        for tab in &tabs {
            let path = Path::new(&tab.path);
            let Some((live_tabs, live_panes)) =
                live.get(tab.session.as_str()).filter(|_| path.exists())
            else {
                state.remove_tab(&tab.path)?;
                continue;
            };
            if live_tabs.iter().any(|live| live.tab_id == tab.tab_id) {
                continue;
            }
            match self
                .anchor_in(live_panes, None, path)
                .filter(|pane| pane.pane_cwd.is_some())
            {
                Some(pane) => state.set_tab(&Tab {
                    tab_id: pane.tab_id,
                    pane_id: pane.id.to_string(),
                    ..tab.clone()
                })?,
                None => state.remove_tab(&tab.path)?,
            }
        }
        Ok(())
    }

    /// Opens an item's tab in the workspace that owns it, or focuses the one already open.
    pub fn open_tab(&self, state: &State, path: &str) -> Result<Tab> {
        let item = state.require_item(path)?;
        self.reconcile(state)?;
        if let Some(tab) = state.tab(path)? {
            if tab.session == item.workspace {
                self.focus(&tab)?;
                if let Some(repo) = &item.repo {
                    state.touch_repo(repo)?;
                }
                return Ok(tab);
            }
            self.close_tab(state, path)?;
        }
        let session = &item.workspace;
        self.ensure_session(session)?;
        let name = self.name_for(state, path)?;
        let output = self.action(
            session,
            &[
                "new-tab",
                "--layout",
                &self.layouts.worktree,
                "--cwd",
                path,
                "--name",
                &name,
            ],
        )?;
        let tab_id = output
            .trim()
            .parse()
            .map_err(|_| eyre!("zellij new-tab printed no tab id: {output:?}"))?;
        let pane_id = self.find_anchor(session, tab_id, Path::new(path))?;
        let tab = Tab {
            path: path.to_owned(),
            session: session.clone(),
            tab_id,
            pane_id,
        };
        state.set_tab(&tab)?;
        if let Some(repo) = &item.repo {
            state.touch_repo(repo)?;
            self.sync_names(state, repo)?;
        }
        Ok(tab)
    }

    pub fn close_tab(&self, state: &State, path: &str) -> Result<()> {
        let Some(tab) = state.tab(path)? else {
            return Ok(());
        };
        // The tab may already be gone; forgetting it is what matters.
        let _ = self.action(&tab.session, &["close-tab-by-id", &tab.tab_id.to_string()]);
        state.remove_tab(path)?;
        if let Some(repo) = state.item(path)?.and_then(|item| item.repo) {
            self.sync_names(state, &repo)?;
        }
        Ok(())
    }

    /// The tab name for an item, counting it among the open tabs of its repo, workspace and group.
    fn name_for(&self, state: &State, path: &str) -> Result<String> {
        let item = state.require_item(path)?;
        let Some(repo_path) = &item.repo else {
            let name = crate::state::dir_name(path);
            return Ok(tab_name(&item.group, &name, "", true, false));
        };
        let repo = state
            .repo_by_path(repo_path)?
            .ok_or_else(|| eyre!("unknown repo: {repo_path}"))?;
        let siblings = self.open_siblings(state, &item)?;
        let branch =
            process::branch(self.runner, path).unwrap_or_else(|| crate::state::dir_name(path));
        Ok(tab_name(
            &item.group,
            &repo.name(),
            &branch,
            path == repo_path,
            !siblings.is_empty(),
        ))
    }

    fn open_siblings(&self, state: &State, item: &crate::state::Item) -> Result<Vec<String>> {
        let Some(repo) = &item.repo else {
            return Ok(Vec::new());
        };
        let mut siblings = Vec::new();
        for other in state.repo_items(repo)? {
            if other.path != item.path
                && other.workspace == item.workspace
                && other.group == item.group
                && state.tab(&other.path)?.is_some()
            {
                siblings.push(other.path);
            }
        }
        Ok(siblings)
    }

    /// Renames every open tab of a repo, so duplicates in a group show their branch.
    pub fn sync_names(&self, state: &State, repo: &str) -> Result<()> {
        for item in state.repo_items(repo)? {
            if let Some(tab) = state.tab(&item.path)? {
                let name = self.name_for(state, &item.path)?;
                self.action(
                    &tab.session,
                    &["rename-tab-by-id", &tab.tab_id.to_string(), &name],
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::process::fake::Fake;

    #[test]
    fn group_and_duplicate_repo() {
        assert_eq!(
            tab_name("ORD-123", "configue", "feature", false, false),
            "ORD-123·configue"
        );
        assert_eq!(
            tab_name("ORD-123", "configue", "feature", false, true),
            "ORD-123·configue:feature"
        );
    }

    #[test]
    fn ungrouped_worktree() {
        assert_eq!(tab_name("", "configue", "main", true, false), "configue");
        assert_eq!(
            tab_name("", "configue", "feature", false, false),
            "configue:feature"
        );
    }

    #[test]
    fn keeps_group_and_repo_up_to_30_characters() {
        let name = tab_name("ORD-123", "a-very-long-repository", "feature", false, false);
        assert_eq!(name, "ORD-123·a-very-long-repository");
        assert_eq!(name.chars().count(), 30);
    }

    #[test]
    fn keeps_both_ends_of_long_branch() {
        let name = tab_name(
            "",
            "configue",
            "atelier-verification-20260930",
            false,
            false,
        );
        assert_eq!(name, "configue:atelier-ve…n-20260930");
        assert_eq!(name.chars().count(), 30);
    }

    #[test]
    fn elides_long_repo_and_group() {
        let name = tab_name(
            "",
            "a-really-very-long-repository-name",
            "some-long-branch-name",
            false,
            false,
        );
        assert_eq!(name.chars().count(), 30);
        let name = tab_name("VERYLONGKEY-123456", "repository", "b", false, true);
        assert!(name.chars().count() <= 30, "{name}");
        assert_eq!(middle_elide("abcdef", 1), "…");
    }

    #[test]
    fn layouts_name_the_anchor_pane_and_quote_commands() {
        let layout = worktree_layout(Some("hx --vsplit"), "claude \"x\"");
        assert!(layout.contains(r#"name="editor""#));
        assert!(layout.contains(r#"args "-c" "exec hx --vsplit""#));
        assert!(layout.contains(r#"args "-lc" "claude \"x\"""#));
        assert!(worktree_layout(None, "claude").contains(r#"name="editor" focus=true"#));
        assert!(!worktree_layout(None, "claude").contains("nvim"));
    }

    fn state() -> State {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("w").unwrap();
        state.add_repo("/r", None, "w").unwrap();
        state
            .add_item("/r/a", "worktree", Some("/r"), "", "w")
            .unwrap();
        state
    }

    fn zellij<'a>(runner: &'a Fake, here: Option<&str>) -> Zellij<'a> {
        Zellij {
            runner,
            here: here.map(Into::into),
            layouts: Layouts {
                session: "S".into(),
                worktree: "W".into(),
            },
            anchor: "editor".into(),
        }
    }

    const PANES: &str = r#"[{"id":0,"tab_id":4,"is_plugin":true,"title":"editor"},
        {"id":7,"tab_id":4,"title":"editor","pane_cwd":"/r/a"},
        {"id":8,"tab_id":4,"title":"shell","pane_cwd":"/r/a"}]"#;

    #[test]
    fn open_tab_creates_the_tab_in_the_owning_session_and_records_the_anchor() {
        let state = state();
        let fake = Fake::default()
            .always("zellij --session w action list-tabs", Some("[]"))
            .always("zellij --session w action new-tab", Some("4\n"))
            .always("zellij --session w action list-panes", Some(PANES))
            .always("git -C /r/a", Some("feat"));
        let tab = zellij(&fake, Some("default"))
            .open_tab(&state, "/r/a")
            .unwrap();
        assert_eq!(
            tab,
            Tab {
                path: "/r/a".into(),
                session: "w".into(),
                tab_id: 4,
                pane_id: "7".into()
            }
        );
        assert_eq!(state.tab("/r/a").unwrap(), Some(tab));
        let calls = fake.calls();
        assert!(calls.contains(
            &"zellij --session w action new-tab --layout W --cwd /r/a --name r:feat".into()
        ));
        assert!(calls.contains(&"zellij --session w action rename-tab-by-id 4 r:feat".into()));
    }

    #[test]
    fn open_tab_starts_a_missing_session() {
        let state = state();
        let fake = Fake::default()
            .once("zellij --session w action list-tabs", None)
            .always("zellij --session w action list-tabs", Some("[]"))
            .always("zellij --session w action new-tab", Some("4"))
            .always("zellij --session w action list-panes", Some(PANES));
        zellij(&fake, None).open_tab(&state, "/r/a").unwrap();
        assert!(
            fake.calls()
                .contains(&"zellij --layout S attach --create-background w".into())
        );
    }

    #[test]
    fn open_tab_focuses_an_existing_tab_across_sessions() {
        let state = state();
        let fake = Fake::default()
            .always(
                "zellij --session w action list-tabs",
                Some(r#"[{"tab_id":4,"position":1}]"#),
            )
            .always("zellij --session w action list-panes", Some(PANES));
        // Reconcile forgets tabs whose path is gone, so use a real directory.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        state.add_item(path, "carnet", None, "", "w").unwrap();
        state
            .set_tab(&Tab {
                path: path.into(),
                session: "w".into(),
                tab_id: 4,
                pane_id: "7".into(),
            })
            .unwrap();
        zellij(&fake, Some("default"))
            .open_tab(&state, path)
            .unwrap();
        assert!(fake.calls().contains(
            &"zellij --session default action switch-session w --pane-id 7 --layout S".into()
        ));
        assert!(!fake.calls().iter().any(|call| call.contains("new-tab")));
    }

    #[test]
    fn reconcile_repairs_resurrected_ids_and_forgets_dead_tabs() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let alive = dir.path().join("alive");
        let gone = dir.path().join("gone");
        std::fs::create_dir(&alive).unwrap();
        std::fs::create_dir(&gone).unwrap();
        for path in [&alive, &gone] {
            let path = path.to_str().unwrap();
            state.add_item(path, "carnet", None, "", "w").unwrap();
            state
                .set_tab(&Tab {
                    path: path.into(),
                    session: "w".into(),
                    tab_id: 1,
                    pane_id: "1".into(),
                })
                .unwrap();
        }
        state.add_item("/missing", "carnet", None, "", "w").unwrap();
        state
            .set_tab(&Tab {
                path: "/missing".into(),
                session: "w".into(),
                tab_id: 9,
                pane_id: "9".into(),
            })
            .unwrap();
        let panes = format!(
            r#"[{{"id":5,"tab_id":3,"title":"editor","pane_cwd":{}}}]"#,
            serde_json::to_string(&alive).unwrap()
        );
        let fake = Fake::default()
            .always(
                "zellij --session w action list-tabs",
                Some(r#"[{"tab_id":3,"position":0}]"#),
            )
            .always("zellij --session w action list-panes", Some(&panes));
        zellij(&fake, None).reconcile(&state).unwrap();
        let tabs = state.tabs().unwrap();
        assert_eq!(tabs.len(), 1);
        assert_eq!((tabs[0].tab_id, tabs[0].pane_id.as_str()), (3, "5"));
        assert_eq!(tabs[0].path, alive.to_str().unwrap());
    }

    #[test]
    fn open_session_inside_zellij_switches_to_the_atelier_tab() {
        let fake = Fake::default().always(
            "zellij --session w action list-tabs",
            Some(r#"[{"tab_id":1,"position":0,"name":"atelier"}]"#),
        );
        zellij(&fake, Some("conf")).open_session("w").unwrap();
        assert_eq!(
            fake.calls().last().unwrap(),
            "zellij --session conf action switch-session w --tab-position 0 --layout S"
        );
        let fake = Fake::default();
        zellij(&fake, None).open_session("w").unwrap();
        assert_eq!(fake.calls(), ["zellij --layout S attach --create w"]);
    }

    #[test]
    fn closing_a_duplicate_renames_the_remaining_tab() {
        let state = state();
        state
            .add_item("/r/b", "worktree", Some("/r"), "", "w")
            .unwrap();
        for (path, id) in [("/r/a", 1), ("/r/b", 2)] {
            state
                .set_tab(&Tab {
                    path: path.into(),
                    session: "w".into(),
                    tab_id: id,
                    pane_id: "0".into(),
                })
                .unwrap();
        }
        state
            .add_item("/r/c", "worktree", Some("/r"), "G-1", "w")
            .unwrap();
        state
            .add_item("/r/d", "worktree", Some("/r"), "G-1", "w")
            .unwrap();
        for (path, id) in [("/r/c", 3), ("/r/d", 4)] {
            state
                .set_tab(&Tab {
                    path: path.into(),
                    session: "w".into(),
                    tab_id: id,
                    pane_id: "0".into(),
                })
                .unwrap();
        }
        let fake = Fake::default()
            .always("git -C /r/d", Some("d"))
            .always("git -C /r/c", Some("c"));
        let z = zellij(&fake, None);
        z.sync_names(&state, "/r").unwrap();
        assert!(
            fake.calls()
                .contains(&"zellij --session w action rename-tab-by-id 4 G-1·r:d".into())
        );
        z.close_tab(&state, "/r/c").unwrap();
        assert_eq!(
            fake.calls().last().unwrap(),
            "zellij --session w action rename-tab-by-id 4 G-1·r"
        );
    }
}
