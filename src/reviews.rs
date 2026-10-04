//! Open reviews from GitHub (`gh`) and GitLab (`glab`), behind a trait so native APIs can come later.

use color_eyre::eyre::{Report, Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::process::Runner;
use crate::state::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Provider {
    GitHub,
    GitLab,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::GitHub, Provider::GitLab];

    /// The provider worktrunk names for a repo's remote.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "github" => Some(Provider::GitHub),
            "gitlab" => Some(Provider::GitLab),
            _ => None,
        }
    }

    /// The CLI it is reached through, which also names its cache and loading indicator.
    pub fn cli(self) -> &'static str {
        match self {
            Provider::GitHub => "gh",
            Provider::GitLab => "glab",
        }
    }

    /// Its reviews on `host`, through its CLI.
    pub fn reviews<'a>(self, runner: &'a dyn Runner, host: String) -> Box<dyn Reviews + 'a> {
        match self {
            Provider::GitHub => Box::new(Gh { runner, host }),
            Provider::GitLab => Box::new(Glab { runner, host }),
        }
    }

    /// worktrunk's shortcut for a review's branch: `pr:12` or `mr:12`.
    pub fn shortcut(self, number: u64) -> String {
        match self {
            Provider::GitHub => format!("pr:{number}"),
            Provider::GitLab => format!("mr:{number}"),
        }
    }

    /// How the provider writes a review's number: `#12` or `!12`.
    pub fn reference(self, number: u64) -> String {
        match self {
            Provider::GitHub => format!("#{number}"),
            Provider::GitLab => format!("!{number}"),
        }
    }
}

/// Whose review it is: one I am asked to review, or one I wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    ToReview,
    Mine,
}

impl Role {
    pub const ALL: [Role; 2] = [Role::ToReview, Role::Mine];

    fn key(self) -> &'static str {
        match self {
            Role::ToReview => "to-review",
            Role::Mine => "mine",
        }
    }
}

/// An open pull or merge request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub provider: Provider,
    pub role: Role,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `owner/repo`, or a GitLab project's full path.
    pub project: String,
    /// The project's web page, as worktrunk reports a registered repo's forge.
    pub project_url: String,
    pub author: String,
    pub branch: String,
    pub base: String,
    pub draft: bool,
    pub updated_at: String,
}

pub trait Reviews {
    fn provider(&self) -> Provider;

    /// The host it talks to, such as `github.com`.
    fn host(&self) -> &str;

    /// My open reviews in `role`, across every project on the host.
    fn reviews(&self, role: Role) -> Result<Vec<Review>>;
}

/// GitHub through `gh api graphql`, whose search spans every repo and reports the head branch.
pub struct Gh<'a> {
    pub runner: &'a dyn Runner,
    pub host: String,
}

/// `--paginate` pages through it by `$endCursor` and `pageInfo`.
const GH_QUERY: &str = "query($q: String!, $endCursor: String) { \
    search(query: $q, type: ISSUE, first: 100, after: $endCursor) { \
    nodes { ... on PullRequest { number title url isDraft updatedAt headRefName baseRefName \
    author { login } repository { nameWithOwner url } } } \
    pageInfo { hasNextPage endCursor } } }";

impl Reviews for Gh<'_> {
    fn provider(&self) -> Provider {
        Provider::GitHub
    }

    fn host(&self) -> &str {
        &self.host
    }

    fn reviews(&self, role: Role) -> Result<Vec<Review>> {
        let who = match role {
            Role::ToReview => "review-requested:@me",
            Role::Mine => "author:@me",
        };
        let query = format!("query={GH_QUERY}");
        let search = format!("q=is:pr is:open archived:false {who}");
        let json = self.runner.output(
            "gh",
            &[
                "api",
                "--hostname",
                &self.host,
                "graphql",
                "--paginate",
                "--slurp",
                "-f",
                &query,
                "-f",
                &search,
            ],
        )?;
        parse_gh(&json, role)
    }
}

/// GitLab through `glab api`, whose `/merge_requests` spans every project. Its fields are the ones
/// the Python prototype read from a live GitLab.
pub struct Glab<'a> {
    pub runner: &'a dyn Runner,
    pub host: String,
}

impl Glab<'_> {
    fn api(&self, endpoint: &str) -> Result<String> {
        self.runner.output(
            "glab",
            &["api", "--paginate", "--hostname", &self.host, endpoint],
        )
    }
}

impl Reviews for Glab<'_> {
    fn provider(&self) -> Provider {
        Provider::GitLab
    }

    fn host(&self) -> &str {
        &self.host
    }

    fn reviews(&self, role: Role) -> Result<Vec<Review>> {
        let scope = match role {
            Role::ToReview => "reviews_for_me",
            Role::Mine => "created_by_me",
        };
        let json = self.api(&format!(
            "/merge_requests?state=opened&scope={scope}&per_page=100"
        ))?;
        parse_glab(&json, role)
    }
}

/// Parses the pages of `gh api graphql`'s search, skipping nodes that are not pull requests.
pub fn parse_gh(json: &str, role: Role) -> Result<Vec<Review>> {
    let pages: Vec<raw::GhResponse> = serde_json::from_str(json).wrap_err("parsing gh reviews")?;
    Ok(pages
        .into_iter()
        .flat_map(|page| page.data.search.nodes)
        .filter_map(|node| {
            Some(Review {
                provider: Provider::GitHub,
                role,
                number: node.number?,
                title: node.title,
                url: node.url,
                project: node.repository.name_with_owner,
                project_url: node.repository.url,
                author: node.author.map(|author| author.login).unwrap_or_default(),
                branch: node.head_ref_name,
                base: node.base_ref_name,
                draft: node.is_draft,
                updated_at: node.updated_at,
            })
        })
        .collect())
}

/// Parses `glab api merge_requests`.
pub fn parse_glab(json: &str, role: Role) -> Result<Vec<Review>> {
    let raw: Vec<raw::GlabMergeRequest> =
        serde_json::from_str(json).wrap_err("parsing glab reviews")?;
    Ok(raw
        .into_iter()
        .map(|mr| {
            let project_url = mr
                .web_url
                .split_once("/-/merge_requests/")
                .map_or(mr.web_url.as_str(), |(project, _)| project)
                .to_owned();
            let project = match mr.references.full.rsplit_once('!') {
                Some((project, _)) => project.to_owned(),
                None => crate::worktrunk::host(&project_url)
                    .and_then(|host| project_url.split_once(host))
                    .map_or(String::new(), |(_, path)| path.trim_matches('/').to_owned()),
            };
            // The draft flag is shown on its own.
            let title = match mr.title.strip_prefix("Draft: ") {
                Some(title) if mr.draft => title.to_owned(),
                _ => mr.title,
            };
            Review {
                provider: Provider::GitLab,
                role,
                number: mr.iid,
                title,
                url: mr.web_url,
                project,
                project_url,
                author: mr.author.username,
                branch: mr.source_branch,
                base: mr.target_branch,
                draft: mr.draft,
                updated_at: mr.updated_at,
            }
        })
        .collect())
}

/// My reviews in `role` through the cache, as [`State::fetch_cached`] serves them.
pub fn fetch(
    state: &State,
    api: &dyn Reviews,
    role: Role,
    force: bool,
) -> (Vec<Review>, Option<Report>) {
    let source = api.provider().cli();
    let key = format!("{} {}", api.host(), role.key());
    let (reviews, error) = state.fetch_cached(source, &key, force, || api.reviews(role));
    let error = error.map(|err| err.wrap_err(format!("{source} reviews on {}", api.host())));
    (reviews, error)
}

/// The subset of each provider's JSON that atelier reads.
mod raw {
    use super::Deserialize;

    #[derive(Deserialize)]
    pub struct GhResponse {
        pub data: GhData,
    }

    #[derive(Deserialize)]
    pub struct GhData {
        pub search: GhSearch,
    }

    #[derive(Deserialize)]
    pub struct GhSearch {
        #[serde(default)]
        pub nodes: Vec<GhPull>,
    }

    /// A search node; one that is not a pull request has no fields.
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    pub struct GhPull {
        pub number: Option<u64>,
        pub title: String,
        pub url: String,
        pub is_draft: bool,
        pub updated_at: String,
        pub head_ref_name: String,
        pub base_ref_name: String,
        /// `None` for a deleted account.
        pub author: Option<GhAuthor>,
        pub repository: GhRepository,
    }

    #[derive(Deserialize)]
    pub struct GhAuthor {
        pub login: String,
    }

    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    pub struct GhRepository {
        pub name_with_owner: String,
        pub url: String,
    }

    #[derive(Deserialize)]
    pub struct GlabMergeRequest {
        pub iid: u64,
        #[serde(default)]
        pub title: String,
        pub web_url: String,
        #[serde(default)]
        pub draft: bool,
        #[serde(default)]
        pub updated_at: String,
        #[serde(default)]
        pub source_branch: String,
        #[serde(default)]
        pub target_branch: String,
        #[serde(default)]
        pub author: GlabAuthor,
        #[serde(default)]
        pub references: GlabReferences,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct GlabAuthor {
        pub username: String,
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub struct GlabReferences {
        pub full: String,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use rusqlite::Connection;

    use color_eyre::eyre::eyre;

    use super::*;
    use crate::process::fake::Fake;

    const GH: &str = include_str!("../tests/fixtures/gh-reviews.json");
    const GLAB: &str = include_str!("../tests/fixtures/glab-reviews.json");

    #[test]
    fn gh_search_parses_every_page_of_pull_requests_only() {
        let reviews = parse_gh(GH, Role::Mine).unwrap();
        let numbers: Vec<u64> = reviews.iter().map(|review| review.number).collect();
        assert_eq!(
            numbers,
            [10, 9, 8],
            "two pages; the empty node is no pull request"
        );
        let first = &reviews[0];
        assert_eq!(first.number, 10);
        assert_eq!(first.project, "remigourdon/atelier");
        assert_eq!(first.project_url, "https://github.com/remigourdon/atelier");
        assert_eq!(
            (first.branch.as_str(), first.base.as_str()),
            ("phase-2-tui", "main")
        );
        assert_eq!(first.author, "remigourdon");
        assert_eq!(first.role, Role::Mine);
        assert!(!first.draft && reviews[2].draft);
        assert!(parse_gh("{\"errors\":[]}", Role::Mine).is_err());
    }

    #[test]
    fn glab_merge_requests_parse_with_their_project() {
        let reviews = parse_glab(GLAB, Role::ToReview).unwrap();
        let first = &reviews[0];
        assert_eq!(first.provider, Provider::GitLab);
        assert_eq!(first.number, 42);
        assert_eq!(first.project, "billing/core/api");
        assert_eq!(
            first.project_url,
            "https://gitlab.example.com/billing/core/api"
        );
        assert_eq!(first.branch, "ORD-3479-cache-tariffs");
        assert_eq!(first.author, "alice");
        assert!(first.draft);
        assert_eq!(first.title, "ORD-3479 Cache tariff lookups");
        assert_eq!(reviews[1].base, "develop");
        assert_eq!(reviews[1].project, "billing/web");
    }

    #[test]
    fn gh_searches_the_host_for_my_role() {
        let fake = Fake::default().always("gh", Some(GH));
        let gh = Gh {
            runner: &fake,
            host: "github.com".into(),
        };
        assert_eq!(gh.reviews(Role::ToReview).unwrap().len(), 3);
        let call = &fake.calls()[0];
        assert!(
            call.starts_with("gh api --hostname github.com graphql --paginate --slurp -f query=")
        );
        assert!(call.ends_with("-f q=is:pr is:open archived:false review-requested:@me"));
    }

    #[test]
    fn glab_lists_merge_requests_by_scope() {
        let fake = Fake::default().always("glab api --paginate --hostname h", Some(GLAB));
        let glab = Glab {
            runner: &fake,
            host: "h".into(),
        };
        glab.reviews(Role::ToReview).unwrap();
        glab.reviews(Role::Mine).unwrap();
        assert_eq!(
            fake.calls(),
            [
                "glab api --paginate --hostname h /merge_requests?state=opened&scope=reviews_for_me&per_page=100",
                "glab api --paginate --hostname h /merge_requests?state=opened&scope=created_by_me&per_page=100",
            ]
        );
    }

    /// Reviews that count their calls and fail when told to.
    struct Counting {
        calls: Cell<usize>,
        fail: bool,
    }

    impl Reviews for Counting {
        fn provider(&self) -> Provider {
            Provider::GitHub
        }

        fn host(&self) -> &str {
            "github.com"
        }

        fn reviews(&self, role: Role) -> Result<Vec<Review>> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                return Err(eyre!("offline"));
            }
            parse_gh(GH, role)
        }
    }

    fn state() -> State {
        State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap()
    }

    #[test]
    fn fetch_serves_fresh_cache_and_refetches_when_forced() {
        let state = state();
        let api = Counting {
            calls: Cell::new(0),
            fail: false,
        };
        let (reviews, error) = fetch(&state, &api, Role::Mine, false);
        assert_eq!((reviews.len(), error.is_none()), (3, true));
        fetch(&state, &api, Role::Mine, false);
        assert_eq!(api.calls.get(), 1, "served from the cache");
        fetch(&state, &api, Role::ToReview, false);
        assert_eq!(api.calls.get(), 2, "each role has its own entry");
        fetch(&state, &api, Role::Mine, true);
        assert_eq!(api.calls.get(), 3);
    }

    #[test]
    fn a_failed_fetch_falls_back_to_a_stale_cache() {
        let state = state();
        let (reviews, error) = fetch(
            &state,
            &Counting {
                calls: Cell::new(0),
                fail: true,
            },
            Role::Mine,
            false,
        );
        assert!(reviews.is_empty());
        let error = format!("{:#}", error.unwrap());
        assert!(error.contains("gh reviews on github.com") && error.contains("offline"));
        let ok = Counting {
            calls: Cell::new(0),
            fail: false,
        };
        fetch(&state, &ok, Role::Mine, false);
        let failing = Counting {
            calls: Cell::new(0),
            fail: true,
        };
        let (reviews, error) = fetch(&state, &failing, Role::Mine, true);
        assert_eq!(reviews.len(), 3);
        assert!(error.is_some());
    }
}
