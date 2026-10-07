//! worktrunk: its user config, where atelier's hooks sit among the user's own, and `wt list`.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::hooks::{self, Phase};
use crate::links::{Links, group_text};
use crate::process::Runner;
use crate::reviews::Provider;

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
    /// How the forge refers to review `number`: `#12`, or `!12` on GitLab.
    pub fn review_reference(&self, number: u64) -> String {
        Provider::from_name(&self.provider)
            .unwrap_or(Provider::GitHub)
            .reference(number)
    }

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
    /// The repo's default branch, when worktrunk reports it.
    pub default_branch: Option<String>,
    /// Its branch is the repo's default branch.
    pub on_default: bool,
    /// worktrunk finds its branch integrated into the default branch's remote, so it is
    /// finished: `default_branch.integration` set, or `display.state` `integrated` or `empty`.
    pub integrated: bool,
    /// Commits ahead of the default branch, when worktrunk reports them.
    pub ahead_of_default: Option<u64>,
    /// Its branch's configured upstream no longer exists, as after a merged review deleted it.
    /// `wt list` cannot tell this apart from never pushed: [`crate::git::gone_branches`] does.
    pub gone: bool,
    /// Its branch's CI, when listed with `--full` and there is one to show.
    pub ci: Option<Ci>,
}

/// A branch's CI, as worktrunk's CI column reports it: its checks and its review's decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Ci {
    /// The column's one state, folding the checks, the decision and the conflicts.
    pub state: CiState,
    /// The checks' own status, `None` without checks.
    pub checks: Option<Checks>,
    /// The review cannot be merged: it conflicts with its base.
    pub conflicts: bool,
    /// Local HEAD differs from the remote, so the status is of an older commit.
    pub stale: bool,
    /// The checks are of the branch's own workflow, as for a default branch: it has no review.
    pub branch_workflow: bool,
    pub review: Option<CiReview>,
}

/// What worktrunk's CI column shows, its "no CI" aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CiState {
    Passed,
    Running,
    Failed,
    Conflicts,
    /// The status could not be fetched.
    Error,
    ChangesRequested,
    /// A required approval is not given yet.
    ApprovalPending,
}

impl CiState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Conflicts => "conflicts",
            Self::Error => "error",
            Self::ChangesRequested => Decision::ChangesRequested.label(),
            Self::ApprovalPending => Decision::Pending.label(),
        }
    }
}

/// A branch's checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    Passed,
    Running,
    Failed,
    /// The status could not be fetched.
    Unavailable,
}

impl Checks {
    fn from_status(status: &str) -> Option<Self> {
        match status {
            "passed" => Some(Self::Passed),
            "running" => Some(Self::Running),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// The open review of a branch, as worktrunk finds it.
#[derive(Debug, Clone, PartialEq)]
pub struct CiReview {
    pub number: Option<u64>,
    pub url: Option<String>,
    /// `None` when the forge reports no decision.
    pub decision: Option<Decision>,
}

/// What reviewers decided on a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    ChangesRequested,
    /// A required approval is not given yet.
    Pending,
    Draft,
    Approved,
}

impl Decision {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "changes_requested" => Some(Self::ChangesRequested),
            "pending" => Some(Self::Pending),
            "draft" => Some(Self::Draft),
            "approved" => Some(Self::Approved),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ChangesRequested => "changes requested",
            Self::Pending => "approval pending",
            Self::Draft => "draft",
            Self::Approved => "approved",
        }
    }
}

impl Ci {
    /// worktrunk's CI column from an item's `checks` and `pr`, `Some(None)` being `null`:
    /// both `null` is a fetch error, `pr.mergeable` false is conflicts, and changes requested
    /// outranks running checks while a pending approval only recolors a passing or check-less
    /// branch. `None` when there is nothing to show.
    fn of(checks: Option<Option<raw::Checks>>, pr: Option<Option<raw::Pr>>) -> Option<Self> {
        if let (Some(None), Some(None)) = (&checks, &pr) {
            return Some(Self {
                state: CiState::Error,
                checks: Some(Checks::Unavailable),
                conflicts: false,
                stale: false,
                branch_workflow: false,
                review: None,
            });
        }
        let checks = checks.flatten();
        let review = pr.flatten().map(|pr| {
            let conflicts = pr.mergeable == Some(false);
            let review = CiReview {
                number: pr.number,
                url: pr.url,
                decision: pr.review.as_deref().and_then(Decision::from_name),
            };
            (review, conflicts)
        });
        let status = checks.as_ref().and_then(|checks| checks.status.as_deref());
        let decision = review.as_ref().and_then(|(review, _)| review.decision);
        let conflicts = review.as_ref().is_some_and(|&(_, conflicts)| conflicts);
        let state = if conflicts {
            CiState::Conflicts
        } else if status == Some("failed") {
            CiState::Failed
        } else if decision == Some(Decision::ChangesRequested) {
            CiState::ChangesRequested
        } else if status == Some("running") {
            CiState::Running
        } else if decision == Some(Decision::Pending) {
            CiState::ApprovalPending
        } else if status == Some("passed") {
            CiState::Passed
        } else {
            return None;
        };
        Some(Self {
            state,
            checks: status.and_then(Checks::from_status),
            conflicts,
            stale: checks.as_ref().is_some_and(|checks| checks.stale),
            branch_workflow: checks.is_some_and(|checks| checks.source == "branch"),
            review: review.map(|(review, _)| review),
        })
    }

    /// worktrunk dims the column for a draft.
    pub fn draft(&self) -> bool {
        self.decision() == Some(Decision::Draft)
    }

    pub fn decision(&self) -> Option<Decision> {
        self.review.as_ref()?.decision
    }

    pub fn review_url(&self) -> Option<&str> {
        self.review.as_ref()?.url.as_deref()
    }
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

/// The worktree holding `path`, with its CI, from `wt list statusline`: the same item as
/// `wt list --full` reports. Its CI lookup is cached by worktrunk, and costs a second or two
/// when the cache is stale.
pub fn statusline(runner: &dyn Runner, path: &Path) -> Result<Statusline> {
    let path = path.to_string_lossy();
    let args = [
        "-C",
        &path,
        "--config-set",
        "list.json-schema=2",
        "list",
        "statusline",
    ];
    let listing =
        Listing::parse(&runner.output("wt", &[&args[..], &["--format", "json"]].concat())?)?;
    let tree = (listing.worktrees.into_iter().next())
        .ok_or_else(|| color_eyre::eyre::eyre!("wt list statusline listed no worktree"))?;
    Ok(Statusline {
        tree,
        forge: listing.forge,
    })
}

/// The worktree `wt list statusline` reports, with its repo's forge for review references.
#[derive(Debug, Clone, PartialEq)]
pub struct Statusline {
    pub tree: Worktree,
    pub forge: Option<Forge>,
}

/// Switches `repo` to `target` (`[--create] <branch>`, or `pr:N`), creating the worktree when
/// needed, and tells atelier's hooks the workspace it goes to, its group and the issue keys it
/// links beyond its branch's.
pub fn switch(
    runner: &dyn Runner,
    repo: &Path,
    target: &[&str],
    workspace: &str,
    links: &Links,
) -> Result<()> {
    let repo = repo.to_string_lossy();
    let workspace = format!("{}={workspace}", hooks::WORKSPACE_VAR);
    let group = format!("{}={}", hooks::GROUP_VAR, group_text(links.group.as_ref()));
    let keys = format!("{}={}", hooks::ISSUE_KEYS_VAR, links.issue_keys.join(","));
    let mut args = vec![
        workspace.as_str(),
        &group,
        &keys,
        "wt",
        "-C",
        &repo,
        "switch",
    ];
    args.extend(target);
    args.extend(["--no-cd", "--yes"]);
    runner.output("env", &args).map(drop)
}

/// Removes the worktree of `target`, a branch or a path, discarding its changes when `force`.
pub fn remove(runner: &dyn Runner, repo: &Path, target: &str, force: bool) -> Result<()> {
    let repo = repo.to_string_lossy();
    let mut args = vec!["-C", &repo, "remove", "--foreground", "--yes"];
    if force {
        args.push("--force");
    }
    args.push(target);
    runner.output("wt", &args).map(drop)
}

impl Listing {
    /// Parses the JSON, keeping items that have a worktree (`--branches` adds ones that don't).
    pub fn parse(json: &str) -> Result<Self> {
        let raw: raw::Listing = serde_json::from_str(json).wrap_err("parsing wt list")?;
        let default_branch = raw.repo.default_branch;
        let worktrees = raw
            .items
            .into_iter()
            .filter_map(|item| {
                let tree = item.worktree?;
                let changes = tree.changes;
                let on_default = match &default_branch {
                    Some(default) => item.branch.as_ref() == Some(default),
                    None => tree.main,
                };
                let to_default = item.default_branch.unwrap_or_default();
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
                    integrated: to_default.integration.is_some()
                        || matches!(item.display.state.as_str(), "integrated" | "empty"),
                    ahead_of_default: to_default.ahead,
                    symbols: item.display.symbols,
                    on_default,
                    default_branch: default_branch.clone(),
                    gone: false,
                    ci: Ci::of(item.checks, item.pr),
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
    use serde::de::IgnoredAny;

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
        pub default_branch: Option<String>,
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
        /// Absent or `null` when worktrunk did not compare it.
        pub default_branch: Option<DefaultBranch>,
        #[serde(default)]
        pub display: Display,
        /// `Some(None)` when `null`, which tells a fetch error apart from no CI.
        #[serde(default, deserialize_with = "present")]
        pub checks: Option<Option<Checks>>,
        #[serde(default, deserialize_with = "present")]
        pub pr: Option<Option<Pr>>,
    }

    /// A field that is there, even as `null`.
    fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::<T>::deserialize(deserializer)?))
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct Checks {
        pub status: Option<String>,
        pub source: String,
        pub stale: bool,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct Pr {
        pub number: Option<u64>,
        pub url: Option<String>,
        pub review: Option<String>,
        pub mergeable: Option<bool>,
    }

    /// How a branch compares with the repo's default branch.
    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct DefaultBranch {
        pub ahead: Option<u64>,
        /// Why it is integrated (`{"reason": "ancestor"}`, …); absent when it is not, `null`
        /// when undetermined.
        pub integration: Option<IgnoredAny>,
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
        pub state: String,
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
        assert_eq!(listing.worktrees.len(), 3);
        let main = &listing.worktrees[0];
        assert!(main.on_default);
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
        assert!(!feature.integrated && !feature.on_default && !feature.gone);
        let ci = main.ci.as_ref().unwrap();
        assert_eq!(ci.state, CiState::Passed);
        assert!(ci.branch_workflow && ci.review.is_none());
        let ci = feature.ci.as_ref().unwrap();
        assert_eq!(ci.state, CiState::ChangesRequested);
        assert!(ci.stale);
        assert_eq!(
            ci.review_url(),
            Some("https://github.com/remigourdon/atelier/pull/12")
        );
        let merged = &listing.worktrees[2];
        assert_eq!(merged.ci, None);
        assert!(merged.integrated);
        assert_eq!(merged.ahead_of_default, Some(1));
        assert_eq!(merged.default_branch.as_deref(), Some("main"));
    }

    fn integrated(item: &str) -> bool {
        let json = format!(r#"{{"items":[{{"branch":"b","worktree":{{"path":"/r.b"}},{item}}}]}}"#);
        Listing::parse(&json).unwrap().worktrees[0].integrated
    }

    #[test]
    fn integration_or_an_integrated_state_marks_a_branch_integrated() {
        assert!(integrated(
            r#""default_branch":{"integration":{"reason":"ancestor"}}"#
        ));
        assert!(!integrated(r#""default_branch":{"ahead":2}"#), "absent");
        assert!(
            !integrated(r#""default_branch":{"integration":null}"#),
            "undetermined"
        );
        assert!(!integrated(r#""default_branch":null"#));
        assert!(integrated(r#""display":{"state":"integrated"}"#));
        assert!(integrated(r#""display":{"state":"empty"}"#));
        assert!(!integrated(r#""display":{"state":"ahead"}"#));
    }

    fn ci(fields: &str) -> Option<Ci> {
        let json =
            format!(r#"{{"items":[{{"branch":"b","worktree":{{"path":"/r.b"}}{fields}}}]}}"#);
        Listing::parse(&json).unwrap().worktrees.remove(0).ci
    }

    fn state(fields: &str) -> Option<CiState> {
        ci(fields).map(|ci| ci.state)
    }

    #[test]
    fn ci_follows_worktrunks_column() {
        assert_eq!(state(""), None, "not collected or never pushed");
        assert_eq!(state(r#","pr":{"number":3}"#), None, "no CI");
        assert_eq!(state(r#","checks":null,"pr":null"#), Some(CiState::Error));
        assert_eq!(
            ci(r#","checks":null,"pr":null"#).unwrap().checks,
            Some(Checks::Unavailable)
        );
        let checks = |status: &str, pr: &str| {
            state(&format!(
                r#","checks":{{"status":{status},"source":"pr"}},"pr":{pr}"#
            ))
        };
        assert_eq!(checks(r#""passed""#, "{}"), Some(CiState::Passed));
        assert_eq!(checks(r#""running""#, "{}"), Some(CiState::Running));
        assert_eq!(checks(r#""failed""#, "{}"), Some(CiState::Failed));
        assert_eq!(
            checks("null", r#"{"mergeable":false}"#),
            Some(CiState::Conflicts)
        );
        assert_eq!(
            checks(r#""running""#, r#"{"review":"changes_requested"}"#),
            Some(CiState::ChangesRequested),
            "outranks running"
        );
        assert_eq!(
            checks(r#""failed""#, r#"{"review":"changes_requested"}"#),
            Some(CiState::Failed)
        );
        assert_eq!(
            checks(r#""passed""#, r#"{"review":"pending"}"#),
            Some(CiState::ApprovalPending)
        );
        assert_eq!(
            checks(r#""running""#, r#"{"review":"pending"}"#),
            Some(CiState::Running),
            "only recolors a passing branch"
        );
        assert_eq!(
            state(r#","pr":{"review":"pending"}"#),
            Some(CiState::ApprovalPending),
            "or a check-less one"
        );
    }

    #[test]
    fn ci_keeps_the_review_and_whether_it_is_stale_or_the_branch_workflows() {
        let review = ci(r#","checks":{"status":"passed","source":"pr","stale":true},
            "pr":{"number":27,"url":"https://github.com/o/r/pull/27","review":"draft"}"#)
        .unwrap();
        assert!(review.stale && !review.branch_workflow);
        assert!(review.draft());
        assert_eq!(review.review_url(), Some("https://github.com/o/r/pull/27"));
        assert_eq!(review.review.unwrap().number, Some(27));
        let main = ci(r#","checks":{"status":"failed","source":"branch"}"#).unwrap();
        assert!(main.branch_workflow && main.review.is_none());
        assert_eq!(main.state, CiState::Failed);
    }

    #[test]
    fn ci_keeps_the_checks_the_decision_and_the_conflicts_apart() {
        let ci = ci(r#","checks":{"status":"running","source":"pr"},
            "pr":{"number":3,"mergeable":false,"review":"changes_requested"}"#)
        .unwrap();
        assert_eq!(ci.state, CiState::Conflicts, "the column folds them");
        assert_eq!(ci.checks, Some(Checks::Running));
        assert_eq!(ci.decision(), Some(Decision::ChangesRequested));
        assert!(ci.conflicts);
    }

    #[test]
    fn the_default_branch_is_the_repos_else_the_main_worktrees() {
        let listing = Listing::parse(
            r#"{"repo":{"default_branch":"trunk"},"items":[
                {"branch":"main","worktree":{"path":"/r","main":true}},
                {"branch":"trunk","worktree":{"path":"/r.t"}}]}"#,
        )
        .unwrap();
        assert!(!listing.worktrees[0].on_default && listing.worktrees[1].on_default);
        let listing =
            Listing::parse(r#"{"items":[{"branch":"main","worktree":{"path":"/r","main":true}}]}"#)
                .unwrap();
        assert!(listing.worktrees[0].on_default);
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
    fn statusline_reads_the_one_worktree_with_its_ci() {
        let fake = crate::process::fake::Fake::default().always(
            "wt",
            Some(include_str!("../tests/fixtures/wt-statusline.json")),
        );
        let Statusline { tree, forge } = statusline(&fake, Path::new("/r.b")).unwrap();
        assert_eq!(forge.unwrap().provider, "github");
        assert_eq!(
            fake.calls(),
            ["wt -C /r.b --config-set list.json-schema=2 list statusline --format json"]
        );
        assert_eq!(tree.branch.as_deref(), Some("ABC-1-fix"));
        assert!(tree.dirty && !tree.main && !tree.integrated);
        assert_eq!(tree.upstream, Some((1, 2)));
        let ci = tree.ci.unwrap();
        assert_eq!(ci.state, CiState::Running);
        assert_eq!(ci.decision(), Some(Decision::Approved));
        assert_eq!(ci.review.unwrap().number, Some(31));
        let empty = crate::process::fake::Fake::default().always("wt", Some(r#"{"items":[]}"#));
        assert!(statusline(&empty, Path::new("/r")).is_err());
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
