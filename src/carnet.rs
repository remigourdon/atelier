//! Carnets: investigation folders `<root>/YYYY-MM-DD-<name>`, each its own git repo.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail};
use regex::Regex;

use crate::config::group_from_name;
use crate::process::Runner;
use crate::state::{self, State};

/// Creates `<root>/<today>-<name>` with a README titled `name`, makes it a git repo, and
/// records it in `workspace` and `group`, else the group its name gives. Returns its path.
pub fn create(
    state: &State,
    runner: &dyn Runner,
    ticket: &Regex,
    root: &Path,
    name: &str,
    workspace: &str,
    group: &str,
) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a carnet name must not be empty");
    }
    if name.contains('/') {
        bail!("a carnet name must not contain /");
    }
    state.require_workspace(workspace)?;
    let slug = name.split_whitespace().collect::<Vec<_>>().join("-");
    let path = root.join(format!("{}-{slug}", state.today()?));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    std::fs::create_dir_all(&path)?;
    let path = path.canonicalize()?;
    std::fs::write(path.join("README.md"), format!("# {name}\n"))?;
    runner.output("git", &["-C", &path.to_string_lossy(), "init", "--quiet"])?;
    let group = match group {
        "" => group_from_name(ticket, name),
        group => group.to_owned(),
    };
    state.add_item(&path, "carnet", None, &group, workspace)?;
    Ok(path)
}

/// Records an existing folder as a carnet in `workspace`, in the group its name gives.
/// Returns its canonical path.
pub fn add(state: &State, ticket: &Regex, path: &Path, workspace: &str) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let group = group_from_name(ticket, &state::dir_name(&path));
    if !state.add_item(&path, "carnet", None, &group, workspace)? {
        bail!("{} is already recorded", path.display());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::config::Config;
    use crate::process::fake::Fake;

    fn state() -> State {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("w").unwrap();
        state
    }

    fn ticket() -> Regex {
        Config::default().ticket_regex().unwrap()
    }

    #[test]
    fn new_carnets_are_dated_git_repos_with_a_readme() {
        let state = state();
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let path = create(
            &state,
            &fake,
            &ticket(),
            &root.path().join("Data"),
            " ABC-12 slow login ",
            "w",
            "",
        )
        .unwrap();
        let name = format!("{}-ABC-12-slow-login", state.today().unwrap());
        assert_eq!(
            path,
            root.path().canonicalize().unwrap().join("Data").join(&name)
        );
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "# ABC-12 slow login\n"
        );
        assert_eq!(
            fake.calls(),
            [format!("git -C {} init --quiet", path.display())]
        );
        let item = state.require_item(&path).unwrap();
        assert_eq!(
            (
                item.repo.as_deref(),
                item.group.as_str(),
                item.workspace.as_str()
            ),
            (None, "ABC-12", "w")
        );
        assert_eq!(state.carnets().unwrap(), [item]);
        let again = create(&state, &fake, &ticket(), root.path(), "x", "w", "G-1").unwrap();
        assert_eq!(state.require_item(&again).unwrap().group, "G-1");
    }

    #[test]
    fn a_carnet_name_must_be_a_plain_new_name() {
        let state = state();
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let make = |name: &str| create(&state, &fake, &ticket(), root.path(), name, "w", "");
        assert!(make("  ").unwrap_err().to_string().contains("empty"));
        assert!(make("a/b").unwrap_err().to_string().contains("/"));
        make("notes").unwrap();
        assert!(make("notes").unwrap_err().to_string().contains("exists"));
        assert!(
            create(&state, &fake, &ticket(), root.path(), "x", "nope", "").is_err(),
            "an unknown workspace"
        );
    }

    #[test]
    fn adding_records_an_existing_folder_once() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("2026-01-02-ORD-7-crash");
        std::fs::create_dir(&folder).unwrap();
        let path = add(&state, &ticket(), &folder, "w").unwrap();
        assert_eq!(path, folder.canonicalize().unwrap());
        let item = state.require_item(&path).unwrap();
        assert_eq!(
            (item.group.as_str(), item.workspace.as_str()),
            ("ORD-7", "w")
        );
        assert!(
            add(&state, &ticket(), &folder, "w")
                .unwrap_err()
                .to_string()
                .contains("already")
        );
        assert!(add(&state, &ticket(), &dir.path().join("missing"), "w").is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(
            add(&state, &ticket(), &file, "w")
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
    }
}
