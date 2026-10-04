//! Worktrunk hooks: the handlers behind `atelier hook <phase>`, and their install in worktrunk's config.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail};
use regex::Regex;
use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::config::group_from_name;
use crate::process;
use crate::state::{State, Tab};
use crate::zellij::Zellij;

pub const PHASES: [&str; 3] = ["pre-start", "pre-switch", "post-remove"];
const ENTRY: &str = "atelier";

/// The context worktrunk passes a hook as JSON on stdin.
#[derive(Debug, Deserialize)]
pub struct Payload {
    pub worktree_path: PathBuf,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub primary_worktree_path: Option<PathBuf>,
    #[serde(default)]
    pub repo_path: Option<PathBuf>,
}

/// The canonical path, resolving the parent of one that does not exist yet (pre-start).
fn resolve(path: &Path) -> String {
    let resolved = path
        .canonicalize()
        .or_else(|err| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => parent.canonicalize().map(|parent| parent.join(name)),
            _ => Err(err),
        });
    resolved
        .unwrap_or_else(|_| path.to_owned())
        .to_string_lossy()
        .into_owned()
}

/// Records the worktree and opens its tab (pre-start, pre-switch), or closes and forgets it (post-remove).
/// Returns the tab that was opened or focused.
pub fn handle(
    state: &State,
    zellij: &Zellij,
    ticket: &Regex,
    phase: &str,
    payload: &Payload,
    group_hint: &str,
) -> Result<Option<Tab>> {
    let path = resolve(&payload.worktree_path);
    let branch = payload
        .branch
        .clone()
        .unwrap_or_else(|| crate::state::dir_name(&path));
    match phase {
        "post-remove" => {
            zellij.close_tab(state, &path)?;
            state.remove_item(&path)?;
            return Ok(None);
        }
        "pre-switch" => {
            // worktrunk reports the destination; a new worktree does not exist yet (pre-start
            // handles it), and before creation it may report the source's path.
            if !Path::new(&path).exists()
                || process::branch(zellij.runner, &path).as_deref() != Some(branch.as_str())
            {
                return Ok(None);
            }
        }
        "pre-start" => {}
        _ => bail!("unknown hook phase: {phase}"),
    }
    let Some(repo_path) = payload
        .primary_worktree_path
        .as_ref()
        .or(payload.repo_path.as_ref())
    else {
        bail!("hook payload has neither primary_worktree_path nor repo_path");
    };
    let repo_path = resolve(repo_path);
    let here = zellij
        .here
        .clone()
        .filter(|session| state.has_workspace(session).unwrap_or(false));
    let repo = match state.repo_by_path(&repo_path)? {
        Some(repo) => repo,
        None => {
            let workspace = here.as_deref().unwrap_or(state.default_workspace());
            state.add_repo(&repo_path, None, workspace)?;
            eprintln!(
                "atelier: registered {} in {workspace}",
                crate::state::dir_name(&repo_path)
            );
            state.repo(&repo_path)?
        }
    };
    let mut group = group_from_name(ticket, &branch);
    if group.is_empty() {
        group = group_from_name(ticket, group_hint);
    }
    let workspace = here.unwrap_or(repo.default_workspace);
    state.add_item(&path, "worktree", Some(&repo_path), &group, &workspace)?;
    zellij.open_tab(state, &path).map(Some)
}

fn command(phase: &str) -> String {
    format!("atelier hook {phase}")
}

/// Whether `phase` runs atelier's hook, in either named or plain-string form.
pub fn installed(doc: &DocumentMut, phase: &str) -> bool {
    match doc.get(phase) {
        Some(Item::Value(v)) if v.as_str() == Some(&command(phase)) => true,
        Some(item) => {
            item.as_table_like()
                .and_then(|table| table.get(ENTRY))
                .and_then(Item::as_str)
                == Some(&command(phase))
        }
        None => false,
    }
}

/// Adds a named `atelier` entry to each phase, keeping the user's other hooks.
pub fn install(doc: &mut DocumentMut) -> Result<()> {
    for phase in PHASES {
        let ours = command(phase);
        match doc.get_mut(phase) {
            None => {
                let mut table = Table::new();
                table.insert(ENTRY, value(&ours));
                doc.insert(phase, Item::Table(table));
            }
            Some(item) if item.as_str().is_some() => {
                let existing = item.as_str().unwrap().to_owned();
                let mut table = Table::new();
                if existing != ours {
                    table.insert("default", value(existing));
                }
                table.insert(ENTRY, value(&ours));
                *item = Item::Table(table);
            }
            Some(item) => match item.as_table_like_mut() {
                Some(table) => {
                    table.insert(ENTRY, value(&ours));
                }
                None => bail!(
                    "{phase} is a hook pipeline; add `{ENTRY} = \"{ours}\"` to a step by hand"
                ),
            },
        }
    }
    Ok(())
}

/// Removes atelier's entries, dropping phases left empty.
pub fn uninstall(doc: &mut DocumentMut) {
    for phase in PHASES {
        let ours = command(phase);
        let remove = match doc.get_mut(phase) {
            Some(item) if item.as_str() == Some(&ours) => true,
            Some(item) => match item.as_table_like_mut() {
                Some(table) => {
                    if table.get(ENTRY).and_then(Item::as_str) == Some(&ours) {
                        table.remove(ENTRY);
                    }
                    table.is_empty()
                }
                None => false,
            },
            None => false,
        };
        if remove {
            doc.remove(phase);
        }
    }
}

/// worktrunk's user config.
pub fn worktrunk_config() -> PathBuf {
    crate::config::config_home().join("worktrunk/config.toml")
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::config::Config;
    use crate::process::fake::Fake;
    use crate::zellij::Layouts;

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    #[test]
    fn install_adds_named_entries_and_keeps_other_hooks() {
        let mut d = doc(
            "worktree-path = \"x\"\npre-start = \"npm ci\"\n\n[post-remove]\nstop = \"kill\"\n",
        );
        install(&mut d).unwrap();
        assert!(PHASES.iter().all(|phase| installed(&d, phase)));
        assert_eq!(d["pre-start"]["default"].as_str(), Some("npm ci"));
        assert_eq!(d["post-remove"]["stop"].as_str(), Some("kill"));
        assert_eq!(
            d["pre-switch"]["atelier"].as_str(),
            Some("atelier hook pre-switch")
        );
        let once = d.to_string();
        install(&mut d).unwrap();
        assert_eq!(d.to_string(), once);
    }

    #[test]
    fn install_converts_atelier_plain_strings() {
        let mut d = doc("\"pre-start\" = \"atelier hook pre-start\"\n");
        assert!(installed(&d, "pre-start"));
        install(&mut d).unwrap();
        assert_eq!(d["pre-start"].as_table().unwrap().len(), 1);
        assert_eq!(
            d["pre-start"]["atelier"].as_str(),
            Some("atelier hook pre-start")
        );
    }

    #[test]
    fn install_refuses_pipelines() {
        let mut d = doc("[[pre-start]]\ninstall = \"npm ci\"\n");
        assert!(install(&mut d).is_err());
    }

    #[test]
    fn uninstall_removes_only_atelier() {
        let mut d =
            doc("pre-switch = \"atelier hook pre-switch\"\n[post-remove]\nstop = \"kill\"\n");
        install(&mut d).unwrap();
        uninstall(&mut d);
        assert!(PHASES.iter().all(|phase| !installed(&d, phase)));
        assert!(d.get("pre-start").is_none());
        assert!(d.get("pre-switch").is_none());
        assert_eq!(d["post-remove"]["stop"].as_str(), Some("kill"));
    }

    struct World {
        state: State,
        dir: tempfile::TempDir,
    }

    fn world() -> World {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("w").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("repo")).unwrap();
        std::fs::create_dir_all(dir.path().join("wt")).unwrap();
        World { state, dir }
    }

    impl World {
        fn path(&self, name: &str) -> String {
            resolve(&self.dir.path().join(name))
        }

        fn payload(&self, branch: &str) -> Payload {
            Payload {
                worktree_path: self.dir.path().join("wt"),
                branch: Some(branch.into()),
                primary_worktree_path: Some(self.dir.path().join("repo")),
                repo_path: None,
            }
        }

        fn fake(&self) -> Fake {
            let panes = format!(
                r#"[{{"id":2,"tab_id":5,"title":"editor","pane_cwd":{}}}]"#,
                serde_json::to_string(&self.path("wt")).unwrap()
            );
            Fake::default()
                .always("zellij --session w action list-tabs", Some("[]"))
                .always("zellij --session default action list-tabs", Some("[]"))
                .always("zellij --session w action new-tab", Some("5"))
                .always("zellij --session default action new-tab", Some("5"))
                .always("zellij --session w action list-panes", Some(&panes))
                .always("zellij --session default action list-panes", Some(&panes))
                .always(&format!("git -C {}", self.path("wt")), Some("ABC-1-x"))
        }

        fn run(&self, fake: &Fake, here: Option<&str>, phase: &str, branch: &str) -> Option<Tab> {
            let zellij = Zellij {
                runner: fake,
                here: here.map(Into::into),
                layouts: Layouts {
                    session: "S".into(),
                    worktree: "W".into(),
                },
                anchor: "editor".into(),
            };
            let ticket = Config::default().ticket_regex().unwrap();
            handle(
                &self.state,
                &zellij,
                &ticket,
                phase,
                &self.payload(branch),
                "",
            )
            .unwrap()
        }
    }

    #[test]
    fn new_worktree_in_a_session_registers_the_repo_there() {
        let w = world();
        let fake = w.fake();
        let tab = w.run(&fake, Some("w"), "pre-start", "ABC-1-x").unwrap();
        assert_eq!(tab.session, "w");
        assert_eq!(
            w.state.repo(&w.path("repo")).unwrap().default_workspace,
            "w"
        );
        let item = w.state.require_item(&w.path("wt")).unwrap();
        assert_eq!(
            (item.group.as_str(), item.workspace.as_str()),
            ("ABC-1", "w")
        );
    }

    #[test]
    fn outside_a_session_uses_the_repo_default() {
        let w = world();
        w.state.add_repo(&w.path("repo"), None, "w").unwrap();
        let fake = w.fake();
        assert_eq!(
            w.run(&fake, None, "pre-start", "ABC-1-x").unwrap().session,
            "w"
        );
        let w = world();
        let fake = w.fake();
        assert_eq!(
            w.run(&fake, Some("unknown"), "pre-start", "x")
                .unwrap()
                .session,
            "default"
        );
    }

    #[test]
    fn existing_owner_wins_over_the_callers_session() {
        let w = world();
        w.state.add_repo(&w.path("repo"), None, "default").unwrap();
        w.state
            .add_item(&w.path("wt"), "worktree", Some(&w.path("repo")), "", "w")
            .unwrap();
        let fake = w.fake();
        assert_eq!(
            w.run(&fake, Some("default"), "pre-switch", "ABC-1-x")
                .unwrap()
                .session,
            "w"
        );
    }

    #[test]
    fn pre_switch_ignores_a_path_on_another_branch() {
        let w = world();
        let fake = w.fake();
        assert!(w.run(&fake, Some("w"), "pre-switch", "other").is_none());
        assert!(w.state.item(&w.path("wt")).unwrap().is_none());
    }

    #[test]
    fn post_remove_closes_and_forgets() {
        let w = world();
        let fake = w.fake();
        w.run(&fake, Some("w"), "pre-start", "ABC-1-x").unwrap();
        w.run(&fake, Some("w"), "post-remove", "ABC-1-x");
        assert!(
            fake.calls()
                .contains(&"zellij --session w action close-tab-by-id 5".into())
        );
        assert!(w.state.item(&w.path("wt")).unwrap().is_none());
        assert!(w.state.tabs().unwrap().is_empty());
    }
}
