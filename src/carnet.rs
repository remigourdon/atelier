//! Carnets: investigation folders `<root>/YYYY-MM-DD-<name>`, each its own git repo, recorded by
//! the front matter of their README.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use regex::Regex;
use toml_edit::{Array, DocumentMut, value};

use crate::config::Config;
use crate::git;
use crate::issues::TrackerConfig;
use crate::links::{Group, IssueKeys, Links, group_text};
use crate::process::{Runner, exited_with};
use crate::state::dir_name;

/// The line that opens and closes a README's front matter.
const FENCE: &str = "+++";

/// A carnet's folder name: its date, then its slug.
static DATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})-(.+)$").unwrap());

/// A carnet's folder name split into its date, `YYYY-MM-DD`, and the rest; `None` when it is
/// not dated.
pub fn dated(folder: &str) -> Option<(&str, &str)> {
    let captures = DATED.captures(folder)?;
    Some((captures.get(1)?.as_str(), captures.get(2)?.as_str()))
}

/// The folder name, after the date, of a carnet named `name`: its letters and digits,
/// lowercased, each run joined by `-`.
pub fn slug(name: &str) -> String {
    (name.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// A carnet as its folder records it.
#[derive(Debug, Clone, PartialEq)]
pub struct Carnet {
    pub path: PathBuf,
    /// The folder name after its date.
    pub name: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub links: Links,
    pub closed: bool,
    pub summary: String,
    /// Its README's stamp, `None` when it has none.
    pub readme: Option<Stamp>,
}

/// A README's size and modification time, which change when it is written, so a copy read
/// earlier can be told stale without keeping the text to compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl Stamp {
    /// The stamp of the README in `carnet`, `None` when it has none.
    pub fn of(carnet: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(carnet.join("README.md")).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// The start of a README up to the end of its front matter, else its first line, so a long
/// README costs a few lines to scan. Empty when it cannot be read.
fn read_front(readme: &Path) -> String {
    let Ok(file) = std::fs::File::open(readme) else {
        return String::new();
    };
    let mut reader = BufReader::new(file);
    let mut front = String::new();
    loop {
        let start = front.len();
        if !matches!(reader.read_line(&mut front), Ok(read) if read > 0) {
            break;
        }
        let fence = front[start..].trim_end() == FENCE;
        if fence != (start == 0) {
            break;
        }
    }
    front
}

/// Every carnet directly under `root`, newest first, its front matter normalised. A missing
/// root holds none.
fn scan(root: &Path, tracker: &TrackerConfig) -> Result<Vec<Carnet>> {
    let Ok(root) = root.canonicalize() else {
        return Ok(Vec::new());
    };
    let mut carnets = Vec::new();
    for entry in std::fs::read_dir(&root).wrap_err_with(|| format!("{}", root.display()))? {
        carnets.extend(read(entry?.path(), tracker));
    }
    carnets.sort_by(|a, b| b.path.cmp(&a.path));
    Ok(carnets)
}

/// The carnet at `path`: a directory named `YYYY-MM-DD-…` with a `.git`, its front matter
/// normalised; `None` for anything else.
fn read(path: PathBuf, tracker: &TrackerConfig) -> Option<Carnet> {
    let folder = dir_name(&path);
    let (date, name) = dated(&folder)?;
    if !path.is_dir() || !path.join(".git").exists() {
        return None;
    }
    let front = Front::parse(&read_front(&path.join("README.md"))).unwrap_or_default();
    Some(Carnet {
        links: Links {
            group: front.group(),
            issue_keys: front.issues(tracker),
        },
        closed: front.closed(),
        summary: front.summary(),
        date: date.to_owned(),
        name: name.to_owned(),
        readme: Stamp::of(&path),
        path,
    })
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

    /// Its `group`, normalised.
    fn group(&self) -> Option<Group> {
        Group::parse((self.doc.get("group")).and_then(|group| group.as_str())?)
    }

    fn set_group(&mut self, group: Option<&Group>) {
        self.doc["group"] = value(group_text(group));
    }

    /// Its `issues`, each short GitHub key resolved against `tracker`.
    fn issues(&self, tracker: &TrackerConfig) -> IssueKeys {
        let issues = self.doc.get("issues").and_then(|issues| issues.as_array());
        let keys = issues.into_iter().flatten().filter_map(|key| key.as_str());
        IssueKeys::resolve(keys, tracker)
    }

    fn set_issues(&mut self, keys: &IssueKeys) {
        self.doc["issues"] = value(keys.iter().map(|key| key.as_str()).collect::<Array>());
    }

    fn set_summary(&mut self, summary: &str) {
        self.doc["summary"] = value(summary);
    }

    /// Writes every key in its normal form, in order: `group`, `issues`, `summary` and
    /// `closed`, each as read, the absent ones empty.
    fn normalise(&mut self, tracker: &TrackerConfig) {
        let (group, keys) = (self.group(), self.issues(tracker));
        let (summary, closed) = (self.summary(), self.closed());
        self.set_group(group.as_ref());
        self.set_issues(&keys);
        self.set_summary(&summary);
        self.doc["closed"] = value(closed);
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

/// How a commit names the change from `old` to `new` links and whether the summary changed
/// (`Set group A, link B-2, unlink C-3, set summary`), or `None` when nothing did.
fn describe(old: &Links, new: &Links, summary_changed: bool) -> Option<String> {
    let mut parts = Vec::new();
    if old.group != new.group {
        parts.push(match &new.group {
            Some(group) => format!("set group {group}"),
            None => "ungroup".into(),
        });
    }
    let added: IssueKeys = (new.issue_keys.iter())
        .filter(|key| !old.links(key))
        .cloned()
        .collect();
    let removed: IssueKeys = (old.issue_keys.iter())
        .filter(|key| !new.links(key))
        .cloned()
        .collect();
    if !added.is_empty() {
        parts.push(format!("link {}", added.join(", ")));
    }
    if !removed.is_empty() {
        parts.push(format!("unlink {}", removed.join(", ")));
    }
    if added.is_empty() && removed.is_empty() && old.issue_keys != new.issue_keys {
        parts.push(format!("reorder {}", new.issue_keys.join(", ")));
    }
    if summary_changed {
        parts.push("set summary".into());
    }
    let message = parts.join(", ");
    let mut chars = message.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// A README without its front matter, as it renders.
pub fn body(readme: &str) -> &str {
    split(readme).map_or(readme, |(_, body)| body)
}

fn commit(runner: &dyn Runner, path: &Path, message: &str) -> Result<()> {
    git::add(runner, path, "README.md")?;
    git::commit(runner, path, message, "README.md")
}

/// The carnets under the configured root, their issue keys resolved against the configured
/// trackers.
pub struct Carnets<'a> {
    /// `None` while carnets are disabled.
    root: Option<PathBuf>,
    tracker: &'a TrackerConfig,
}

impl<'a> Carnets<'a> {
    pub fn new(config: &'a Config) -> Result<Self> {
        Ok(Self {
            root: config.carnet_root(),
            tracker: &config.tracker,
        })
    }

    /// Where carnets live, or why there are none.
    pub fn root(&self) -> Result<&Path> {
        (self.root.as_deref())
            .ok_or_else(|| eyre!("carnets are disabled: set `root` under [carnets] in the config"))
    }

    /// Every carnet, newest first; none while carnets are disabled.
    pub fn scan(&self) -> Result<Vec<Carnet>> {
        match &self.root {
            Some(root) => scan(root, self.tracker),
            None => Ok(Vec::new()),
        }
    }

    /// The carnet at `path` as its folder records it, `None` when there is none there. Read
    /// whether or not carnets are enabled, as a recorded carnet is.
    pub fn read(&self, path: &Path) -> Option<Carnet> {
        read(path.to_owned(), self.tracker)
    }

    /// Edits a carnet's front matter, in its normal form, with `edit`, which says how to
    /// describe the change, or that there is none. Writes the README and commits only it.
    fn edit(
        &self,
        runner: &dyn Runner,
        path: &Path,
        edit: impl FnOnce(&mut Front) -> Option<String>,
    ) -> Result<()> {
        let readme_path = path.join("README.md");
        let readme = std::fs::read_to_string(&readme_path).unwrap_or_default();
        let mut front = Front::parse(&readme)?;
        front.normalise(self.tracker);
        let Some(message) = edit(&mut front) else {
            return Ok(());
        };
        std::fs::write(&readme_path, front.render())?;
        commit(runner, path, &message)
    }

    /// Edits a carnet's group and issue keys, as its front matter records them, with `edit`, in
    /// one commit naming what changed. Returns its links once edited.
    pub fn relink(
        &self,
        runner: &dyn Runner,
        path: &Path,
        edit: impl FnOnce(&mut Links),
    ) -> Result<Links> {
        let amended = self.amend(runner, path, |links, _| edit(links))?;
        Ok(amended.0)
    }

    /// Edits a carnet's group, issue keys and summary, as its front matter records them, with
    /// `edit`, in one commit naming what changed. Returns them once edited.
    pub fn amend(
        &self,
        runner: &dyn Runner,
        path: &Path,
        edit: impl FnOnce(&mut Links, &mut String),
    ) -> Result<(Links, String)> {
        let mut edited = None;
        self.edit(runner, path, |front| {
            let old = Links {
                group: front.group(),
                issue_keys: front.issues(self.tracker),
            };
            let (mut new, mut summary) = (old.clone(), front.summary());
            edit(&mut new, &mut summary);
            let summary = summary.trim().to_owned();
            let message = describe(&old, &new, summary != front.summary());
            front.set_group(new.group.as_ref());
            front.set_issues(&new.issue_keys);
            front.set_summary(&summary);
            edited = Some((new, summary));
            message
        })?;
        edited.ok_or_else(|| eyre!("{} was not edited", path.display()))
    }

    pub fn set_closed(&self, runner: &dyn Runner, path: &Path, closed: bool) -> Result<()> {
        self.edit(runner, path, |front| {
            if front.closed() == closed {
                return None;
            }
            front.doc["closed"] = value(closed);
            Some(if closed { "Close" } else { "Reopen" }.into())
        })
    }

    /// Creates `<root>/<date>-<name in kebab case>` with a README of only front matter, holding
    /// `links` and `summary`, makes it a git repo and commits the README. Returns it as its
    /// folder now records it. Recording it is the caller's.
    pub fn create(
        &self,
        runner: &dyn Runner,
        date: &str,
        name: &str,
        links: &Links,
        summary: &str,
    ) -> Result<Carnet> {
        let root = self.root()?;
        let slug = slug(name);
        if slug.is_empty() {
            bail!("a carnet name needs a letter or a digit");
        }
        let path = root.join(format!("{date}-{slug}"));
        if path.exists() {
            bail!("{} already exists", path.display());
        }
        let summary = summary.trim();
        std::fs::create_dir_all(&path)?;
        let path = path.canonicalize()?;
        let made = (|| {
            let mut front = Front::default();
            front.normalise(self.tracker);
            front.set_group(links.group.as_ref());
            front.set_issues(&links.issue_keys);
            front.set_summary(summary);
            std::fs::write(path.join("README.md"), front.render())?;
            git::init(runner, &path)?;
            commit(runner, &path, "Create carnet")
        })();
        // A half-made folder would block retrying under the same name.
        if let Err(err) = made {
            let _ = std::fs::remove_dir_all(&path);
            return Err(err);
        }
        Ok(Carnet {
            name: slug,
            date: date.to_owned(),
            links: links.clone(),
            closed: false,
            summary: summary.to_owned(),
            readme: Stamp::of(&path),
            path,
        })
    }

    /// Searches every carnet for `text` with `rg`, its output on the terminal.
    pub fn search(&self, runner: &dyn Runner, text: &str) -> Result<()> {
        let root = self.root()?;
        if !on_path("rg") {
            bail!(NO_RIPGREP);
        }
        let root = root.to_string_lossy();
        match runner.interactive("rg", &search_args(text, &root)) {
            // `rg` exits 1 when nothing matches.
            Err(err) if exited_with(&err, 1) => {
                eprintln!("no carnet mentions {text}");
                Ok(())
            }
            result => result,
        }
    }

    /// Searches every carnet for `text` with `rg`, as the TUI does: each carnet with hits, and
    /// its hit lines as `<file>:<line>:<text>`.
    pub fn hits(&self, runner: &dyn Runner, text: &str) -> Result<BTreeMap<PathBuf, Vec<String>>> {
        let root = self.root()?;
        if !on_path("rg") {
            bail!(NO_RIPGREP);
        }
        // Canonical, as the scan lists carnets.
        let root = root
            .canonicalize()
            .wrap_err_with(|| format!("{}", root.display()))?;
        let root_arg = root.to_string_lossy();
        let mut args = vec!["--no-heading"];
        args.extend(search_args(text, &root_arg));
        let output = match runner.output("rg", &args) {
            Ok(output) => output,
            // `rg` exits 1 when nothing matches.
            Err(err) if exited_with(&err, 1) => String::new(),
            Err(err) => return Err(err),
        };
        Ok(group_hits(&root, &output))
    }
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

/// `rg --no-heading` output grouped by the carnet folder each hit is in.
fn group_hits(root: &Path, output: &str) -> BTreeMap<PathBuf, Vec<String>> {
    let prefix = format!("{}/", root.display());
    let mut hits: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for line in output.lines() {
        let Some((folder, hit)) =
            (line.strip_prefix(&prefix)).and_then(|rest| rest.split_once('/'))
        else {
            continue;
        };
        hits.entry(root.join(folder))
            .or_default()
            .push(hit.to_owned());
    }
    hits
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::links::IssueKey;
    use crate::links::tests::{group, keys};
    use crate::process::fake::Fake;

    /// The date new carnets are made on.
    const TODAY: &str = "2026-10-07";

    /// The carnets under `root`, GitHub issues coming from `o/atelier`, `o/web` and `p/web`.
    pub fn carnets(root: &Path) -> Carnets<'static> {
        let text = format!(
            "[carnets]\nroot = {:?}\n[tracker.github]\nrepos = [\"o/atelier\", \"o/web\", \"p/web\"]\n",
            root.display().to_string()
        );
        let config = Box::leak(Box::new(Config::parse(&text).unwrap()));
        Carnets::new(config).unwrap()
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

    fn make(runner: &dyn Runner, root: &Path, name: &str, group: &str) -> Result<PathBuf> {
        let links = Links {
            group: self::group(group),
            ..Links::default()
        };
        let carnet = carnets(root).create(runner, TODAY, name, &links, "")?;
        Ok(carnet.path)
    }

    fn set_group(carnets: &Carnets, runner: &dyn Runner, path: &Path, group: Option<&Group>) {
        let set = |links: &mut Links| links.group = group.cloned();
        carnets.relink(runner, path, set).unwrap();
    }

    /// The front matter written at `path`.
    fn front(path: &Path) -> Front {
        Front::parse(&std::fs::read_to_string(path.join("README.md")).unwrap()).unwrap()
    }

    #[test]
    fn new_carnets_are_dated_git_repos_with_a_committed_readme() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let root_path = root.path().join("Data");
        let name = " ABC-12 Slow login_page ";
        let path = make(&fake, &root_path, name, " login ").unwrap();
        let name = format!("{TODAY}-abc-12-slow-login-page");
        assert_eq!(
            path,
            root.path().canonicalize().unwrap().join("Data").join(&name)
        );
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "+++\ngroup = \"LOGIN\"\nissues = []\nsummary = \"\"\nclosed = false\n+++\n"
        );
        let mut calls = vec![format!("git -C {} init --quiet", path.display())];
        calls.extend(commits(&path, "Create carnet"));
        assert_eq!(fake.calls(), calls);
    }

    #[test]
    fn a_new_carnet_takes_the_group_as_given_and_links_nothing_from_its_name() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let made = |name: &str, group: &str| {
            let path = make(&fake, root.path(), name, group).unwrap();
            let front = front(&path);
            let links = Links {
                group: front.group(),
                issue_keys: front.issues(carnets(root.path()).tracker),
            };
            let name = dir_name(&path);
            let name = name.strip_prefix(&format!("{TODAY}-")).unwrap().to_owned();
            (name, links)
        };
        let links = crate::links::tests::links;
        assert_eq!(
            made("DEF-3 logs", "GH-1"),
            ("def-3-logs".into(), links("GH-1", &[]))
        );
        assert_eq!(
            made("perf", "slow pages"),
            ("perf".into(), links("SLOW PAGES", &[])),
            "any group, never a key"
        );
        assert_eq!(
            made("notes about DEF-4", ""),
            ("notes-about-def-4".into(), links("", &[]))
        );
        assert_eq!(made("XYZ-9", ""), ("xyz-9".into(), links("", &[])));
    }

    #[test]
    fn a_new_carnet_links_the_keys_given_with_its_summary() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let links = crate::links::tests::links("login", &["o/web#2", "DEF-3"]);
        let carnet = (carnets(root.path()))
            .create(&fake, TODAY, "DEF-3 logs", &links, " Slow logs ")
            .unwrap();
        assert_eq!(
            (carnet.date.as_str(), carnet.name.as_str()),
            (TODAY, "def-3-logs")
        );
        let expected = crate::links::tests::links("LOGIN", &["o/web#2", "DEF-3"]);
        assert_eq!(carnet.links, expected);
        assert_eq!(
            (carnet.summary.as_str(), carnet.closed),
            ("Slow logs", false)
        );
        assert_eq!(
            std::fs::read_to_string(carnet.path.join("README.md")).unwrap(),
            "+++\ngroup = \"LOGIN\"\nissues = [\"o/web#2\", \"DEF-3\"]\nsummary = \"Slow logs\"\nclosed = false\n+++\n"
        );
        assert_eq!(carnet.readme, Stamp::of(&carnet.path));
        std::fs::create_dir(carnet.path.join(".git")).unwrap();
        let read = carnets(root.path()).read(&carnet.path);
        assert_eq!(read, Some(carnet), "as its folder now records it");
    }

    #[test]
    fn a_carnet_name_must_be_a_plain_new_name() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let named = |name: &str| make(&fake, root.path(), name, "");
        assert!(named("  ").unwrap_err().to_string().contains("letter"));
        assert!(named(" _ -/: ").unwrap_err().to_string().contains("letter"));
        let path = named("Fix: a/b, again").unwrap();
        assert!(
            dir_name(&path).ends_with("-fix-a-b-again"),
            "punctuation dropped"
        );
        named("notes").unwrap();
        assert!(named("notes").unwrap_err().to_string().contains("exists"));
    }

    #[test]
    fn disabled_carnets_scan_none_and_create_none() {
        let config = Config::parse("").unwrap();
        let carnets = Carnets::new(&config).unwrap();
        assert_eq!(carnets.scan().unwrap(), []);
        let error = carnets.create(&Fake::default(), TODAY, "x", &Links::default(), "");
        let error = error.unwrap_err();
        assert!(error.to_string().contains("[carnets]"), "{error}");
    }

    #[test]
    fn a_failed_creation_leaves_nothing_behind() {
        let root = tempfile::tempdir().unwrap();
        let failing = Fake::default().always("git", None);
        assert!(make(&failing, root.path(), "notes", "").is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        make(&Fake::default(), root.path(), "notes", "").unwrap();
    }

    #[test]
    fn scan_finds_dated_git_folders_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let front = "+++\ngroup = \" login rewrite\"\nissues = [\"ABC-1\", \"o/web#2\"]\nclosed = true\nsummary = \"Slow\"\n+++\n# x\n";
        repo(root, "2026-01-02-ORD-7-crash", None);
        repo(root, "2026-03-04-notes", Some(front));
        repo(root, "2026-02-01-ORD-8-plain", Some("# no front matter\n"));
        repo(root, "undated", None);
        std::fs::create_dir(root.join("2026-05-05-not-git")).unwrap();
        std::fs::write(root.join("2026-06-06-file"), "").unwrap();
        let carnets = self::carnets(root).scan().unwrap();
        let found: Vec<_> = (carnets.iter())
            .map(|carnet| {
                (
                    carnet.date.as_str(),
                    carnet.name.as_str(),
                    carnet.links.clone(),
                )
            })
            .collect();
        let links = crate::links::tests::links;
        assert_eq!(
            found,
            [
                (
                    "2026-03-04",
                    "notes",
                    links("LOGIN REWRITE", &["ABC-1", "o/web#2"])
                ),
                ("2026-02-01", "ORD-8-plain", links("", &[])),
                ("2026-01-02", "ORD-7-crash", links("", &[])),
            ],
            "without front matter, a carnet has no group and no keys, whatever its folder"
        );
        assert!(carnets[0].closed && carnets[0].summary == "Slow");
        assert!(!carnets[1].closed && carnets[1].summary.is_empty());
        assert_eq!(carnets[1].readme.map(|stamp| stamp.len), Some(18));
        assert_eq!(carnets[2].readme, None);
        assert_eq!(self::carnets(&root.join("missing")).scan().unwrap(), []);
        let read = |name: &str| self::carnets(root).read(&root.join(name));
        assert_eq!(read("2026-03-04-notes").as_ref(), carnets.first());
        assert_eq!(read("2026-05-05-not-git"), None);
        assert_eq!(read("undated"), None);
    }

    #[test]
    fn short_github_keys_resolve_against_tracker_repos_only() {
        let dir = tempfile::tempdir().unwrap();
        let front = "+++\nissues = [\"atelier#14\", \"web#3\", \"api#2\", \"ABC-1\"]\n+++\n";
        let path = repo(dir.path(), "2026-01-02-x", Some(front));
        let carnets = carnets(dir.path()).scan().unwrap();
        assert_eq!(
            carnets[0].links.issue_keys,
            keys(&["o/atelier#14", "web#3", "api#2", "ABC-1"]),
            "web is ambiguous, api is no tracker repo"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            front,
            "reading never rewrites"
        );
    }

    #[test]
    fn a_scan_reads_a_readme_up_to_its_front_matter_only() {
        let dir = tempfile::tempdir().unwrap();
        let read = |readme: &str| {
            let path = dir.path().join("README.md");
            std::fs::write(&path, readme).unwrap();
            read_front(&path)
        };
        assert_eq!(read("+++\na = 1\n+++\n# T\nlong\n"), "+++\na = 1\n+++\n");
        assert_eq!(read("# T\nlong\n"), "# T\n", "no front matter");
        assert_eq!(read("+++\nunclosed\n"), "+++\nunclosed\n");
        assert_eq!(read_front(&dir.path().join("missing")), "");
    }

    #[test]
    fn front_matter_edits_keep_other_keys_comments_and_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let readme =
            "+++\n# why\nowner = \"me\" # mine\ngroup = \"A\"\n+++\n# Title\n\n+++ not a fence\n";
        let path = repo(dir.path(), "2026-01-02-x", Some(readme));
        let fake = Fake::default();
        let carnets = carnets(dir.path());
        let login = group("login rewrite");
        set_group(&carnets, &fake, &path, login.as_ref());
        let written = std::fs::read_to_string(path.join("README.md")).unwrap();
        assert_eq!(
            written,
            "+++\n# why\nowner = \"me\" # mine\ngroup = \"LOGIN REWRITE\"\nissues = []\nsummary = \"\"\nclosed = false\n+++\n# Title\n\n+++ not a fence\n"
        );
        assert_eq!(fake.calls(), commits(&path, "Set group LOGIN REWRITE"));
        carnets.set_closed(&fake, &path, true).unwrap();
        let written = std::fs::read_to_string(path.join("README.md")).unwrap();
        assert!(
            written.contains("closed = true\n+++\n# Title\n"),
            "{written}"
        );
        assert_eq!(fake.calls()[2..], commits(&path, "Close"));
        carnets.set_closed(&fake, &path, true).unwrap();
        set_group(&carnets, &fake, &path, login.as_ref());
        assert_eq!(fake.calls().len(), 4, "no change, no commit");
        carnets.set_closed(&fake, &path, false).unwrap();
        assert_eq!(fake.calls()[4..], commits(&path, "Reopen"));
        set_group(&carnets, &fake, &path, None);
        assert_eq!(front(&path).group(), None);
        assert_eq!(fake.calls()[6..], commits(&path, "Ungroup"));
    }

    #[test]
    fn setting_issue_keys_replaces_them_in_one_commit_naming_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ngroup = \"A\"\nissues = [\"ABC-1\", \"atelier#2\"]\n+++\n# T\n";
        let path = repo(dir.path(), "2026-01-02-x", Some(readme));
        let fake = Fake::default();
        let carnets = carnets(dir.path());
        let set = |linked: &[&str]| {
            let set = |links: &mut Links| links.issue_keys = keys(linked);
            carnets.relink(&fake, &path, set).unwrap()
        };
        set(&["o/atelier#2", "ABC-5"]);
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "+++\ngroup = \"A\"\nissues = [\"o/atelier#2\", \"ABC-5\"]\nsummary = \"\"\nclosed = false\n+++\n# T\n"
        );
        assert_eq!(fake.calls(), commits(&path, "Link ABC-5, unlink ABC-1"));
        set(&["o/atelier#2", "ABC-5"]);
        assert_eq!(fake.calls().len(), 2, "no change, no commit");
        set(&["ABC-5", "o/atelier#2"]);
        assert_eq!(
            fake.calls()[2..],
            commits(&path, "Reorder ABC-5, o/atelier#2")
        );
        set(&[]);
        assert_eq!(
            fake.calls()[4..],
            commits(&path, "Unlink ABC-5, o/atelier#2")
        );
        assert_eq!(front(&path).issues(carnets.tracker), keys(&[]));
    }

    #[test]
    fn an_amendment_sets_links_and_summary_in_one_commit_naming_each_change() {
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ngroup = \"A\"\nissues = [\"ABC-1\"]\nsummary = \"Old\"\n+++\n# T\n";
        let path = repo(dir.path(), "2026-01-02-x", Some(readme));
        let fake = Fake::default();
        let carnets = carnets(dir.path());
        let amended = carnets.amend(&fake, &path, |links, summary| {
            links.group = group("login rewrite");
            links.issue_keys = keys(&["ABC-1", "ABC-5"]);
            *summary = "New".into();
        });
        let (links, summary) = amended.unwrap();
        assert_eq!(
            links,
            crate::links::tests::links("LOGIN REWRITE", &["ABC-1", "ABC-5"])
        );
        assert_eq!(summary, "New");
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "+++\ngroup = \"LOGIN REWRITE\"\nissues = [\"ABC-1\", \"ABC-5\"]\nsummary = \"New\"\nclosed = false\n+++\n# T\n"
        );
        assert_eq!(
            fake.calls(),
            commits(&path, "Set group LOGIN REWRITE, link ABC-5, set summary")
        );
        let unchanged = carnets.amend(&fake, &path, |_, summary| *summary = "New".into());
        assert_eq!(unchanged.unwrap().1, "New");
        assert_eq!(fake.calls().len(), 2, "no change, no commit");
    }

    #[test]
    fn an_edit_writes_every_key_in_its_normal_form() {
        let dir = tempfile::tempdir().unwrap();
        let readme = "+++\ngroup = \"login\"\nissues = [\"atelier#14\", \"ABC-1\"]\n+++\n";
        let path = repo(dir.path(), "2026-01-02-x", Some(readme));
        (carnets(dir.path()).set_closed(&Fake::default(), &path, true)).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "+++\ngroup = \"LOGIN\"\nissues = [\"o/atelier#14\", \"ABC-1\"]\nsummary = \"\"\nclosed = true\n+++\n"
        );
        assert_eq!(
            front(&path).issues(carnets(dir.path()).tracker).first(),
            Some(&IssueKey::listed("o/atelier#14".into()))
        );
    }

    #[test]
    fn a_readme_without_front_matter_gets_one_on_its_first_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo(dir.path(), "2026-01-02-ORD-7-crash", Some("# Crash\n"));
        let fake = Fake::default();
        let carnets = carnets(dir.path());
        set_group(&carnets, &fake, &path, group("crash").as_ref());
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "+++\ngroup = \"CRASH\"\nissues = []\nsummary = \"\"\nclosed = false\n+++\n\n# Crash\n",
            "the folder's key is no issue key"
        );
        let bare = repo(dir.path(), "2026-01-03-bare", None);
        carnets.set_closed(&fake, &bare, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(bare.join("README.md")).unwrap(),
            "+++\ngroup = \"\"\nissues = []\nsummary = \"\"\nclosed = true\n+++\n"
        );
        let broken = repo(
            dir.path(),
            "2026-01-04-broken",
            Some("+++\nnot toml\n+++\n"),
        );
        assert!(
            carnets.set_closed(&fake, &broken, true).is_err(),
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
    fn search_hits_group_by_carnet_folder() {
        let output = "/data/2026-01-01-a/README.md:3:the bug\n\
                      /data/2026-01-01-a/notes/log.txt:10:bug again\n\
                      /data/2026-01-02-b/README.md:1:# Bug\n\
                      /elsewhere/x:1:bug";
        let hits = group_hits(Path::new("/data"), output);
        assert_eq!(
            hits.into_iter().collect::<Vec<_>>(),
            [
                (
                    PathBuf::from("/data/2026-01-01-a"),
                    vec![
                        "README.md:3:the bug".to_owned(),
                        "notes/log.txt:10:bug again".to_owned()
                    ]
                ),
                (
                    PathBuf::from("/data/2026-01-02-b"),
                    vec!["README.md:1:# Bug".to_owned()]
                ),
            ]
        );
    }
}
