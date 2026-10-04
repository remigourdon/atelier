//! The handlers behind `atelier hook <phase>`, run by worktrunk.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use color_eyre::eyre::{Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::config::group_from_name;
use crate::process;
use crate::state::{State, Tab};
use crate::zellij::Zellij;

/// The worktrunk hooks atelier handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Phase {
    PreStart,
    PreSwitch,
    PostRemove,
}

impl Phase {
    pub const ALL: [Phase; 3] = [Phase::PreStart, Phase::PreSwitch, Phase::PostRemove];

    /// The hook's name in worktrunk's config.
    pub fn name(self) -> &'static str {
        match self {
            Phase::PreStart => "pre-start",
            Phase::PreSwitch => "pre-switch",
            Phase::PostRemove => "post-remove",
        }
    }

    /// The command worktrunk runs for this hook.
    pub fn command(self) -> String {
        format!("atelier hook {}", self.name())
    }
}

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

/// What the caller knows about a new worktree, passed through worktrunk in the environment.
#[derive(Debug, Default)]
pub struct Hints {
    /// `ATELIER_GROUP_HINT`: a name to take the group from when the branch has no ticket key.
    pub group: String,
    /// `ATELIER_WORKSPACE`: the workspace a new worktree goes to, over the caller's session.
    pub workspace: Option<String>,
}

impl Hints {
    pub fn from_env() -> Self {
        Self {
            group: std::env::var("ATELIER_GROUP_HINT").unwrap_or_default(),
            workspace: std::env::var("ATELIER_WORKSPACE")
                .ok()
                .filter(|name| !name.is_empty()),
        }
    }
}

/// The canonical path, resolving the parent of one that does not exist yet (pre-start).
fn resolve(path: &Path) -> PathBuf {
    let resolved = path
        .canonicalize()
        .or_else(|err| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => parent.canonicalize().map(|parent| parent.join(name)),
            _ => Err(err),
        });
    resolved.unwrap_or_else(|_| path.to_owned())
}

/// Records the worktree and opens its tab (pre-start, pre-switch), or closes and forgets it (post-remove).
/// Returns the tab that was opened or focused.
pub fn handle(
    state: &State,
    zellij: &Zellij,
    ticket: &Regex,
    phase: Phase,
    payload: &Payload,
    hints: &Hints,
) -> Result<Option<Tab>> {
    let path = resolve(&payload.worktree_path);
    let branch = payload
        .branch
        .clone()
        .unwrap_or_else(|| crate::state::dir_name(&path));
    match phase {
        Phase::PostRemove => {
            zellij.close_tab(state, &path)?;
            state.remove_item(&path)?;
            return Ok(None);
        }
        Phase::PreSwitch => {
            // worktrunk reports the destination; a new worktree does not exist yet (pre-start
            // handles it), and before creation it may report the source's path.
            if !path.exists()
                || process::branch(zellij.runner, &path).as_deref() != Some(branch.as_str())
            {
                return Ok(None);
            }
        }
        Phase::PreStart => {}
    }
    let Some(repo_path) = payload
        .primary_worktree_path
        .as_ref()
        .or(payload.repo_path.as_ref())
    else {
        bail!("hook payload has neither primary_worktree_path nor repo_path");
    };
    let repo_path = resolve(repo_path);
    let known = |name: &&String| state.has_workspace(name).unwrap_or(false);
    let here = (hints.workspace.iter().find(known))
        .or(zellij.here.iter().find(known))
        .cloned();
    let repo = match state.repo_by_path(&repo_path)? {
        Some(repo) => repo,
        None => {
            let workspace = here.as_deref().unwrap_or(state.default_workspace());
            state.add_repo(&repo_path, None, workspace)?;
            eprintln!(
                "atelier: registered {} in {workspace}",
                crate::state::dir_name(&repo_path)
            );
            state
                .repo_by_path(&repo_path)?
                .expect("the repo was just added")
        }
    };
    let mut group = group_from_name(ticket, &branch);
    if group.is_empty() {
        group = group_from_name(ticket, &hints.group);
    }
    let workspace = here.unwrap_or(repo.default_workspace);
    state.add_item(&path, "worktree", Some(&repo_path), &group, &workspace)?;
    zellij.open_tab(state, &path).map(Some)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::config::Config;
    use crate::process::fake::Fake;
    use crate::zellij::Layouts;

    #[test]
    fn phases_use_worktrunk_names() {
        for phase in Phase::ALL {
            let value = phase.to_possible_value().unwrap();
            assert_eq!(value.get_name(), phase.name());
        }
        assert_eq!(Phase::PostRemove.command(), "atelier hook post-remove");
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
        fn path(&self, name: &str) -> PathBuf {
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
                .always(
                    &format!("git -C {}", self.path("wt").display()),
                    Some("ABC-1-x"),
                )
        }

        fn run(&self, fake: &Fake, here: Option<&str>, phase: Phase, branch: &str) -> Option<Tab> {
            self.run_with(fake, here, phase, branch, &Hints::default())
        }

        fn run_with(
            &self,
            fake: &Fake,
            here: Option<&str>,
            phase: Phase,
            branch: &str,
            hints: &Hints,
        ) -> Option<Tab> {
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
                hints,
            )
            .unwrap()
        }
    }

    #[test]
    fn new_worktree_in_a_session_registers_the_repo_there() {
        let w = world();
        let fake = w.fake();
        let tab = w.run(&fake, Some("w"), Phase::PreStart, "ABC-1-x").unwrap();
        assert_eq!(tab.session, "w");
        let repo = w.state.repo_by_path(w.path("repo")).unwrap().unwrap();
        assert_eq!(repo.default_workspace, "w");
        let item = w.state.require_item(w.path("wt")).unwrap();
        assert_eq!(
            (item.group.as_str(), item.workspace.as_str()),
            ("ABC-1", "w")
        );
    }

    #[test]
    fn outside_a_session_uses_the_repo_default() {
        let w = world();
        w.state.add_repo(w.path("repo"), None, "w").unwrap();
        let fake = w.fake();
        let tab = w.run(&fake, None, Phase::PreStart, "ABC-1-x").unwrap();
        assert_eq!(tab.session, "w");
        let w = world();
        let fake = w.fake();
        let tab = w.run(&fake, Some("unknown"), Phase::PreStart, "x").unwrap();
        assert_eq!(tab.session, "default");
    }

    #[test]
    fn existing_owner_wins_over_the_callers_session() {
        let w = world();
        w.state.add_repo(w.path("repo"), None, "default").unwrap();
        w.state
            .add_item(w.path("wt"), "worktree", Some(&w.path("repo")), "", "w")
            .unwrap();
        let fake = w.fake();
        let tab = w
            .run(&fake, Some("default"), Phase::PreSwitch, "ABC-1-x")
            .unwrap();
        assert_eq!(tab.session, "w");
    }

    #[test]
    fn hints_name_the_workspace_and_group_of_a_new_worktree() {
        let w = world();
        w.state.add_repo(w.path("repo"), None, "default").unwrap();
        let fake = w.fake();
        let hints = Hints {
            group: "XYZ-9".into(),
            workspace: Some("w".into()),
        };
        let tab = w
            .run_with(&fake, Some("default"), Phase::PreStart, "plain", &hints)
            .unwrap();
        assert_eq!(tab.session, "w");
        let item = w.state.require_item(w.path("wt")).unwrap();
        assert_eq!(
            (item.group.as_str(), item.workspace.as_str()),
            ("XYZ-9", "w")
        );
    }

    #[test]
    fn an_unknown_workspace_hint_is_ignored() {
        let w = world();
        let fake = w.fake();
        let hints = Hints {
            group: String::new(),
            workspace: Some("nope".into()),
        };
        let tab = w
            .run_with(&fake, Some("w"), Phase::PreStart, "x", &hints)
            .unwrap();
        assert_eq!(tab.session, "w");
    }

    #[test]
    fn pre_switch_ignores_a_path_on_another_branch() {
        let w = world();
        let fake = w.fake();
        assert!(w.run(&fake, Some("w"), Phase::PreSwitch, "other").is_none());
        assert!(w.state.item(w.path("wt")).unwrap().is_none());
    }

    #[test]
    fn post_remove_closes_and_forgets() {
        let w = world();
        let fake = w.fake();
        w.run(&fake, Some("w"), Phase::PreStart, "ABC-1-x").unwrap();
        w.run(&fake, Some("w"), Phase::PostRemove, "ABC-1-x");
        assert!(
            fake.calls()
                .contains(&"zellij --session w action close-tab-by-id 5".into())
        );
        assert!(w.state.item(w.path("wt")).unwrap().is_none());
        assert!(w.state.tabs().unwrap().is_empty());
    }
}
