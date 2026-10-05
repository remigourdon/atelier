//! Carnets: investigation folders `<root>/YYYY-MM-DD-<name>`, each its own git repo, recorded by
//! the front matter of their README.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};
use regex::Regex;
use toml_edit::{Array, DocumentMut, value};

use crate::git;
use crate::process::Runner;
use crate::state::{ItemKind, State, dir_name};

/// The line that opens and closes a README's front matter.
const FENCE: &str = "+++";

/// How carnet names carry a ticket key: right after the date in a folder name, and at the
/// start of a typed name.
#[derive(Debug, Clone)]
pub struct Names {
    dated: Regex,
    folder: Regex,
    typed: Regex,
    key: Regex,
    github: Regex,
}

impl Names {
    pub fn new(ticket_pattern: &str) -> Result<Self> {
        let key = format!("((?:{ticket_pattern}))");
        Ok(Self {
            dated: Regex::new(r"^(\d{4}-\d{2}-\d{2})-(.+)$")?,
            folder: Regex::new(&format!(r"^\d{{4}}-\d{{2}}-\d{{2}}-{key}(?:-|$)"))?,
            typed: Regex::new(&format!(r"^{key}(?:[\s_-]+|$)"))?,
            key: Regex::new(&format!("^{key}$"))?,
            github: Regex::new(r"^(?:[\w.-]+/)?[\w.-]+#[1-9][0-9]*$")?,
        })
    }

    /// The group a carnet's folder name gives: the ticket key right after its date, or `""`.
    pub fn group(&self, folder: &str) -> String {
        (self.folder.captures(folder)).map_or_else(String::new, |captures| captures[1].to_owned())
    }

    /// Whether `group` is a ticket key or a GitHub issue key (`repo#12`, `owner/repo#12`),
    /// rather than a label set by hand.
    fn is_ticket(&self, group: &str) -> bool {
        self.key.is_match(group) || self.github.is_match(group)
    }

    /// The folder name, after the date, of a carnet named `name`, and the key typed at its
    /// start, which the folder name keeps.
    fn slug(&self, name: &str) -> (String, Option<String>) {
        let (key, rest) = match self.typed.captures(name) {
            Some(captures) => (Some(captures[1].to_owned()), &name[captures[0].len()..]),
            None => (None, name),
        };
        let words = rest
            .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase);
        let slug = (key.iter().cloned())
            .chain(words)
            .collect::<Vec<_>>()
            .join("-");
        (slug, key)
    }
}

/// A carnet as its folder records it.
#[derive(Debug, Clone, PartialEq)]
pub struct Carnet {
    pub path: PathBuf,
    /// The folder name after its date.
    pub name: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Its first ticket is its group.
    pub tickets: Vec<String>,
    pub closed: bool,
    pub summary: String,
    /// The README as read, `None` when it has none.
    pub readme: Option<String>,
}

impl Carnet {
    /// Its group: its first ticket, or `""`.
    pub fn group(&self) -> &str {
        self.tickets.first().map_or("", String::as_str)
    }
}

/// Every carnet directly under `root`: a directory named `YYYY-MM-DD-…` with a `.git`, newest
/// first. A missing root holds none.
pub fn scan(root: &Path, names: &Names) -> Result<Vec<Carnet>> {
    let Ok(root) = root.canonicalize() else {
        return Ok(Vec::new());
    };
    let mut carnets = Vec::new();
    for entry in std::fs::read_dir(&root).wrap_err_with(|| format!("{}", root.display()))? {
        let path = entry?.path();
        let folder = dir_name(&path);
        let Some(captures) = names.dated.captures(&folder) else {
            continue;
        };
        if !path.is_dir() || !path.join(".git").exists() {
            continue;
        }
        let readme = std::fs::read_to_string(path.join("README.md")).ok();
        let front = Front::parse(readme.as_deref().unwrap_or("")).unwrap_or_default();
        carnets.push(Carnet {
            tickets: front.tickets().unwrap_or_else(|| fallback(names, &folder)),
            closed: front.closed(),
            summary: front.summary(),
            date: captures[1].to_owned(),
            name: captures[2].to_owned(),
            readme,
            path,
        });
    }
    carnets.sort_by(|a, b| b.path.cmp(&a.path));
    Ok(carnets)
}

/// The tickets of a carnet whose front matter names none: the key after its date, if any.
fn fallback(names: &Names, folder: &str) -> Vec<String> {
    Some(names.group(folder))
        .filter(|key| !key.is_empty())
        .into_iter()
        .collect()
}

/// The newest open carnet listing `key` among its tickets.
pub fn newest_open<'a>(carnets: &'a [Carnet], key: &str) -> Option<&'a Carnet> {
    (carnets.iter())
        .filter(|carnet| !carnet.closed && carnet.tickets.iter().any(|ticket| ticket == key))
        .max_by(|a, b| a.path.cmp(&b.path))
}

/// A README's front matter and the rest of it.
#[derive(Debug, Default)]
struct Front {
    doc: DocumentMut,
    body: String,
    /// Whether the README had front matter.
    fenced: bool,
}

impl Front {
    /// A README that does not begin with a `+++` line, or never closes it, has none.
    fn parse(readme: &str) -> Result<Self> {
        let Some((toml, body)) = split(readme) else {
            return Ok(Self {
                body: readme.to_owned(),
                ..Self::default()
            });
        };
        Ok(Self {
            doc: toml.parse().wrap_err("reading the README's front matter")?,
            body: body.to_owned(),
            fenced: true,
        })
    }

    fn tickets(&self) -> Option<Vec<String>> {
        let tickets = self.doc.get("tickets")?.as_array()?;
        Some(
            tickets
                .iter()
                .filter_map(|ticket| ticket.as_str())
                .map(Into::into)
                .collect(),
        )
    }

    fn set_tickets(&mut self, tickets: &[String]) {
        self.doc["tickets"] = value(tickets.iter().collect::<Array>());
    }

    fn closed(&self) -> bool {
        (self.doc.get("closed")).and_then(|closed| closed.as_bool()) == Some(true)
    }

    fn summary(&self) -> String {
        (self.doc.get("summary"))
            .and_then(|summary| summary.as_str())
            .unwrap_or_default()
            .to_owned()
    }

    fn render(&self) -> String {
        let mut toml = self.doc.to_string();
        if !toml.is_empty() && !toml.ends_with('\n') {
            toml.push('\n');
        }
        // A block added to a README sits apart from its text.
        let gap = if self.fenced || self.body.is_empty() {
            ""
        } else {
            "\n"
        };
        format!("{FENCE}\n{toml}{FENCE}\n{gap}{}", self.body)
    }
}

/// The TOML between a README's opening `+++` line and the next one, and the text after it.
fn split(readme: &str) -> Option<(&str, &str)> {
    let mut lines = readme.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end() != FENCE {
        return None;
    }
    let start = first.len();
    let mut end = start;
    for line in lines {
        if line.trim_end() == FENCE {
            return Some((&readme[start..end], &readme[end + line.len()..]));
        }
        end += line.len();
    }
    None
}

/// A README without its front matter, as it renders.
pub fn body(readme: &str) -> &str {
    split(readme).map_or(readme, |(_, body)| body)
}

/// Edits a carnet's front matter with `edit`, which says how to describe the change, or that
/// there is none. Writes the README and commits only it.
fn edit(
    runner: &dyn Runner,
    path: &Path,
    edit: impl FnOnce(&mut Front) -> Option<String>,
) -> Result<()> {
    let readme_path = path.join("README.md");
    let readme = std::fs::read_to_string(&readme_path).unwrap_or_default();
    let mut front = Front::parse(&readme)?;
    let Some(message) = edit(&mut front) else {
        return Ok(());
    };
    std::fs::write(&readme_path, front.render())?;
    commit(runner, path, &message)
}

fn commit(runner: &dyn Runner, path: &Path, message: &str) -> Result<()> {
    git::add(runner, path, "README.md")?;
    git::commit(runner, path, message, "README.md")
}

/// Sets a carnet's first ticket, keeping the others; an empty `key` removes the first one.
/// Returns its tickets.
pub fn set_first_ticket(
    runner: &dyn Runner,
    names: &Names,
    path: &Path,
    key: &str,
) -> Result<Vec<String>> {
    let mut tickets = Vec::new();
    edit(runner, path, |front| {
        tickets = front
            .tickets()
            .unwrap_or_else(|| fallback(names, &dir_name(path)));
        let message = if key.is_empty() {
            if tickets.is_empty() {
                return None;
            }
            format!("Unlink {}", tickets.remove(0))
        } else {
            if tickets.first().map(String::as_str) == Some(key) {
                return None;
            }
            tickets.retain(|ticket| ticket != key);
            match tickets.first_mut() {
                Some(first) => *first = key.to_owned(),
                None => tickets.push(key.to_owned()),
            }
            format!("Link {key}")
        };
        front.set_tickets(&tickets);
        Some(message)
    })?;
    Ok(tickets)
}

pub fn set_closed(runner: &dyn Runner, path: &Path, closed: bool) -> Result<()> {
    edit(runner, path, |front| {
        if front.closed() == closed {
            return None;
        }
        front.doc["closed"] = value(closed);
        Some(if closed { "Close" } else { "Reopen" }.into())
    })
}

/// Creates `<root>/<today>-<name in kebab case>`, keeping a key typed at the start of `name`,
/// with a README titled `name` whose ticket is that key, else `group` when it is a ticket key.
/// Makes it a git repo, commits the README, and records the carnet in `workspace`. Returns its
/// path.
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
    let (slug, key) = names.slug(name);
    if slug.is_empty() {
        bail!("a carnet name must not be empty");
    }
    state.require_workspace(workspace)?;
    let path = root.join(format!("{}-{slug}", state.today()?));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let tickets: Vec<String> = key
        .or_else(|| Some(group.to_owned()).filter(|group| names.is_ticket(group)))
        .into_iter()
        .collect();
    std::fs::create_dir_all(&path)?;
    let path = path.canonicalize()?;
    let made = (|| {
        let mut front = Front {
            body: format!("# {name}\n"),
            ..Front::default()
        };
        front.set_tickets(&tickets);
        front.doc["summary"] = value("");
        std::fs::write(path.join("README.md"), front.render())?;
        git::init(runner, &path)?;
        commit(runner, &path, "Create carnet")?;
        let group = tickets.first().map_or("", String::as_str);
        state.add_item(&path, ItemKind::Carnet, None, group, workspace)
    })();
    // A half-made folder would block retrying under the same name.
    if let Err(err) = made {
        let _ = std::fs::remove_dir_all(&path);
        return Err(err);
    }
    Ok(path)
}

/// The arguments of `rg` searching every carnet under `root` for `text`.
pub fn search_args<'a>(text: &'a str, root: &'a str) -> [&'a str; 6] {
    [
        "--line-number",
        "--ignore-case",
        "--fixed-strings",
        "--",
        text,
        root,
    ]
}

pub const NO_RIPGREP: &str = "carnet search needs ripgrep (rg) on PATH";

/// Whether `program` is an executable file in a `PATH` directory.
pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// Searches every carnet under `root` for `text` with `rg`, its output on the terminal.
pub fn search(runner: &dyn Runner, root: &Path, text: &str) -> Result<()> {
    if !on_path("rg") {
        bail!(NO_RIPGREP);
    }
    let root = root.to_string_lossy();
    runner.interactive("rg", &search_args(text, &root))
}

#[cfg(test)]
pub mod tests {
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

    pub fn names() -> Names {
        Names::new(Config::default().ticket_pattern()).unwrap()
    }

    /// A git repo `<dir>/<name>` with `readme`, as `git init` leaves it.
    pub fn repo(dir: &Path, name: &str, readme: Option<&str>) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        if let Some(readme) = readme {
            std::fs::write(path.join("README.md"), readme).unwrap();
        }
        path.canonicalize().unwrap()
    }

    fn commits(path: &Path, message: &str) -> [String; 2] {
        let path = path.display();
        [
            format!("git -C {path} add README.md"),
            format!("git -C {path} commit -m {message} -- README.md"),
        ]
    }

    #[test]
    fn new_carnets_are_dated_git_repos_with_a_committed_readme() {
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
            "+++\ntickets = [\"ABC-12\"]\nsummary = \"\"\n+++\n\n# ABC-12 Slow login_page\n"
        );
        let mut calls = vec![format!("git -C {} init --quiet", path.display())];
        calls.extend(commits(&path, "Create carnet"));
        assert_eq!(fake.calls(), calls);
        let item = state.require_item(&path).unwrap();
        assert_eq!(
            (
                item.repo.as_deref(),
                item.group.as_str(),
                item.workspace.as_str()
            ),
            (None, "ABC-12", "w")
        );
    }

    #[test]
    fn a_new_carnet_keeps_a_typed_key_else_takes_a_ticket_group() {
        let state = state();
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let today = state.today().unwrap();
        let make = |name: &str, group: &str| {
            let path = create(&state, &fake, &names(), root.path(), name, "w", group).unwrap();
            let readme = std::fs::read_to_string(path.join("README.md")).unwrap();
            let tickets = Front::parse(&readme).unwrap().tickets().unwrap();
            let name = dir_name(&path);
            let name = name.strip_prefix(&format!("{today}-")).unwrap().to_owned();
            let group = tickets.first().cloned().unwrap_or_default();
            assert_eq!(state.require_item(&path).unwrap().group, group);
            (name, tickets)
        };
        let tickets = |keys: &[&str]| keys.iter().map(|key| key.to_string()).collect::<Vec<_>>();
        assert_eq!(
            make("DEF-3 logs", "GH-1"),
            ("DEF-3-logs".into(), tickets(&["DEF-3"]))
        );
        assert_eq!(
            make("Login timeout", "GH-1"),
            ("login-timeout".into(), tickets(&["GH-1"]))
        );
        assert_eq!(
            make("crash", "web#12"),
            ("crash".into(), tickets(&["web#12"]))
        );
        assert_eq!(
            make("leak", "o/web#3"),
            ("leak".into(), tickets(&["o/web#3"]))
        );
        assert_eq!(make("perf", "slow pages"), ("perf".into(), tickets(&[])));
        assert_eq!(
            make("notes about DEF-4", ""),
            ("notes-about-def-4".into(), tickets(&[]))
        );
        assert_eq!(make("XYZ-9", ""), ("XYZ-9".into(), tickets(&["XYZ-9"])));
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
    fn a_carnets_fallback_ticket_is_the_key_right_after_its_date() {
        let names = names();
        assert_eq!(names.group("2026-01-02-ORD-7-crash"), "ORD-7");
        assert_eq!(names.group("2026-01-02-ORD-7"), "ORD-7");
        assert_eq!(names.group("2026-01-02-crash-ORD-7"), "");
        assert_eq!(names.group("2026-01-02-ORD-7x"), "");
        assert_eq!(names.group("ORD-7-crash"), "");
    }

    #[test]
    fn scan_finds_dated_git_folders_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let front =
            "+++\ntickets = [\"ABC-1\", \"web#2\"]\nclosed = true\nsummary = \"Slow\"\n+++\n# x\n";
        repo(root, "2026-01-02-ORD-7-crash", None);
        repo(root, "2026-03-04-notes", Some(front));
        repo(root, "2026-02-01-ORD-8-plain", Some("# no front matter\n"));
        repo(root, "undated", None);
        std::fs::create_dir(root.join("2026-05-05-not-git")).unwrap();
        std::fs::write(root.join("2026-06-06-file"), "").unwrap();
        let carnets = scan(root, &names()).unwrap();
        let found: Vec<_> = (carnets.iter())
            .map(|carnet| {
                (
                    carnet.date.as_str(),
                    carnet.name.as_str(),
                    carnet.tickets.clone(),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                ("2026-03-04", "notes", vec!["ABC-1".into(), "web#2".into()]),
                ("2026-02-01", "ORD-8-plain", vec!["ORD-8".into()]),
                ("2026-01-02", "ORD-7-crash", vec!["ORD-7".into()]),
            ]
        );
        assert!(carnets[0].closed && carnets[0].summary == "Slow");
        assert!(!carnets[1].closed && carnets[1].summary.is_empty());
        assert_eq!(carnets[1].readme.as_deref(), Some("# no front matter\n"));
        assert_eq!(carnets[2].readme, None);
        assert_eq!(scan(&root.join("missing"), &names()).unwrap(), []);
    }

    #[test]
    fn an_empty_tickets_key_overrides_the_folder_key() {
        let dir = tempfile::tempdir().unwrap();
        repo(
            dir.path(),
            "2026-01-02-ORD-7-crash",
            Some("+++\ntickets = []\n+++\n"),
        );
        assert_eq!(
            scan(dir.path(), &names()).unwrap()[0].tickets,
            Vec::<String>::new()
        );
    }

    #[test]
    fn front_matter_edits_keep_other_keys_comments_and_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\n# why\nowner = \"me\" # mine\ntickets = [\"A-1\", \"B-2\"]\n+++\n# Title\n\n+++ not a fence\n";
        let path = repo(dir.path(), "2026-01-02-x", Some(readme));
        let fake = Fake::default();
        let tickets = set_first_ticket(&fake, &names(), &path, "C-3").unwrap();
        assert_eq!(tickets, ["C-3", "B-2"]);
        let written = std::fs::read_to_string(path.join("README.md")).unwrap();
        assert_eq!(
            written,
            "+++\n# why\nowner = \"me\" # mine\ntickets = [\"C-3\", \"B-2\"]\n+++\n# Title\n\n+++ not a fence\n"
        );
        assert_eq!(fake.calls(), commits(&path, "Link C-3"));
        set_closed(&fake, &path, true).unwrap();
        let written = std::fs::read_to_string(path.join("README.md")).unwrap();
        assert!(
            written.contains("closed = true\n+++\n# Title\n"),
            "{written}"
        );
        assert_eq!(fake.calls()[2..], commits(&path, "Close"));
        set_closed(&fake, &path, true).unwrap();
        set_first_ticket(&fake, &names(), &path, "C-3").unwrap();
        assert_eq!(fake.calls().len(), 4, "no change, no commit");
        set_closed(&fake, &path, false).unwrap();
        assert_eq!(fake.calls()[4..], commits(&path, "Reopen"));
        let tickets = set_first_ticket(&fake, &names(), &path, "").unwrap();
        assert_eq!(tickets, ["B-2"]);
        assert_eq!(fake.calls()[6..], commits(&path, "Unlink C-3"));
    }

    #[test]
    fn a_readme_without_front_matter_gets_one_on_its_first_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo(dir.path(), "2026-01-02-ORD-7-crash", Some("# Crash\n"));
        let fake = Fake::default();
        assert_eq!(
            set_first_ticket(&fake, &names(), &path, "web#4").unwrap(),
            ["web#4"]
        );
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "+++\ntickets = [\"web#4\"]\n+++\n\n# Crash\n",
            "the folder's key was its one ticket"
        );
        let bare = repo(dir.path(), "2026-01-03-bare", None);
        set_closed(&fake, &bare, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(bare.join("README.md")).unwrap(),
            "+++\nclosed = true\n+++\n"
        );
        let broken = repo(
            dir.path(),
            "2026-01-04-broken",
            Some("+++\nnot toml\n+++\n"),
        );
        assert!(
            set_closed(&fake, &broken, true).is_err(),
            "never overwritten"
        );
    }

    #[test]
    fn the_body_is_the_readme_after_its_front_matter() {
        assert_eq!(body("+++\na = 1\n+++\n# T\n"), "# T\n");
        assert_eq!(body("# T\n+++\n"), "# T\n+++\n");
        assert_eq!(body("+++\nunclosed\n"), "+++\nunclosed\n");
    }

    #[test]
    fn the_newest_open_carnet_of_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        repo(root, "2026-01-01-ORD-7-a", None);
        repo(root, "2026-01-02-ORD-7-b", None);
        repo(
            root,
            "2026-01-03-ORD-7-c",
            Some("+++\nclosed = true\n+++\n"),
        );
        let carnets = scan(root, &names()).unwrap();
        let found = newest_open(&carnets, "ORD-7").unwrap();
        assert_eq!(found.name, "ORD-7-b");
        assert!(newest_open(&carnets, "ORD-8").is_none());
    }
}
