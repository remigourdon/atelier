//! worktrunk: its user config, where atelier's hooks sit among the user's own, and `wt list`.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};
use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::hooks::Phase;
use crate::process::Runner;

/// The key atelier's command sits under in each hook's named table.
const ENTRY: &str = "atelier";

/// worktrunk's user config file.
pub fn config_path() -> PathBuf {
    crate::config::config_home().join("worktrunk/config.toml")
}

/// worktrunk's config as read from disk, so edits keep the user's formatting.
pub struct HooksConfig {
    path: PathBuf,
    original: String,
    doc: DocumentMut,
}

impl HooksConfig {
    /// Reads the file; a missing one is empty.
    pub fn load(path: &Path) -> Result<Self> {
        let original = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err.into()),
        };
        let doc = original
            .parse()
            .wrap_err_with(|| format!("parsing {}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            original,
            doc,
        })
    }

    /// Writes the file back if it changed. Returns whether it did.
    pub fn save(&self) -> Result<bool> {
        let text = self.doc.to_string();
        if text == self.original {
            return Ok(false);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, text)?;
        Ok(true)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `phase` runs atelier's hook, in either named or plain-string form.
    pub fn installed(&self, phase: Phase) -> bool {
        let ours = phase.command();
        match self.doc.get(phase.name()) {
            Some(item) if item.is_str() => item.as_str() == Some(&ours),
            Some(item) => entry(item) == Some(&ours),
            None => false,
        }
    }

    /// Adds a named `atelier` entry to each phase, keeping the user's other hooks.
    pub fn install(&mut self) -> Result<()> {
        for phase in Phase::ALL {
            let ours = phase.command();
            let item = self
                .doc
                .entry(phase.name())
                .or_insert_with(|| Item::Table(Table::new()));
            if let Some(existing) = item.as_str().map(str::to_owned) {
                let mut table = Table::new();
                if existing != ours {
                    table.insert("default", value(existing));
                }
                *item = Item::Table(table);
            }
            let Some(table) = item.as_table_like_mut() else {
                bail!(
                    "{} is a hook pipeline; add `{ENTRY} = \"{ours}\"` to a step by hand",
                    phase.name()
                );
            };
            table.insert(ENTRY, value(ours));
        }
        Ok(())
    }

    /// Removes atelier's entries, dropping phases left empty.
    pub fn uninstall(&mut self) {
        for phase in Phase::ALL {
            let ours = phase.command();
            let Some(item) = self.doc.get_mut(phase.name()) else {
                continue;
            };
            let empty = if item.is_str() {
                item.as_str() == Some(&ours)
            } else if let Some(table) = item.as_table_like_mut() {
                if table.get(ENTRY).and_then(Item::as_str) == Some(&ours) {
                    table.remove(ENTRY);
                }
                table.is_empty()
            } else {
                false
            };
            if empty {
                self.doc.remove(phase.name());
            }
        }
    }
}

/// One repo's worktrees as `wt list --format json` reports them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Listing {
    pub forge: Option<Forge>,
    pub worktrees: Vec<Worktree>,
}

/// Where a repo is hosted, as worktrunk detects it from its remote.
#[derive(Debug, Clone, PartialEq)]
pub struct Forge {
    /// The repo's web page.
    pub url: String,
    /// `github`, `gitlab`, …
    pub provider: String,
}

/// The host of a web URL: `https://github.com/o/r` → `github.com`.
pub fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().filter(|host| !host.is_empty())
}

/// Whether two project web pages are the same, ignoring case and a trailing `/` or `.git`.
pub fn same_project(a: &str, b: &str) -> bool {
    let normal = |url: &str| {
        let url = url.trim_end_matches('/');
        url.strip_suffix(".git").unwrap_or(url).to_lowercase()
    };
    normal(a) == normal(b)
}

impl Forge {
    /// The web page of a branch.
    pub fn branch_url(&self, branch: &str) -> String {
        match self.provider.as_str() {
            "gitlab" => format!("{}/-/tree/{branch}", self.url),
            _ => format!("{}/tree/{branch}", self.url),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Worktree {
    pub path: PathBuf,
    /// `None` when detached.
    pub branch: Option<String>,
    pub main: bool,
    pub dirty: bool,
    /// Lines added and deleted in the working tree.
    pub diff: (u64, u64),
    /// Ahead and behind the upstream branch, when there is one.
    pub upstream: Option<(u64, u64)>,
    pub short_sha: String,
    pub subject: String,
    pub committed_at: String,
    /// worktrunk's compact status, such as `!?↑`.
    pub symbols: String,
}

/// Lists a repo's worktrees, pinning the JSON schema whatever the user's config says.
pub fn list(runner: &dyn Runner, repo: &Path, full: bool) -> Result<Listing> {
    let repo = repo.to_string_lossy();
    let mut args = vec!["-C", &repo, "--config-set", "list.json-schema=2", "list"];
    args.extend(["--format", "json"]);
    if full {
        args.push("--full");
    }
    Listing::parse(&runner.output("wt", &args)?)
}

impl Listing {
    /// Parses the JSON, keeping items that have a worktree (`--branches` adds ones that don't).
    pub fn parse(json: &str) -> Result<Self> {
        let raw: raw::Listing = serde_json::from_str(json).wrap_err("parsing wt list")?;
        let worktrees = raw
            .items
            .into_iter()
            .filter_map(|item| {
                let tree = item.worktree?;
                let changes = tree.changes;
                Some(Worktree {
                    path: tree.path,
                    branch: item.branch.filter(|_| !tree.detached),
                    main: tree.main,
                    dirty: changes.staged
                        || changes.modified
                        || changes.untracked
                        || changes.renamed
                        || changes.deleted
                        || changes.conflicted,
                    diff: (changes.diff.added, changes.diff.deleted),
                    upstream: item.upstream.map(|up| (up.ahead, up.behind)),
                    short_sha: item.head.short_sha,
                    subject: item.head.subject,
                    committed_at: item.head.committed_at,
                    symbols: item.display.symbols,
                })
            })
            .collect();
        Ok(Self {
            forge: raw.repo.forge.map(|forge| Forge {
                url: forge.url,
                provider: forge.provider,
            }),
            worktrees,
        })
    }
}

/// The subset of worktrunk's JSON schema 2 that atelier reads; everything is optional.
mod raw {
    use std::path::PathBuf;

    use serde::Deserializer;

    use super::Deserialize;

    /// A section that is `null`, as `head` and `changes` are in a repo without commits, reads
    /// as its default.
    fn nullable<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
    }

    #[derive(Deserialize)]
    pub struct Listing {
        #[serde(default)]
        pub repo: Repo,
        #[serde(default)]
        pub items: Vec<Item>,
    }

    #[derive(Deserialize, Default)]
    pub struct Repo {
        pub forge: Option<Forge>,
    }

    #[derive(Deserialize)]
    pub struct Forge {
        pub url: String,
        #[serde(default)]
        pub provider: String,
    }

    #[derive(Deserialize)]
    pub struct Item {
        pub branch: Option<String>,
        #[serde(default, deserialize_with = "nullable")]
        pub head: Head,
        pub worktree: Option<Tree>,
        pub upstream: Option<Upstream>,
        #[serde(default)]
        pub display: Display,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct Head {
        pub short_sha: String,
        pub subject: String,
        pub committed_at: String,
    }

    #[derive(Deserialize)]
    pub struct Tree {
        pub path: PathBuf,
        #[serde(default)]
        pub main: bool,
        #[serde(default)]
        pub detached: bool,
        #[serde(default, deserialize_with = "nullable")]
        pub changes: Changes,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct Changes {
        pub staged: bool,
        pub modified: bool,
        pub untracked: bool,
        pub renamed: bool,
        pub deleted: bool,
        pub conflicted: bool,
        pub diff: Diff,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct Diff {
        pub added: u64,
        pub deleted: u64,
    }

    #[derive(Deserialize)]
    pub struct Upstream {
        #[serde(default)]
        pub ahead: u64,
        #[serde(default)]
        pub behind: u64,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct Display {
        pub symbols: String,
    }
}

fn entry(item: &Item) -> Option<&str> {
    item.as_table_like()?.get(ENTRY)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_projects_compare_loosely() {
        assert_eq!(host("https://github.com/o/r"), Some("github.com"));
        assert_eq!(host("gitlab.example.com/g/r"), Some("gitlab.example.com"));
        assert_eq!(host(""), None);
        assert!(same_project(
            "https://GitHub.com/O/R/",
            "https://github.com/o/r.git"
        ));
        assert!(!same_project(
            "https://github.com/o/r",
            "https://github.com/o/r2"
        ));
    }

    #[test]
    fn listing_keeps_worktrees_only() {
        let listing = Listing::parse(include_str!("../tests/fixtures/wt-list.json")).unwrap();
        let forge = listing.forge.unwrap();
        assert_eq!(forge.url, "https://github.com/remigourdon/atelier");
        assert_eq!(
            forge.branch_url("a/b"),
            "https://github.com/remigourdon/atelier/tree/a/b"
        );
        assert_eq!(listing.worktrees.len(), 2);
        let main = &listing.worktrees[0];
        assert_eq!(main.path, Path::new("/home/remi/atelier"));
        assert_eq!(main.branch.as_deref(), Some("main"));
        assert!(main.main && !main.dirty);
        assert_eq!(main.upstream, Some((0, 2)));
        assert_eq!(main.short_sha, "e5c1dff");
        assert_eq!(main.subject, "Phase 1: core, CLI and hooks (#9)");
        let feature = &listing.worktrees[1];
        assert!(!feature.main && feature.dirty);
        assert_eq!(feature.upstream, None);
        assert_eq!(feature.symbols, "!?↑");
        assert_eq!(feature.diff, (12, 3));
    }

    #[test]
    fn list_pins_the_schema() {
        let fake = crate::process::fake::Fake::default().always("wt", Some(r#"{"items":[]}"#));
        list(&fake, Path::new("/r"), true).unwrap();
        assert_eq!(
            fake.calls(),
            ["wt -C /r --config-set list.json-schema=2 list --format json --full"]
        );
    }

    #[test]
    fn gitlab_branch_urls_use_its_tree_path() {
        let forge = Forge {
            url: "https://gitlab.com/g/r".into(),
            provider: "gitlab".into(),
        };
        assert_eq!(
            forge.branch_url("main"),
            "https://gitlab.com/g/r/-/tree/main"
        );
    }

    #[test]
    fn listing_tolerates_missing_sections() {
        let listing =
            Listing::parse(r#"{"items":[{"worktree":{"path":"/r","detached":true}}]}"#).unwrap();
        assert_eq!(listing.forge, None);
        assert_eq!(listing.worktrees[0].branch, None);
        assert!(Listing::parse("nope").is_err());
    }

    #[test]
    fn listing_tolerates_a_repo_without_commits() {
        let listing = Listing::parse(
            r#"{"items":[{"branch":"main","head":null,
                "worktree":{"path":"/r","main":true,"changes":null}}]}"#,
        )
        .unwrap();
        assert_eq!(listing.worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(listing.worktrees[0].short_sha, "");
    }

    fn config(text: &str) -> (tempfile::TempDir, HooksConfig) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worktrunk/config.toml");
        if !text.is_empty() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let config = HooksConfig::load(&path).unwrap();
        (dir, config)
    }

    #[test]
    fn install_adds_named_entries_and_keeps_other_hooks() {
        let (_dir, mut c) = config(
            "worktree-path = \"x\"\npre-start = \"npm ci\"\n\n[post-remove]\nstop = \"kill\"\n",
        );
        c.install().unwrap();
        assert!(Phase::ALL.iter().all(|&phase| c.installed(phase)));
        assert_eq!(c.doc["pre-start"]["default"].as_str(), Some("npm ci"));
        assert_eq!(c.doc["post-remove"]["stop"].as_str(), Some("kill"));
        assert_eq!(
            c.doc["pre-switch"]["atelier"].as_str(),
            Some("atelier hook pre-switch")
        );
        let once = c.doc.to_string();
        c.install().unwrap();
        assert_eq!(c.doc.to_string(), once);
    }

    #[test]
    fn install_converts_atelier_plain_strings() {
        let (_dir, mut c) = config("\"pre-start\" = \"atelier hook pre-start\"\n");
        assert!(c.installed(Phase::PreStart));
        c.install().unwrap();
        assert_eq!(c.doc["pre-start"].as_table().unwrap().len(), 1);
        assert_eq!(
            c.doc["pre-start"]["atelier"].as_str(),
            Some("atelier hook pre-start")
        );
    }

    #[test]
    fn install_refuses_pipelines() {
        let (_dir, mut c) = config("[[pre-start]]\ninstall = \"npm ci\"\n");
        assert!(c.install().is_err());
    }

    #[test]
    fn uninstall_removes_only_atelier() {
        let (_dir, mut c) =
            config("pre-switch = \"atelier hook pre-switch\"\n[post-remove]\nstop = \"kill\"\n");
        c.install().unwrap();
        c.uninstall();
        assert!(Phase::ALL.iter().all(|&phase| !c.installed(phase)));
        assert!(c.doc.get("pre-start").is_none());
        assert!(c.doc.get("pre-switch").is_none());
        assert_eq!(c.doc["post-remove"]["stop"].as_str(), Some("kill"));
    }

    #[test]
    fn save_writes_only_changes_and_creates_the_directory() {
        let (_dir, mut c) = config("");
        assert!(!c.save().unwrap());
        assert!(!c.path().exists());
        c.install().unwrap();
        assert!(c.save().unwrap());
        let reloaded = HooksConfig::load(c.path()).unwrap();
        assert!(Phase::ALL.iter().all(|&phase| reloaded.installed(phase)));
        assert!(!reloaded.save().unwrap());
    }

    #[test]
    fn unparsable_config_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "pre-start = ").unwrap();
        assert!(HooksConfig::load(&path).is_err());
    }
}
