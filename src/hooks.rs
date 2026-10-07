//! The handlers behind `atelier hook <phase>`, run by worktrunk.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use color_eyre::eyre::{Result, bail};
use serde::Deserialize;

use crate::git;
use crate::items::Items;
use crate::links::Group;
use crate::process::Runner;
use crate::state::Tab;

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

/// The variables that carry [`Hints`] through worktrunk to the hook.
pub const GROUP_VAR: &str = "ATELIER_GROUP";
pub const ISSUE_KEYS_VAR: &str = "ATELIER_ISSUE_KEYS";
pub const WORKSPACE_VAR: &str = "ATELIER_WORKSPACE";

/// What the caller knows about a new worktree, passed through worktrunk in the environment.
#[derive(Debug, Default)]
pub struct Hints {
    /// `ATELIER_GROUP`: the worktree's group.
    pub group: Option<Group>,
    /// `ATELIER_ISSUE_KEYS`: the issue keys it links before those in its branch, comma-separated.
    pub issue_keys: Option<Vec<String>>,
    /// `ATELIER_WORKSPACE`: the workspace a new worktree goes to, over the caller's session.
    pub workspace: Option<String>,
}

impl Hints {
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// The hints in the variables `var` reads; an empty one gives none.
    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let var = |name| var(name).filter(|value| !value.trim().is_empty());
        Self {
            group: var(GROUP_VAR).and_then(|group| Group::parse(&group)),
            issue_keys: var(ISSUE_KEYS_VAR).map(|keys| {
                (keys.split(','))
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .map(str::to_owned)
                    .collect()
            }),
            workspace: var(WORKSPACE_VAR),
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
    items: &Items,
    runner: &dyn Runner,
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
            items.forget(&path)?;
            return Ok(None);
        }
        Phase::PreSwitch => {
            // worktrunk reports the destination; a new worktree does not exist yet (pre-start
            // handles it), and before creation it may report the source's path.
            if !path.exists() || git::branch(runner, &path).as_deref() != Some(branch.as_str()) {
                return Ok(None);
            }
        }
        Phase::PreStart => {}
    }
    let Some(repo) = payload
        .primary_worktree_path
        .as_ref()
        .or(payload.repo_path.as_ref())
    else {
        bail!("hook payload has neither primary_worktree_path nor repo_path");
    };
    items.record(&path, &resolve(repo), &branch, hints)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::config::Config;
    use crate::links::group_text;
    use crate::links::tests::{group, keys, links};
    use crate::process::fake::Fake;
    use crate::state::State;
    use crate::zellij::layouts;

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
            let config = Config::default();
            let items = Items::new(&self.state, fake, &config, layouts())
                .unwrap()
                .in_session(here);
            handle(&items, fake, phase, &self.payload(branch), hints).unwrap()
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
            (
                group_text(item.links().unwrap().group.as_ref()),
                item.workspace.as_str()
            ),
            ("", "w"),
            "no group without a hint"
        );
        assert_eq!(
            item.links().unwrap().issue_keys,
            keys(&["ABC-1"]),
            "the keys in its branch"
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
            .add_worktree(w.path("wt"), &w.path("repo"), &links("", &[]), "w")
            .unwrap();
        let fake = w.fake();
        let tab = w
            .run(&fake, Some("default"), Phase::PreSwitch, "ABC-1-x")
            .unwrap();
        assert_eq!(tab.session, "w");
    }

    #[test]
    fn worktrees_of_a_carnet_are_not_tracked() {
        let w = world();
        w.state.add_carnet(w.path("repo"), "w").unwrap();
        let fake = w.fake();
        assert_eq!(w.run(&fake, Some("w"), Phase::PreStart, "ABC-1-x"), None);
        assert_eq!(w.state.repos().unwrap(), []);
        assert_eq!(w.state.item(w.path("wt")).unwrap(), None);
        assert_eq!(fake.calls(), Vec::<String>::new());
    }

    #[test]
    fn hints_name_the_workspace_group_and_keys_of_a_new_worktree() {
        let w = world();
        w.state.add_repo(w.path("repo"), None, "default").unwrap();
        let fake = w.fake();
        let hints = Hints {
            group: group("LOGIN"),
            issue_keys: Some(vec!["XYZ-9".into(), "o/r#2".into()]),
            workspace: Some("w".into()),
        };
        let tab = w
            .run_with(&fake, Some("default"), Phase::PreStart, "ABC-1-x", &hints)
            .unwrap();
        assert_eq!(tab.session, "w");
        let item = w.state.require_item(w.path("wt")).unwrap();
        assert_eq!(
            (
                group_text(item.links().unwrap().group.as_ref()),
                item.workspace.as_str()
            ),
            ("LOGIN", "w")
        );
        assert_eq!(
            item.links().unwrap().issue_keys,
            keys(&["XYZ-9", "o/r#2", "ABC-1"]),
            "then the branch's keys"
        );
    }

    #[test]
    fn hints_are_read_from_the_environment_normalised() {
        let vars = |group: &str, keys: &str| {
            Hints::from_vars(|name| match name {
                GROUP_VAR => Some(group.to_owned()),
                ISSUE_KEYS_VAR => Some(keys.to_owned()),
                _ => None,
            })
        };
        let hints = vars("  login rewrite ", " ABC-1, ,o/r#2 ");
        assert_eq!(hints.group, group("LOGIN REWRITE"));
        assert_eq!(
            hints.issue_keys,
            Some(vec!["ABC-1".to_owned(), "o/r#2".to_owned()])
        );
        assert_eq!(hints.workspace, None);
        let empty = vars(" ", "");
        assert_eq!((empty.group, empty.issue_keys), (None, None));
    }

    #[test]
    fn an_unknown_workspace_hint_is_ignored() {
        let w = world();
        let fake = w.fake();
        let hints = Hints {
            workspace: Some("nope".into()),
            ..Hints::default()
        };
        let tab = w
            .run_with(&fake, Some("w"), Phase::PreStart, "x", &hints)
            .unwrap();
        assert_eq!(tab.session, "w");
    }

    #[test]
    fn a_group_hint_is_normalised_and_the_keys_still_come_from_the_branch() {
        let w = world();
        let fake = w.fake();
        let hints = Hints {
            group: group(" slow pages"),
            ..Hints::default()
        };
        w.run_with(&fake, Some("w"), Phase::PreStart, "ABC-1-x", &hints)
            .unwrap();
        let item = w.state.require_item(w.path("wt")).unwrap();
        assert_eq!(item.links().unwrap().group, group("SLOW PAGES"));
        assert_eq!(item.links().unwrap().issue_keys, keys(&["ABC-1"]));
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
