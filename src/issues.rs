//! Issues from GitHub (`gh`) and Jira (`acli`), normalised to a state, labels and whether they
//! are blocked, and placed in sections by ordered rules. Behind a trait so native APIs can come
//! later.

use std::sync::LazyLock;

use color_eyre::eyre::{Report, Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::process::Runner;

/// How long a fetched list of issues is served from the cache, as for reviews.
pub const CACHE_SECS: u64 = crate::reviews::CACHE_SECS;

/// Where issues come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Source {
    GitHub,
    Jira,
}

impl Source {
    /// The CLI it is reached through, which also names its cache and loading indicator.
    pub fn cli(self) -> &'static str {
        match self {
            Source::GitHub => "gh",
            Source::Jira => "acli",
        }
    }

    /// Its issues in `scope`, a GitHub repo or a JQL search, through its CLI. `site` is Jira's
    /// web address for issue links.
    pub fn issues<'a>(
        self,
        runner: &'a dyn Runner,
        scope: String,
        site: Option<String>,
    ) -> Box<dyn Issues + 'a> {
        match self {
            Source::GitHub => Box::new(Gh {
                runner,
                repo: scope,
            }),
            Source::Jira => Box::new(Acli {
                runner,
                jql: scope,
                site,
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
    pub source: Source,
    /// `ABC-123`, or `repo#12` on GitHub. It is also the group of the issue's linked work.
    pub key: String,
    pub title: String,
    pub url: String,
    /// `owner/repo`, or a Jira project key.
    pub project: String,
    /// The repo's web page on GitHub, to find its registered repo; empty for Jira.
    pub project_url: String,
    pub state: State,
    /// The tracker's own status name.
    pub status: String,
    pub labels: Vec<String>,
    pub blocked: bool,
    pub assignees: Vec<String>,
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
    fn source(&self) -> Source;

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
pub struct Tracker {
    pub github: Option<GitHubTracker>,
    pub jira: Option<JiraTracker>,
    /// Issues it matches are never listed.
    pub hide: Option<Rule>,
    sections: Vec<Section>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GitHubTracker {
    /// `owner/name`, each listed for its open issues.
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JiraTracker {
    pub jql: String,
    /// The site issue links point to, else `$ATLASSIAN_URL`, else `$JIRA_URL`.
    pub url: Option<String>,
}

impl Tracker {
    /// The configured sections, else one per state.
    pub fn sections(&self) -> &[Section] {
        if self.sections.is_empty() {
            &BY_STATE
        } else {
            &self.sections
        }
    }

    /// The section an issue is listed in: the first whose rule matches, unless it is hidden.
    pub fn section(&self, issue: &Issue) -> Option<usize> {
        if self.hide.as_ref().is_some_and(|hide| hide.matches(issue)) {
            return None;
        }
        self.sections()
            .iter()
            .position(|section| section.rule.matches(issue))
    }

    /// What each source lists: GitHub repos, or one JQL search.
    pub fn scopes(&self) -> Vec<(Source, Vec<String>)> {
        let mut scopes = Vec::new();
        if let Some(github) = self.github.as_ref().filter(|gh| !gh.repos.is_empty()) {
            scopes.push((Source::GitHub, github.repos.clone()));
        }
        if let Some(jira) = self.jira.as_ref().filter(|jira| !jira.jql.is_empty()) {
            scopes.push((Source::Jira, vec![jira.jql.clone()]));
        }
        scopes
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

/// GitHub through `gh api graphql`, one repo's open issues, most recently updated first.
pub struct Gh<'a> {
    pub runner: &'a dyn Runner,
    /// `owner/name`.
    pub repo: String,
}

/// `--paginate` pages through it by `$endCursor` and `pageInfo`. `blockedBy` counts open
/// blockers only.
const GH_QUERY: &str = "query($owner: String!, $name: String!, $endCursor: String) { \
    repository(owner: $owner, name: $name) { nameWithOwner url \
    issues(states: OPEN, first: 100, after: $endCursor, orderBy: {field: UPDATED_AT, direction: DESC}) { \
    nodes { number title url updatedAt assignees(first: 100) { nodes { login } } \
    labels(first: 100) { nodes { name } } issueDependenciesSummary { blockedBy } } \
    pageInfo { hasNextPage endCursor } } } }";

impl Issues for Gh<'_> {
    fn source(&self) -> Source {
        Source::GitHub
    }

    fn scope(&self) -> &str {
        &self.repo
    }

    fn issues(&self) -> Result<Vec<Issue>> {
        let (owner, name) = self
            .repo
            .split_once('/')
            .ok_or_else(|| color_eyre::eyre::eyre!("{} is not owner/name", self.repo))?;
        let json = self.runner.output(
            "gh",
            &[
                "api",
                "graphql",
                "--paginate",
                "--slurp",
                "-f",
                &format!("query={GH_QUERY}"),
                "-f",
                &format!("owner={owner}"),
                "-f",
                &format!("name={name}"),
            ],
        )?;
        parse_gh(&json)
    }
}

/// Jira through `acli jira workitem search`, every result of a JQL search in its order.
pub struct Acli<'a> {
    pub runner: &'a dyn Runner,
    pub jql: String,
    /// The site issue links point to; else it is read from each issue's API address.
    pub site: Option<String>,
}

impl Issues for Acli<'_> {
    fn source(&self) -> Source {
        Source::Jira
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
                "key,summary,status,labels,assignee,updated",
                "--json",
                "--paginate",
            ],
        )?;
        parse_acli(&json, self.site.as_deref())
    }
}

/// Parses the pages of `gh api graphql`'s repository issues. Every open issue is `todo`.
pub fn parse_gh(json: &str) -> Result<Vec<Issue>> {
    let pages: Vec<raw::GhResponse> = serde_json::from_str(json).wrap_err("parsing gh issues")?;
    Ok(pages
        .into_iter()
        .flat_map(|page| {
            let repo = page.data.repository;
            let name = (repo.name_with_owner.rsplit('/').next())
                .unwrap_or_default()
                .to_owned();
            repo.issues.nodes.into_iter().map(move |node| Issue {
                source: Source::GitHub,
                key: format!("{name}#{}", node.number),
                title: node.title,
                url: node.url,
                project: repo.name_with_owner.clone(),
                project_url: repo.url.clone(),
                state: State::Todo,
                status: "open".into(),
                labels: node.labels.nodes.into_iter().map(|l| l.name).collect(),
                blocked: node.issue_dependencies_summary.blocked_by > 0,
                assignees: node.assignees.nodes.into_iter().map(|a| a.login).collect(),
                updated_at: node.updated_at,
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
                source: Source::Jira,
                url: site.map_or_else(String::new, |site| {
                    format!("{}/browse/{}", site.trim_end_matches('/'), issue.key)
                }),
                project: (issue.key.split_once('-'))
                    .map_or(issue.key.clone(), |(project, _)| project.to_owned()),
                key: issue.key,
                title: fields.summary,
                project_url: String::new(),
                state,
                blocked: fields.status.name.eq_ignore_ascii_case("blocked"),
                status: fields.status.name,
                labels: fields.labels,
                assignees: fields
                    .assignee
                    .map(|a| a.display_name)
                    .into_iter()
                    .collect(),
                updated_at: fields.updated,
            }
        })
        .collect())
}

/// The issues in `api`'s scope from the cache while fresh (unless `force`), else from the
/// tracker. A failed fetch falls back to the cache at any age and also returns the error.
pub fn fetch(
    state: &crate::state::State,
    api: &dyn Issues,
    force: bool,
) -> (Vec<Issue>, Option<Report>) {
    let source = api.source().cli();
    let key = format!("issues {}", api.scope());
    let cached = |max_age| -> Option<Vec<Issue>> {
        let json = state.cached(source, &key, max_age).ok()??;
        serde_json::from_str(&json).ok()
    };
    if !force && let Some(issues) = cached(Some(CACHE_SECS)) {
        return (issues, None);
    }
    let fetched = api.issues().and_then(|issues| {
        state.store_cache(source, &key, &serde_json::to_string(&issues)?)?;
        Ok(issues)
    });
    match fetched {
        Ok(issues) => (issues, None),
        Err(err) => (
            cached(None).unwrap_or_default(),
            Some(err.wrap_err(format!("{source} issues in {}", api.scope()))),
        ),
    }
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
    pub struct GhConnection<T> {
        #[serde(default = "Vec::new")]
        pub nodes: Vec<T>,
    }

    impl<T> Default for GhConnection<T> {
        fn default() -> Self {
            Self { nodes: Vec::new() }
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct GhIssue {
        pub number: u64,
        #[serde(default)]
        pub title: String,
        pub url: String,
        #[serde(default)]
        pub updated_at: String,
        #[serde(default)]
        pub assignees: GhConnection<GhUser>,
        #[serde(default)]
        pub labels: GhConnection<GhLabel>,
        #[serde(default)]
        pub issue_dependencies_summary: GhDependencies,
    }

    #[derive(Deserialize)]
    pub struct GhUser {
        pub login: String,
    }

    #[derive(Deserialize)]
    pub struct GhLabel {
        pub name: String,
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

    #[derive(Deserialize)]
    #[serde(default)]
    #[derive(Default)]
    pub struct JiraFields {
        pub summary: String,
        pub labels: Vec<String>,
        pub updated: String,
        /// `null` when unassigned.
        pub assignee: Option<JiraUser>,
        pub status: JiraStatus,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct JiraUser {
        pub display_name: String,
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

    use color_eyre::eyre::eyre;
    use rusqlite::Connection;

    use super::*;
    use crate::process::fake::Fake;

    pub const GH: &str = include_str!("../tests/fixtures/gh-issues.json");
    const ACLI: &str = include_str!("../tests/fixtures/acli-issues.json");

    #[test]
    fn gh_issues_parse_every_page_with_labels_and_blockers() {
        let issues = parse_gh(GH).unwrap();
        let keys: Vec<&str> = issues.iter().map(|issue| issue.key.as_str()).collect();
        assert_eq!(keys, ["atelier#5", "atelier#6", "atelier#7"], "two pages");
        let first = &issues[0];
        assert_eq!(first.title, "Phase 4: Issues panel");
        assert_eq!(first.project, "remigourdon/atelier");
        assert_eq!(first.project_url, "https://github.com/remigourdon/atelier");
        assert_eq!(first.url, "https://github.com/remigourdon/atelier/issues/5");
        assert_eq!(first.labels, ["enhancement", "ready-for-agent"]);
        assert_eq!(first.assignees, ["remigourdon"]);
        assert!(issues.iter().all(|issue| issue.state == State::Todo));
        let blocked: Vec<bool> = issues.iter().map(|issue| issue.blocked).collect();
        assert_eq!(blocked, [false, true, false]);
        assert!(parse_gh("{\"errors\":[]}").is_err());
    }

    #[test]
    fn acli_issues_take_their_state_from_the_status_category() {
        let issues = parse_acli(ACLI, None).unwrap();
        let states: Vec<State> = issues.iter().map(|issue| issue.state).collect();
        assert_eq!(states, [State::InProgress, State::Todo, State::Done]);
        let first = &issues[0];
        assert_eq!(first.key, "ORD-3479");
        assert_eq!(first.project, "ORD");
        assert_eq!(first.status, "In Review");
        assert_eq!(first.labels, ["backend", "perf"]);
        assert_eq!(first.assignees, ["Alice Martin"]);
        assert_eq!(first.url, "https://example.atlassian.net/browse/ORD-3479");
        assert!(issues[1].blocked, "in status Blocked");
        assert!(issues[1].assignees.is_empty() && issues[2].assignees.is_empty());
        let issues = parse_acli(ACLI, Some("https://jira.example.com/")).unwrap();
        assert_eq!(issues[0].url, "https://jira.example.com/browse/ORD-3479");
    }

    #[test]
    fn gh_lists_a_repos_issues_and_acli_runs_the_search() {
        let fake = Fake::default().always("gh", Some(GH));
        let gh = Source::GitHub.issues(&fake, "remigourdon/atelier".into(), None);
        assert_eq!(gh.issues().unwrap().len(), 3);
        let call = &fake.calls()[0];
        assert!(call.starts_with("gh api graphql --paginate --slurp -f query="));
        assert!(call.ends_with("-f owner=remigourdon -f name=atelier"));
        assert!(
            Source::GitHub
                .issues(&fake, "atelier".into(), None)
                .issues()
                .is_err()
        );
        let fake = Fake::default().always("acli", Some(ACLI));
        let jira = Source::Jira.issues(&fake, "project = ORD".into(), None);
        assert_eq!(jira.issues().unwrap().len(), 3);
        assert_eq!(
            fake.calls(),
            ["acli jira workitem search --jql project = ORD \
              --fields key,summary,status,labels,assignee,updated --json --paginate"]
        );
    }

    #[test]
    fn branches_start_with_the_key_or_number() {
        let issues = parse_gh(GH).unwrap();
        assert_eq!(issues[0].branch(), "5-phase-4-issues-panel");
        let jira = parse_acli(ACLI, None).unwrap();
        assert_eq!(jira[0].branch(), "ORD-3479-cache-tariff-lookups");
        let mut long = jira[0].clone();
        long.title = "a ".repeat(30) + "end";
        assert!(long.branch().len() <= "ORD-3479".len() + 41);
    }

    /// A label scheme in the style of a triage workflow, as a user would write it.
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
        state = ["todo"]

        [[tracker.sections]]
        title = "Backlog"
        not_labels = ["question"]
    "#;

    pub fn issue(key: &str, labels: &[&str], blocked: bool) -> Issue {
        Issue {
            source: Source::GitHub,
            key: key.into(),
            title: format!("Issue {key}"),
            url: format!("https://forge/api/issues/{key}"),
            project: "org/api".into(),
            project_url: "https://forge/api".into(),
            state: State::Todo,
            status: "open".into(),
            labels: labels.iter().map(|&label| label.into()).collect(),
            blocked,
            assignees: Vec::new(),
            updated_at: String::new(),
        }
    }

    fn tracker(text: &str) -> Tracker {
        crate::config::Config::parse(text).unwrap().tracker
    }

    #[test]
    fn sections_take_issues_in_order_and_hide_wins() {
        let tracker = tracker(SCHEME);
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
        assert_eq!(section(&[], false), Some(3), "the catch-all");
        assert_eq!(section(&["question"], false), None, "matches no section");
        assert_eq!(
            section(&["ready-for-agent", "wontfix"], false),
            None,
            "hidden"
        );
        let mut done = issue("a#2", &["needs-triage"], false);
        done.state = State::Done;
        assert_eq!(tracker.section(&done), Some(3), "triage wants todo");
    }

    #[test]
    fn without_sections_there_is_one_per_state() {
        let tracker = tracker("");
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
        let tracker = tracker(
            "[tracker.github]\nrepos = [\"o/a\", \"o/b\"]\n\
             [tracker.jira]\njql = \"assignee = currentUser()\"\nurl = \"https://j\"\n",
        );
        assert_eq!(
            tracker.scopes(),
            [
                (Source::GitHub, vec!["o/a".to_owned(), "o/b".to_owned()]),
                (Source::Jira, vec!["assignee = currentUser()".to_owned()]),
            ]
        );
        assert_eq!(tracker.jira_site().as_deref(), Some("https://j"));
    }

    /// Issues that count their calls and fail when told to.
    struct Counting {
        calls: Cell<usize>,
        fail: bool,
    }

    impl Issues for Counting {
        fn source(&self) -> Source {
            Source::GitHub
        }

        fn scope(&self) -> &str {
            "o/r"
        }

        fn issues(&self) -> Result<Vec<Issue>> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                return Err(eyre!("offline"));
            }
            parse_gh(GH)
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
