//! Carnets: investigation folders `<root>/YYYY-MM-DD-[KEY-]<name>`, each its own git repo.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail};
use regex::Regex;

use crate::process::Runner;
use crate::state::{ItemKind, State, dir_name};

/// How carnet names carry a ticket key: right after the date in a folder name, and at the
/// start of a typed name.
#[derive(Debug, Clone)]
pub struct Names {
    dated: Regex,
    folder: Regex,
    typed: Regex,
    key: Regex,
}

impl Names {
    pub fn new(ticket_pattern: &str) -> Result<Self> {
        let key = format!("((?:{ticket_pattern}))");
        Ok(Self {
            dated: Regex::new(r"^\d{4}-\d{2}-\d{2}-.")?,
            folder: Regex::new(&format!(r"^\d{{4}}-\d{{2}}-\d{{2}}-{key}(?:-|$)"))?,
            typed: Regex::new(&format!(r"^{key}(?:[\s_-]+|$)"))?,
            key: Regex::new(&format!("^{key}$"))?,
        })
    }

    /// The group a carnet's folder name gives: the ticket key right after its date, or `""`.
    pub fn group(&self, folder: &str) -> String {
        (self.folder.captures(folder)).map_or_else(String::new, |captures| captures[1].to_owned())
    }

    /// The folder name, after the date, and the group of a carnet named `name` in `group`:
    /// the key `name` starts with, else `group`, which also prefixes the name when it is a key.
    fn slug(&self, name: &str, group: &str) -> (String, String) {
        let (key, rest) = match self.typed.captures(name) {
            Some(captures) => (captures[1].to_owned(), &name[captures[0].len()..]),
            None => (String::new(), name),
        };
        let group = if key.is_empty() { group } else { &key };
        let prefix = if self.key.is_match(group) { group } else { "" };
        let words = rest
            .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase);
        let slug = std::iter::once(prefix.to_owned())
            .filter(|prefix| !prefix.is_empty())
            .chain(words)
            .collect::<Vec<_>>()
            .join("-");
        (slug, group.to_owned())
    }
}

/// Creates `<root>/<today>-[KEY-]<name in kebab case>` with a README titled `name`, makes it a
/// git repo, and records it in `workspace`, in the group `name` starts with, else `group`.
/// Returns its path.
pub fn create(
    state: &State,
    runner: &dyn Runner,
    names: &Names,
    root: &Path,
    name: &str,
    workspace: &str,
    group: &str,
) -> Result<PathBuf> {
    let name = name.trim();
    if name.contains('/') {
        bail!("a carnet name must not contain /");
    }
    let (slug, group) = names.slug(name, group);
    if slug.is_empty() {
        bail!("a carnet name must not be empty");
    }
    state.require_workspace(workspace)?;
    let path = root.join(format!("{}-{slug}", state.today()?));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    std::fs::create_dir_all(&path)?;
    let path = path.canonicalize()?;
    let made = (|| {
        std::fs::write(path.join("README.md"), format!("# {name}\n"))?;
        crate::git::init(runner, &path)?;
        state.add_item(&path, ItemKind::Carnet, None, &group, workspace)
    })();
    // A half-made folder would block retrying under the same name.
    if let Err(err) = made {
        let _ = std::fs::remove_dir_all(&path);
        return Err(err);
    }
    Ok(path)
}

/// Records a dated git repo directly under `root` as a carnet in `workspace`, in the group its
/// name gives. Returns its canonical path.
pub fn add(
    state: &State,
    names: &Names,
    root: &Path,
    path: &Path,
    workspace: &str,
) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    if path.parent() != Some(root.canonicalize()?.as_path()) {
        bail!(
            "{} is not directly under {}",
            path.display(),
            root.display()
        );
    }
    let name = dir_name(&path);
    if !names.dated.is_match(&name) {
        bail!("{name} is not named YYYY-MM-DD-[KEY-]<name>: rename it first");
    }
    if !path.join(".git").exists() {
        bail!("{} is not a git repo", path.display());
    }
    if state.repo_by_path(&path)?.is_some() {
        bail!(
            "{} is a registered repo, which is never a carnet",
            path.display()
        );
    }
    if !state.add_item(
        &path,
        ItemKind::Carnet,
        None,
        &names.group(&name),
        workspace,
    )? {
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

    fn names() -> Names {
        Names::new(Config::default().ticket_pattern()).unwrap()
    }

    /// A git repo `<dir>/<name>`, as `git init` leaves it.
    fn repo(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        path
    }

    #[test]
    fn new_carnets_are_dated_git_repos_with_a_readme() {
        let state = state();
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let path = create(
            &state,
            &fake,
            &names(),
            &root.path().join("Data"),
            " ABC-12 Slow login_page ",
            "w",
            "",
        )
        .unwrap();
        let name = format!("{}-ABC-12-slow-login-page", state.today().unwrap());
        assert_eq!(
            path,
            root.path().canonicalize().unwrap().join("Data").join(&name)
        );
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "# ABC-12 Slow login_page\n"
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
    }

    #[test]
    fn a_new_carnet_is_named_after_its_key_then_its_group() {
        let state = state();
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let today = state.today().unwrap();
        let make = |name: &str, group: &str| {
            let path = create(&state, &fake, &names(), root.path(), name, "w", group).unwrap();
            let item = state.require_item(&path).unwrap();
            let name = dir_name(&path);
            (
                name.strip_prefix(&format!("{today}-")).unwrap().to_owned(),
                item.group,
            )
        };
        assert_eq!(
            make("DEF-3 logs", "GH-1"),
            ("DEF-3-logs".into(), "DEF-3".into())
        );
        assert_eq!(
            make("Login timeout", "GH-1"),
            ("GH-1-login-timeout".into(), "GH-1".into())
        );
        assert_eq!(make("crash", "web#12"), ("crash".into(), "web#12".into()));
        assert_eq!(
            make("notes about DEF-4", ""),
            ("notes-about-def-4".into(), "".into())
        );
        assert_eq!(make("XYZ-9", ""), ("XYZ-9".into(), "XYZ-9".into()));
    }

    #[test]
    fn a_carnet_name_must_be_a_plain_new_name() {
        let state = state();
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let make = |name: &str| create(&state, &fake, &names(), root.path(), name, "w", "");
        assert!(make("  ").unwrap_err().to_string().contains("empty"));
        assert!(make(" _ - ").unwrap_err().to_string().contains("empty"));
        assert!(make("a/b").unwrap_err().to_string().contains("/"));
        make("notes").unwrap();
        assert!(make("notes").unwrap_err().to_string().contains("exists"));
        assert!(
            create(&state, &fake, &names(), root.path(), "x", "nope", "").is_err(),
            "an unknown workspace"
        );
    }

    #[test]
    fn a_failed_creation_leaves_nothing_behind() {
        let state = state();
        let root = tempfile::tempdir().unwrap();
        let failing = Fake::default().always("git", None);
        assert!(create(&state, &failing, &names(), root.path(), "notes", "w", "").is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert_eq!(state.carnets().unwrap(), []);
        create(
            &state,
            &Fake::default(),
            &names(),
            root.path(),
            "notes",
            "w",
            "",
        )
        .unwrap();
    }

    #[test]
    fn a_carnets_group_is_the_key_right_after_its_date() {
        let names = names();
        assert_eq!(names.group("2026-01-02-ORD-7-crash"), "ORD-7");
        assert_eq!(names.group("2026-01-02-ORD-7"), "ORD-7");
        assert_eq!(names.group("2026-01-02-crash-ORD-7"), "");
        assert_eq!(names.group("2026-01-02-ORD-7x"), "");
        assert_eq!(names.group("ORD-7-crash"), "");
    }

    #[test]
    fn adding_records_a_dated_repo_under_the_root_once() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Data");
        let folder = repo(&root, "2026-01-02-ORD-7-crash");
        let path = add(&state, &names(), &root, &folder, "w").unwrap();
        assert_eq!(path, folder.canonicalize().unwrap());
        let item = state.require_item(&path).unwrap();
        assert_eq!(
            (item.group.as_str(), item.workspace.as_str()),
            ("ORD-7", "w")
        );
        let error = |path: &Path| {
            add(&state, &names(), &root, path, "w")
                .unwrap_err()
                .to_string()
        };
        assert!(error(&folder).contains("already"));
        let registered = repo(&root, "2026-01-06-registered");
        state
            .add_repo(registered.canonicalize().unwrap(), None, "w")
            .unwrap();
        assert!(error(&registered).contains("registered repo"));
        assert!(add(&state, &names(), &root, &root.join("missing"), "w").is_err());
        let file = root.join("file");
        std::fs::write(&file, "").unwrap();
        assert!(error(&file).contains("not a directory"));
        let plain = root.join("2026-01-03-plain");
        std::fs::create_dir(&plain).unwrap();
        assert!(error(&plain).contains("not a git repo"));
        assert!(error(&repo(&root, "crash")).contains("YYYY-MM-DD-"));
        assert!(error(&repo(dir.path(), "2026-01-04-out")).contains("not directly under"));
        assert!(error(&repo(&root, "2026-01-05-a/2026-01-05-b")).contains("not directly under"));
    }
}
