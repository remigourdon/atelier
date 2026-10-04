//! Keeps the recorded items in step with the worktrees worktrunk lists.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Report, Result};
use regex::Regex;

use crate::config::group_from_name;
use crate::process::Runner;
use crate::state::{self, Item, ItemKind, Repo, State};
use crate::worktrunk::{self, Forge, Worktree};

/// A listed worktree with its recorded item.
pub struct Tracked {
    pub repo: Repo,
    pub item: Item,
    pub tree: Worktree,
}

#[derive(Default)]
pub struct Synced {
    pub worktrees: Vec<Tracked>,
    /// Each repo's forge web page, by repo path.
    pub forges: HashMap<PathBuf, Forge>,
    /// Repos whose listing failed; their items are kept as they were.
    pub failures: Vec<(Repo, Report)>,
}

/// The canonical path when it exists, so paths match the ones hooks record.
pub fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

/// Lists every repo's worktrees, records the unknown ones in their repo's default workspace
/// with the group their branch names, and forgets items whose worktree is gone.
pub fn sync(state: &State, runner: &dyn Runner, ticket: &Regex, full: bool) -> Result<Synced> {
    let mut synced = Synced::default();
    let mut listed = HashSet::new();
    for repo in state.repos()? {
        let listing = match worktrunk::list(runner, &repo.path, full) {
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
            let name = tree
                .branch
                .clone()
                .unwrap_or_else(|| state::dir_name(&tree.path));
            state.add_item(
                &tree.path,
                ItemKind::Worktree,
                Some(&repo.path),
                &group_from_name(ticket, &name),
                &repo.default_workspace,
            )?;
            listed.insert(tree.path.clone());
            synced.worktrees.push(Tracked {
                repo: repo.clone(),
                item: state.require_item(&tree.path)?,
                tree,
            });
        }
    }
    for item in state.items()? {
        // A carnet whose folder is gone was deleted on purpose.
        if !listed.contains(&item.path) && !item.path.exists() {
            state.remove_item(&item.path)?;
        }
    }
    Ok(synced)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::config::Config;
    use crate::process::fake::Fake;

    const LISTING: &str = r#"{"repo":{"forge":{"url":"https://forge/r"}},"items":[
        {"branch":"main","worktree":{"path":"/r","main":true}},
        {"branch":"ABC-1-x","worktree":{"path":"/r.ABC-1-x"}},
        {"branch":"DEF-2-y","worktree":{"path":"/r.DEF-2-y"}}]}"#;

    fn state() -> State {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("side").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        state
            .add_item(
                "/r.gone",
                ItemKind::Worktree,
                Some(Path::new("/r")),
                "",
                "default",
            )
            .unwrap();
        state
    }

    fn ticket() -> Regex {
        Config::default().ticket_regex().unwrap()
    }

    #[test]
    fn records_new_worktrees_and_forgets_vanished_ones() {
        let state = state();
        state
            .add_item(
                "/r.ABC-1-x",
                ItemKind::Worktree,
                Some(Path::new("/r")),
                "",
                "side",
            )
            .unwrap();
        let fake = Fake::default().always("wt -C /r", Some(LISTING));
        let synced = sync(&state, &fake, &ticket(), false).unwrap();
        assert!(synced.failures.is_empty());
        assert_eq!(synced.forges[Path::new("/r")].url, "https://forge/r");
        let items: Vec<_> = synced
            .worktrees
            .iter()
            .map(|s| (s.item.workspace.as_str(), s.item.group.as_str()))
            .collect();
        assert_eq!(
            items,
            [("default", ""), ("side", ""), ("default", "DEF-2")],
            "a recorded item keeps its workspace and group; a new one takes its branch's"
        );
        assert!(state.item("/r.gone").unwrap().is_none());
        sync(&state, &fake, &ticket(), true).unwrap();
        assert!(fake.calls().iter().any(|call| call.ends_with("--full")));
    }

    #[test]
    fn forgets_carnets_whose_folder_is_gone() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        state
            .add_item(dir.path(), ItemKind::Carnet, None, "", "default")
            .unwrap();
        state
            .add_item("/gone-carnet", ItemKind::Carnet, None, "", "default")
            .unwrap();
        let fake = Fake::default().always("wt -C /r", Some(LISTING));
        sync(&state, &fake, &ticket(), false).unwrap();
        assert!(state.item(dir.path()).unwrap().is_some());
        assert!(state.item("/gone-carnet").unwrap().is_none());
    }

    #[test]
    fn a_failed_listing_keeps_the_repos_items() {
        let state = state();
        let fake = Fake::default().always("wt", None);
        let synced = sync(&state, &fake, &ticket(), false).unwrap();
        assert!(synced.worktrees.is_empty());
        assert_eq!(synced.failures[0].0.name(), "r");
        assert!(state.item("/r.gone").unwrap().is_some());
    }
}
