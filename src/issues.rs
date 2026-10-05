//! Issues from GitHub (`gh`) and Jira (`acli`), normalised to a state, labels and whether they
//! are blocked, and placed in sections by ordered rules. Behind a trait so native APIs can come
//! later.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Report, Result, WrapErr, eyre};
use serde::{Deserialize, Serialize};

use crate::process::Runner;

/// How far back closed GitHub issues are listed, by when they were last updated.
pub const CLOSED_DAYS: u64 = 14;

/// The title of the section that takes issues no configured section matches.
pub const OTHER: &str = "Other";

/// Where issues come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tracker {
    GitHub,
    Jira,
}

impl Tracker {
    /// The CLI it is reached through, which also names its cache and loading indicator.
    pub fn cli(self) -> &'static str {
        match self {
            Tracker::GitHub => "gh",
            Tracker::Jira => "acli",
        }
    }

    /// Its issues in `scope`, a GitHub repo or a JQL search, through its CLI.
    pub fn issues<'a>(
        self,
        runner: &'a dyn Runner,
        scope: String,
        tracker: &TrackerConfig,
    ) -> Box<dyn Issues + 'a> {
        match self {
            Tracker::GitHub => Box::new(Gh {
                runner,
                qualified: tracker.qualified(&scope),
                repo: scope,
                since: days_ago(CLOSED_DAYS),
            }),
            Tracker::Jira => Box::new(Acli {
                runner,
                jql: scope,
                site: tracker.jira_site(),
            }),
        }
    }
}

/// An issue's normalised progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Todo,
    InProgress,
    Done,
}

impl State {
    pub const ALL: [State; 3] = [State::Todo, State::InProgress, State::Done];

    pub fn label(self) -> &'static str {
        match self {
            State::Todo => "To do",
            State::InProgress => "In progress",
            State::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub tracker: Tracker,
    /// `ABC-123`, or `repo#12` on GitHub (`owner/repo#12` when two configured repos share a
    /// name). It is also the group of the issue's linked work.
    pub key: String,
    pub title: String,
    /// Its web page; for Jira, unknown when no site is configured or reported.
    pub url: Option<String>,
    /// `owner/repo`, or a Jira project key.
    pub project: String,
    /// The repo's web page on GitHub, to find its registered repo.
    pub project_url: Option<String>,
    pub state: State,
    /// The tracker's own status name.
    pub status: String,
    pub labels: Vec<String>,
    pub blocked: bool,
    pub assignees: Vec<String>,
    /// Jira's issue type and priority.
    pub kind: Option<String>,
    pub priority: Option<String>,
    pub updated_at: String,
}

impl Issue {
    /// A branch for working on it: its key or number, then its title in kebab case.
    pub fn branch(&self) -> String {
        let id = self.key.rsplit_once('#').map_or(&*self.key, |(_, n)| n);
        let mut slug = String::new();
        for word in self
            .title
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
        {
            if slug.len() + word.len() > 40 {
                break;
            }
            slug.push('-');
            slug.push_str(&word.to_lowercase());
        }
        format!("{id}{slug}")
    }
}

pub trait Issues {
    fn tracker(&self) -> Tracker;

    /// What it lists, a repo or a JQL search, which keys its cache.
    fn scope(&self) -> &str;

    fn issues(&self) -> Result<Vec<Issue>>;
}

/// Conditions on an issue; one with none matches every issue.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Rule {
    /// Any of these labels.
    pub labels: Vec<String>,
    /// None of these labels.
    pub not_labels: Vec<String>,
    /// Any of these states.
    pub state: Vec<State>,
    pub blocked: Option<bool>,
}

impl Rule {
    pub fn matches(&self, issue: &Issue) -> bool {
        let has =
            |wanted: &String| (issue.labels.iter()).any(|label| label.eq_ignore_ascii_case(wanted));
        (self.labels.is_empty() || self.labels.iter().any(has))
            && !self.not_labels.iter().any(has)
            && (self.state.is_empty() || self.state.contains(&issue.state))
            && self.blocked.is_none_or(|blocked| blocked == issue.blocked)
    }

    fn is_empty(&self) -> bool {
        *self == Rule::default()
    }
}

/// An Issues sub-tab: the issues its rule matches that no earlier section took.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Section {
    pub title: String,
    #[serde(flatten)]
    pub rule: Rule,
}

/// One section per state, when none are configured.
static BY_STATE: LazyLock<Vec<Section>> = LazyLock::new(|| {
    State::ALL
        .into_iter()
        .map(|state| Section {
            title: state.label().into(),
            rule: Rule {
                state: vec![state],
                ..Rule::default()
            },
        })
        .collect()
});

/// `[tracker]`: where issues come from and how they are sectioned.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    pub github: Option<GitHubTracker>,
    pub jira: Option<JiraTracker>,
    /// Issues it matches are never listed; with no conditions it hides nothing.
    pub hide: Rule,
    sections: Vec<Section>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GitHubTracker {
    /// `owner/name`, each listed for its open and recently closed issues.
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JiraTracker {
    pub jql: String,
    /// The site issue links point to, else `$ATLASSIAN_URL`, else `$JIRA_URL`.
    pub url: Option<String>,
}

impl TrackerConfig {
    /// The configured sections, else one per state. [`OTHER`] follows them.
    pub fn sections(&self) -> &[Section] {
        if self.sections.is_empty() {
            &BY_STATE
        } else {
            &self.sections
        }
    }

    /// A section's title, [`OTHER`] past the configured ones.
    pub fn title(&self, index: usize) -> &str {
        self.sections()
            .get(index)
            .map_or(OTHER, |section| &section.title)
    }

    /// The section an issue is listed in: the first whose rule matches, else [`OTHER`] (one
    /// past the last), and none when it is hidden.
    pub fn section(&self, issue: &Issue) -> Option<usize> {
        if !self.hide.is_empty() && self.hide.matches(issue) {
            return None;
        }
        let sections = self.sections();
        let index = sections
            .iter()
            .position(|section| section.rule.matches(issue));
        Some(index.unwrap_or(sections.len()))
    }

    /// What each source lists: GitHub repos, or one JQL search.
    pub fn scopes(&self) -> Vec<(Tracker, Vec<String>)> {
        let mut scopes = Vec::new();
        if let Some(github) = self.github.as_ref().filter(|gh| !gh.repos.is_empty()) {
            scopes.push((Tracker::GitHub, github.repos.clone()));
        }
        if let Some(jira) = self.jira.as_ref().filter(|jira| !jira.jql.is_empty()) {
            scopes.push((Tracker::Jira, vec![jira.jql.clone()]));
        }
        scopes
    }

    /// Whether a GitHub repo's issue keys need its owner: another configured repo has its name.
    fn qualified(&self, repo: &str) -> bool {
        let name = |repo: &str| repo.rsplit('/').next().unwrap_or_default().to_lowercase();
        let repos = self.github.iter().flat_map(|github| &github.repos);
        repos
            .filter(|other| !other.eq_ignore_ascii_case(repo))
            .any(|other| name(other) == name(repo))
    }

    /// Jira's web address for issue links, when it is known.
    pub fn jira_site(&self) -> Option<String> {
        let configured = self.jira.as_ref().and_then(|jira| jira.url.clone());
        configured
            .or_else(|| std::env::var("ATLASSIAN_URL").ok())
            .or_else(|| std::env::var("JIRA_URL").ok())
            .filter(|url| !url.is_empty())
    }
}

/// The moment `days` ago, as GitHub's `DateTime`.
fn days_ago(days: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    timestamp(now.saturating_sub(days * 86_400))
}

/// Seconds since the Unix epoch as `YYYY-MM-DDThh:mm:ssZ`, the day by Howard Hinnant's
/// `civil_from_days`.
fn timestamp(seconds: u64) -> String {
    let (day, time) = (seconds / 86_400, seconds % 86_400);
    let z = day + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    let (hour, minute, second) = (time / 3600, time % 3600 / 60, time % 60);
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// GitHub through `gh api graphql`: one repo's open issues, then those closed and updated since
/// `since`, each most recently updated first.
pub struct Gh<'a> {
    pub runner: &'a dyn Runner,
    /// `owner/name`.
    pub repo: String,
    /// Whether its keys carry the owner.
    pub qualified: bool,
    pub since: String,
}

/// `--paginate` pages through the issues by `$endCursor` and `pageInfo`; `{filter}` picks open
/// or recently closed ones. `blockedBy` counts open blockers only.
fn gh_query(variables: &str, filter: &str) -> String {
    format!(
        "query($owner: String!, $name: String!, {variables}$endCursor: String) {{ \
         repository(owner: $owner, name: $name) {{ nameWithOwner url \
         issues(first: 100, after: $endCursor, {filter}, \
         orderBy: {{field: UPDATED_AT, direction: DESC}}) {{ \
         nodes {{ number state title url updatedAt stateReason \
         assignees(first: 100) {{ nodes {{ login }} }} \
         labels(first: 100) {{ nodes {{ name }} }} \
         issueDependenciesSummary {{ blockedBy }} \
         closedByPullRequestsReferences(first: 100) {{ nodes {{ state }} }} }} \
         pageInfo {{ hasNextPage endCursor }} }} }} }}"
    )
}

impl Gh<'_> {
    fn list(&self, query: &str, extra: &[String]) -> Result<Vec<Issue>> {
        let (owner, name) =
            (self.repo.split_once('/')).ok_or_else(|| eyre!("{} is not owner/name", self.repo))?;
        let mut args = vec![
            "api".to_owned(),
            "graphql".into(),
            "--paginate".into(),
            "--slurp".into(),
            "-f".into(),
            format!("query={query}"),
            "-f".into(),
            format!("owner={owner}"),
            "-f".into(),
            format!("name={name}"),
        ];
        args.extend(extra.iter().cloned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        parse_gh(&self.runner.output("gh", &args)?, self.qualified)
    }
}

impl Issues for Gh<'_> {
    fn tracker(&self) -> Tracker {
        Tracker::GitHub
    }

    fn scope(&self) -> &str {
        &self.repo
    }

    fn issues(&self) -> Result<Vec<Issue>> {
        let mut issues = self.list(&gh_query("", "states: OPEN"), &[])?;
        let closed = gh_query(
            "$since: DateTime!, ",
            "states: CLOSED, filterBy: {since: $since}",
        );
        issues.extend(self.list(&closed, &["-f".into(), format!("since={}", self.since)])?);
        Ok(issues)
    }
}

/// Jira through `acli jira workitem search`, every result of a JQL search in its order, as the
/// Python prototype ran it against a live Jira.
pub struct Acli<'a> {
    pub runner: &'a dyn Runner,
    pub jql: String,
    /// The site issue links point to; else it is read from each issue's API address.
    pub site: Option<String>,
}

impl Issues for Acli<'_> {
    fn tracker(&self) -> Tracker {
        Tracker::Jira
    }

    fn scope(&self) -> &str {
        &self.jql
    }

    fn issues(&self) -> Result<Vec<Issue>> {
        let json = self.runner.output(
            "acli",
            &[
                "jira",
                "workitem",
                "search",
                "--jql",
                &self.jql,
                "--fields",
                "key,summary,status,labels,assignee,updated,issuetype,priority",
                "--json",
                "--paginate",
            ],
        )?;
        parse_acli(&json, self.site.as_deref())
    }
}

/// Parses the pages of `gh api graphql`'s repository issues. A closed issue is `done`, with why
/// as its status; an open one is `in_progress` while a pull request that closes it is open,
/// else `todo`. With `qualified`, keys carry the repo's owner.
pub fn parse_gh(json: &str, qualified: bool) -> Result<Vec<Issue>> {
    let pages: Vec<raw::GhResponse> = serde_json::from_str(json).wrap_err("parsing gh issues")?;
    Ok(pages
        .into_iter()
        .flat_map(|page| {
            let repo = page.data.repository;
            let prefix = match repo.name_with_owner.rsplit_once('/') {
                Some((_, name)) if !qualified => name.to_owned(),
                _ => repo.name_with_owner.clone(),
            };
            repo.issues.nodes.into_iter().map(move |node| {
                let pull_open = (node.closed_by_pull_requests_references.nodes.iter())
                    .any(|pull| pull.state == "OPEN");
                let (state, status) = match node.state.as_str() {
                    "CLOSED" => (
                        State::Done,
                        match node.state_reason.as_deref() {
                            Some("NOT_PLANNED") => "not planned".into(),
                            Some(reason) => reason.to_lowercase(),
                            None => "closed".into(),
                        },
                    ),
                    _ if pull_open => (State::InProgress, "open, pull request open".into()),
                    _ => (State::Todo, "open".into()),
                };
                Issue {
                    tracker: Tracker::GitHub,
                    key: format!("{prefix}#{}", node.number),
                    title: node.title,
                    url: Some(node.url),
                    project: repo.name_with_owner.clone(),
                    project_url: Some(repo.url.clone()),
                    state,
                    status,
                    labels: (node.labels.nodes.into_iter())
                        .map(|label| label.name)
                        .collect(),
                    blocked: node.issue_dependencies_summary.blocked_by > 0,
                    assignees: (node.assignees.nodes.into_iter())
                        .map(|user| user.login)
                        .collect(),
                    kind: None,
                    priority: None,
                    updated_at: node.updated_at,
                }
            })
        })
        .collect())
}

/// Parses `acli jira workitem search --json`. The state comes from the status category, and
/// an issue in status `Blocked` is blocked.
pub fn parse_acli(json: &str, site: Option<&str>) -> Result<Vec<Issue>> {
    let raw: Vec<raw::JiraIssue> = serde_json::from_str(json).wrap_err("parsing acli issues")?;
    Ok(raw
        .into_iter()
        .map(|issue| {
            let fields = issue.fields;
            let site = site
                .map(str::to_owned)
                .or_else(|| (issue.self_url.split_once("/rest/")).map(|(site, _)| site.to_owned()));
            let state = match fields.status.status_category.key.as_str() {
                "indeterminate" => State::InProgress,
                "done" => State::Done,
                _ => State::Todo,
            };
            Issue {
                tracker: Tracker::Jira,
                url: site
                    .map(|site| format!("{}/browse/{}", site.trim_end_matches('/'), issue.key)),
                project: (issue.key.split_once('-'))
                    .map_or(issue.key.clone(), |(project, _)| project.to_owned()),
                key: issue.key,
                title: fields.summary,
                project_url: None,
                state,
                blocked: fields.status.name.eq_ignore_ascii_case("blocked"),
                status: fields.status.name,
                labels: fields.labels,
                assignees: (fields.assignee.into_iter())
                    .map(|user| user.display_name)
                    .collect(),
                kind: fields.issuetype.map(|named| named.name),
                priority: fields.priority.map(|named| named.name),
                updated_at: fields.updated,
            }
        })
        .collect())
}

/// The issues in `api`'s scope through the cache, as [`crate::state::State::fetch_cached`]
/// serves them.
pub fn fetch(
    state: &crate::state::State,
    api: &dyn Issues,
    force: bool,
) -> (Vec<Issue>, Option<Report>) {
    let source = api.tracker().cli();
    let key = format!("issues {}", api.scope());
    let (issues, error) = state.fetch_cached(source, &key, force, || api.issues());
    let error = error.map(|err| err.wrap_err(format!("{source} issues in {}", api.scope())));
    (issues, error)
}

/// The issue `key` as the last fetch of a configured scope left it in the cache, and when that
/// fetch ran. Runs no command.
pub fn cached(
    state: &crate::state::State,
    config: &TrackerConfig,
    key: &str,
) -> Result<Option<(Issue, String)>> {
    let mut found = None;
    for (tracker, scopes) in config.scopes() {
        for scope in scopes {
            let entry = state.cached_entry(tracker.cli(), &format!("issues {scope}"))?;
            let Some((json, fetched_at)) = entry else {
                continue;
            };
            let Ok(issues) = serde_json::from_str::<Vec<Issue>>(&json) else {
                continue;
            };
            if let Some(issue) = issues.into_iter().find(|issue| issue.key == key)
                && found.as_ref().is_none_or(|(_, at)| *at < fetched_at)
            {
                found = Some((issue, fetched_at));
            }
        }
    }
    Ok(found)
}

/// The subset of each tracker's JSON that atelier reads.
mod raw {
    use super::Deserialize;

    #[derive(Deserialize)]
    pub struct GhResponse {
        pub data: GhData,
    }

    #[derive(Deserialize)]
    pub struct GhData {
        pub repository: GhRepository,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct GhRepository {
        pub name_with_owner: String,
        pub url: String,
        pub issues: GhConnection<GhIssue>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct GhConnection<T> {
        #[serde(default = "Vec::new")]
        pub nodes: Vec<T>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct GhIssue {
        pub number: u64,
        pub state: String,
        #[serde(default)]
        pub title: String,
        pub url: String,
        #[serde(default)]
        pub updated_at: String,
        pub state_reason: Option<String>,
        pub assignees: GhConnection<GhUser>,
        pub labels: GhConnection<GhLabel>,
        #[serde(default)]
        pub issue_dependencies_summary: GhDependencies,
        pub closed_by_pull_requests_references: GhConnection<GhPull>,
    }

    #[derive(Deserialize)]
    pub struct GhUser {
        pub login: String,
    }

    #[derive(Deserialize)]
    pub struct GhLabel {
        pub name: String,
    }

    #[derive(Deserialize)]
    pub struct GhPull {
        /// `OPEN`, `CLOSED` or `MERGED`.
        pub state: String,
    }

    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    pub struct GhDependencies {
        /// Open blockers only.
        pub blocked_by: u64,
    }

    #[derive(Deserialize)]
    pub struct JiraIssue {
        pub key: String,
        #[serde(rename = "self", default)]
        pub self_url: String,
        pub fields: JiraFields,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct JiraFields {
        pub summary: String,
        pub labels: Vec<String>,
        pub updated: String,
        /// `null` when unassigned.
        pub assignee: Option<JiraUser>,
        pub status: JiraStatus,
        pub issuetype: Option<JiraNamed>,
        pub priority: Option<JiraNamed>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct JiraUser {
        pub display_name: String,
    }

    #[derive(Deserialize)]
    pub struct JiraNamed {
        pub name: String,
    }

    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    pub struct JiraStatus {
        pub name: String,
        pub status_category: JiraCategory,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct JiraCategory {
        /// `new`, `indeterminate` or `done`.
        pub key: String,
    }
}

#[cfg(test)]
pub mod tests {
    use std::cell::Cell;

    use rusqlite::Connection;

    use super::*;
    use crate::process::fake::Fake;

    /// Recorded from `gh` on this repo, two to a page, then given labels, an assignee, a
    /// blocker and an open pull request, which its issues have none of.
    pub const GH: &str = include_str!("../tests/fixtures/gh-issues.json");
    /// Recorded likewise; one completed close is turned into not planned.
    pub const GH_CLOSED: &str = include_str!("../tests/fixtures/gh-closed-issues.json");
    /// Shaped as the Python prototype read `acli jira workitem search --json` from a live Jira.
    const ACLI: &str = include_str!("../tests/fixtures/acli-issues.json");

    #[test]
    fn gh_open_issues_parse_every_page_with_labels_blockers_and_pull_requests() {
        let issues = parse_gh(GH, false).unwrap();
        let keys: Vec<&str> = issues.iter().map(|issue| issue.key.as_str()).collect();
        assert_eq!(keys, ["atelier#5", "atelier#6", "atelier#7"], "two pages");
        let first = &issues[0];
        assert_eq!(first.title, "Phase 4: Issues panel");
        assert_eq!(first.project, "remigourdon/atelier");
        assert_eq!(
            first.project_url.as_deref(),
            Some("https://github.com/remigourdon/atelier")
        );
        assert_eq!(
            first.url.as_deref(),
            Some("https://github.com/remigourdon/atelier/issues/5")
        );
        assert_eq!(first.labels, ["enhancement", "ready-for-agent"]);
        assert_eq!(first.assignees, ["remigourdon"]);
        let states: Vec<State> = issues.iter().map(|issue| issue.state).collect();
        assert_eq!(
            states,
            [State::InProgress, State::Todo, State::Todo],
            "an open pull request closes #5"
        );
        let blocked: Vec<bool> = issues.iter().map(|issue| issue.blocked).collect();
        assert_eq!(blocked, [false, true, false]);
        assert_eq!(parse_gh(GH, true).unwrap()[0].key, "remigourdon/atelier#5");
        assert!(parse_gh("{\"errors\":[]}", false).is_err());
    }

    #[test]
    fn gh_closed_issues_are_done_with_their_reason() {
        let issues = parse_gh(GH_CLOSED, false).unwrap();
        assert!(issues.iter().all(|issue| issue.state == State::Done));
        let statuses: Vec<&str> = issues.iter().map(|issue| issue.status.as_str()).collect();
        assert_eq!(
            statuses,
            ["completed", "not planned", "completed", "completed"],
            "a merged pull request does not make it in progress"
        );
    }

    #[test]
    fn acli_issues_take_their_state_from_the_status_category() {
        let issues = parse_acli(ACLI, None).unwrap();
        let states: Vec<State> = issues.iter().map(|issue| issue.state).collect();
        assert_eq!(states, [State::InProgress, State::Todo, State::Done]);
        let first = &issues[0];
        assert_eq!(first.key, "ORD-3479");
        assert_eq!(first.project, "ORD");
        assert_eq!(first.project_url, None);
        assert_eq!(first.status, "In Review");
        assert_eq!(first.labels, ["backend", "perf"]);
        assert_eq!(first.assignees, ["Alice Martin"]);
        assert_eq!(
            (first.kind.as_deref(), first.priority.as_deref()),
            (Some("Story"), Some("High"))
        );
        assert_eq!(
            first.url.as_deref(),
            Some("https://example.atlassian.net/browse/ORD-3479")
        );
        assert!(issues[1].blocked, "in status Blocked");
        assert!(issues[1].assignees.is_empty() && issues[2].assignees.is_empty());
        let issues = parse_acli(ACLI, Some("https://jira.example.com/")).unwrap();
        assert_eq!(
            issues[0].url.as_deref(),
            Some("https://jira.example.com/browse/ORD-3479")
        );
        let unknown = parse_acli(r#"[{"key":"ORD-1","fields":{}}]"#, None).unwrap();
        assert_eq!(unknown[0].url, None, "no site configured or reported");
    }

    #[test]
    fn gh_lists_open_then_recently_closed_issues() {
        let fake = Fake::default()
            .once("gh", Some(GH))
            .once("gh", Some(GH_CLOSED));
        let gh = Gh {
            runner: &fake,
            repo: "remigourdon/atelier".into(),
            qualified: false,
            since: "2026-09-20T00:00:00Z".into(),
        };
        assert_eq!(gh.issues().unwrap().len(), 7);
        let calls = fake.calls();
        assert!(calls[0].starts_with("gh api graphql --paginate --slurp -f query="));
        assert!(calls[0].contains("states: OPEN") && !calls[0].contains("$since"));
        assert!(calls[0].ends_with("-f owner=remigourdon -f name=atelier"));
        assert!(calls[1].contains("states: CLOSED, filterBy: {since: $since}"));
        assert!(calls[1].ends_with("-f name=atelier -f since=2026-09-20T00:00:00Z"));
        let bad = Gh {
            repo: "atelier".into(),
            ..gh
        };
        assert!(bad.issues().is_err());
    }

    #[test]
    fn acli_runs_the_search_with_every_field_it_reads() {
        let fake = Fake::default().always("acli", Some(ACLI));
        let jira = Tracker::Jira.issues(&fake, "project = ORD".into(), &TrackerConfig::default());
        assert_eq!(jira.issues().unwrap().len(), 3);
        assert_eq!(
            fake.calls(),
            ["acli jira workitem search --jql project = ORD \
                 --fields key,summary,status,labels,assignee,updated,issuetype,priority \
                 --json --paginate"]
        );
    }

    #[test]
    fn timestamps_count_from_the_epoch() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(20_365 * 86_400 + 3_723), "2025-10-04T01:02:03Z");
        assert_eq!(timestamp(11_016 * 86_400 + 86_399), "2000-02-29T23:59:59Z");
        assert!(days_ago(CLOSED_DAYS) < days_ago(0));
    }

    #[test]
    fn branches_start_with_the_key_or_number() {
        let issues = parse_gh(GH, true).unwrap();
        assert_eq!(issues[0].branch(), "5-phase-4-issues-panel");
        let jira = parse_acli(ACLI, None).unwrap();
        assert_eq!(jira[0].branch(), "ORD-3479-cache-tariff-lookups");
        let mut long = jira[0].clone();
        long.title = "a ".repeat(30) + "end";
        assert!(long.branch().len() <= "ORD-3479".len() + 41);
    }

    /// The triage label scheme docs/design.md shows, for this repo.
    pub const SCHEME: &str = r#"
        [tracker.github]
        repos = ["remigourdon/atelier"]

        [tracker]
        hide = { labels = ["wontfix", "duplicate"] }

        [[tracker.sections]]
        title = "Blocked"
        blocked = true

        [[tracker.sections]]
        title = "Ready for agent"
        labels = ["ready-for-agent"]

        [[tracker.sections]]
        title = "Triage"
        labels = ["needs-triage", "needs-info"]

        [[tracker.sections]]
        title = "Backlog"
    "#;

    pub fn issue(key: &str, labels: &[&str], blocked: bool) -> Issue {
        Issue {
            tracker: Tracker::GitHub,
            key: key.into(),
            title: format!("Issue {key}"),
            url: Some(format!("https://forge/api/issues/{key}")),
            project: "org/api".into(),
            project_url: Some("https://forge/api".into()),
            state: State::Todo,
            status: "open".into(),
            labels: labels.iter().map(|&label| label.into()).collect(),
            blocked,
            assignees: Vec::new(),
            kind: None,
            priority: None,
            updated_at: String::new(),
        }
    }

    fn config(text: &str) -> TrackerConfig {
        crate::config::Config::parse(text).unwrap().tracker
    }

    #[test]
    fn sections_take_issues_in_order_and_hide_wins() {
        let tracker = config(SCHEME);
        let titles: Vec<&str> = (tracker.sections().iter())
            .map(|section| section.title.as_str())
            .collect();
        assert_eq!(titles, ["Blocked", "Ready for agent", "Triage", "Backlog"]);
        let section = |labels: &[&str], blocked| tracker.section(&issue("a#1", labels, blocked));
        assert_eq!(
            section(&["ready-for-agent"], true),
            Some(0),
            "first match wins"
        );
        assert_eq!(
            section(&["Ready-For-Agent"], false),
            Some(1),
            "labels ignore case"
        );
        assert_eq!(
            section(&["needs-info"], false),
            Some(2),
            "any of the labels"
        );
        assert_eq!(section(&["enhancement"], false), Some(3), "the catch-all");
        assert_eq!(
            section(&["ready-for-agent", "wontfix"], false),
            None,
            "hidden"
        );
    }

    #[test]
    fn rules_combine_their_conditions_and_the_rest_go_to_other() {
        let tracker = config(
            "[[tracker.sections]]\ntitle = \"Open features\"\nlabels = [\"enhancement\"]\n\
             not_labels = [\"question\"]\nstate = [\"todo\", \"in_progress\"]\n",
        );
        let mut feature = issue("a#1", &["enhancement"], false);
        assert_eq!(tracker.section(&feature), Some(0));
        feature.state = State::Done;
        assert_eq!(tracker.section(&feature), Some(1), "not one of its states");
        assert_eq!(tracker.title(1), OTHER);
        let question = issue("a#2", &["enhancement", "question"], false);
        assert_eq!(tracker.section(&question), Some(1), "one of its not_labels");
    }

    #[test]
    fn without_sections_there_is_one_per_state_and_an_empty_hide_hides_nothing() {
        let tracker = config("[tracker]\nhide = {}\n");
        let titles: Vec<&str> = (tracker.sections().iter())
            .map(|section| section.title.as_str())
            .collect();
        assert_eq!(titles, ["To do", "In progress", "Done"]);
        let mut issue = issue("ORD-1", &[], false);
        issue.state = State::InProgress;
        assert_eq!(tracker.section(&issue), Some(1));
        assert!(tracker.scopes().is_empty(), "no tracker configured");
        assert!(
            crate::config::Config::parse("[[tracker.sections]]\nstate = [\"later\"]\n").is_err()
        );
    }

    #[test]
    fn scopes_list_github_repos_and_the_jira_search() {
        let tracker = config(
            "[tracker.github]\nrepos = [\"o/a\", \"o/b\", \"p/a\"]\n\
             [tracker.jira]\njql = \"assignee = currentUser()\"\nurl = \"https://j\"\n",
        );
        assert_eq!(
            tracker.scopes(),
            [
                (
                    Tracker::GitHub,
                    vec!["o/a".into(), "o/b".into(), "p/a".into()]
                ),
                (Tracker::Jira, vec!["assignee = currentUser()".to_owned()]),
            ]
        );
        assert!(tracker.qualified("o/a") && tracker.qualified("p/a"));
        assert!(
            !tracker.qualified("o/b"),
            "only repos sharing a name carry the owner"
        );
        assert_eq!(tracker.jira_site().as_deref(), Some("https://j"));
    }

    /// Issues that count their calls and fail when told to.
    struct Counting {
        calls: Cell<usize>,
        fail: bool,
    }

    impl Issues for Counting {
        fn tracker(&self) -> Tracker {
            Tracker::GitHub
        }

        fn scope(&self) -> &str {
            "o/r"
        }

        fn issues(&self) -> Result<Vec<Issue>> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                return Err(eyre!("offline"));
            }
            parse_gh(GH, false)
        }
    }

    fn state() -> crate::state::State {
        crate::state::State::from_connection(Connection::open_in_memory().unwrap(), "default")
            .unwrap()
    }

    #[test]
    fn fetch_serves_fresh_cache_and_falls_back_to_a_stale_one() {
        let state = state();
        let ok = Counting {
            calls: Cell::new(0),
            fail: false,
        };
        let (issues, error) = fetch(&state, &ok, false);
        assert_eq!((issues.len(), error.is_none()), (3, true));
        fetch(&state, &ok, false);
        assert_eq!(ok.calls.get(), 1, "served from the cache");
        fetch(&state, &ok, true);
        assert_eq!(ok.calls.get(), 2, "forced past it");
        let failing = Counting {
            calls: Cell::new(0),
            fail: true,
        };
        let (issues, error) = fetch(&state, &failing, true);
        assert_eq!(issues.len(), 3);
        let error = format!("{:#}", error.unwrap());
        assert!(error.contains("gh issues in o/r") && error.contains("offline"));
        let (issues, error) = fetch(&self::state(), &failing, false);
        assert!(issues.is_empty() && error.is_some());
    }
}
