//! Linked work: which items an issue, a group, an item or a review is linked to, answered once
//! for the TUI and the CLI alike from the caller's own items.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::links::{Group, IssueKey, IssueKeys, Links};
use crate::reviews::Review;
use crate::state::Repo;
use crate::worktrunk::{self, Forge};

/// What linked work needs to know of an item.
pub trait Item {
    fn links(&self) -> &Links;
    /// A worktree's repo.
    fn repo(&self) -> Option<&Path>;
    /// A worktree's branch, when it is known and not detached.
    fn branch(&self) -> Option<&str>;
}

/// A view of the caller's items, repos, forges and reviews, built per call: it stores nothing,
/// so nothing goes stale. Answers are the caller's own items.
pub struct LinkedWork<'a, I> {
    items: &'a [I],
    repos: &'a [Repo],
    forges: &'a HashMap<PathBuf, Forge>,
    reviews: &'a [Review],
}

impl<'a, I: Item> LinkedWork<'a, I> {
    pub fn new(
        items: &'a [I],
        repos: &'a [Repo],
        forges: &'a HashMap<PathBuf, Forge>,
        reviews: &'a [Review],
    ) -> Self {
        Self {
            items,
            repos,
            forges,
            reviews,
        }
    }

    /// A view of `items` alone, for the questions about items: no review is placed.
    pub fn over(items: &'a [I]) -> Self {
        static NO_FORGES: LazyLock<HashMap<PathBuf, Forge>> = LazyLock::new(HashMap::new);
        Self::new(items, &[], &NO_FORGES, &[])
    }

    /// An issue's linked work: every item linking `key`, open or closed, in any group.
    pub fn of_issue(&self, key: &IssueKey) -> Vec<&'a I> {
        (self.items.iter())
            .filter(|item| item.links().links(key))
            .collect()
    }

    /// Every group of every item, in every workspace, once each, sorted.
    pub fn groups(&self) -> Vec<Group> {
        let groups: BTreeSet<&Group> = (self.items.iter())
            .filter_map(|item| item.links().group.as_ref())
            .collect();
        groups.into_iter().cloned().collect()
    }

    /// Every item in `group`, in every workspace, closed carnets included.
    pub fn members(&self, group: &Group) -> Vec<&'a I> {
        (self.items.iter())
            .filter(|item| item.links().group.as_ref() == Some(group))
            .collect()
    }

    /// An item's linked work, given its links: its group's members and the linked work of its
    /// issue keys.
    pub fn of_links(&self, links: &Links) -> Vec<&'a I> {
        let in_group = |other: &Links| links.group.is_some() && other.group == links.group;
        (self.items.iter())
            .filter(|item| {
                let other = item.links();
                in_group(other) || other.issue_keys.shares(&links.issue_keys)
            })
            .collect()
    }

    /// The one group among the items linking any of `keys`; items in no group do not count.
    pub fn linked_group(&self, keys: &IssueKeys) -> Option<Group> {
        let mut groups: BTreeSet<&Group> = (self.items.iter())
            .filter(|item| item.links().issue_keys.shares(keys))
            .filter_map(|item| item.links().group.as_ref())
            .collect();
        match groups.len() {
            1 => groups.pop_first().cloned(),
            _ => None,
        }
    }

    /// The registered repo whose forge web page is `project_url`.
    pub fn project_repo(&self, project_url: &str) -> Option<&'a Repo> {
        (self.repos.iter()).find(|repo| self.is_project(&repo.path, project_url))
    }

    /// A review's worktree: the one on its branch in a repo whose forge is its project.
    pub fn review_worktree(&self, review: &Review) -> Option<&'a I> {
        (self.items.iter()).find(|item| {
            item.branch() == Some(review.branch.as_str())
                && (item.repo()).is_some_and(|repo| self.is_project(repo, &review.project_url))
        })
    }

    /// A review's group: its worktree's.
    pub fn review_group(&self, review: &Review) -> Option<&'a Group> {
        self.review_worktree(review)?.links().group.as_ref()
    }

    /// The reviews linking any of `keys`, each once.
    pub fn reviews_linking(&self, keys: &IssueKeys) -> Vec<&'a Review> {
        let mut seen = HashSet::new();
        (self.reviews.iter())
            .filter(|review| review.issue_keys.shares(keys) && seen.insert(&review.url))
            .collect()
    }

    /// The worktree on `branch` in `repo`.
    pub fn worktree_on(&self, repo: &Path, branch: &str) -> Option<&'a I> {
        (self.items.iter()).find(|item| item.repo() == Some(repo) && item.branch() == Some(branch))
    }

    /// Whether `repo`'s forge web page is `project_url`.
    fn is_project(&self, repo: &Path, project_url: &str) -> bool {
        (self.forges.get(repo))
            .is_some_and(|forge| worktrunk::same_project(&forge.url, project_url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::tests::{group, key, keys, links};
    use crate::reviews::tests::review;
    use crate::reviews::{Provider, Role};

    /// A worktree, with a repo and a branch, or a carnet, with neither.
    struct TestItem {
        path: PathBuf,
        links: Links,
        repo: Option<PathBuf>,
        branch: Option<String>,
    }

    impl Item for TestItem {
        fn links(&self) -> &Links {
            &self.links
        }
        fn repo(&self) -> Option<&Path> {
            self.repo.as_deref()
        }
        fn branch(&self) -> Option<&str> {
            self.branch.as_deref()
        }
    }

    fn tree(repo: &str, branch: &str, links: Links) -> TestItem {
        TestItem {
            path: format!("/src/{repo}.{branch}").into(),
            links,
            repo: Some(format!("/src/{repo}").into()),
            branch: Some(branch.into()),
        }
    }

    /// Open or closed alike: linked work never asks.
    fn carnet(name: &str, links: Links) -> TestItem {
        TestItem {
            path: format!("/data/{name}").into(),
            links,
            repo: None,
            branch: None,
        }
    }

    const API: &str = "https://forge/org/api";
    const WEB: &str = "https://forge/org/web";

    /// LOGIN: two api worktrees, one linking ABC-1, and a closed carnet linking ORD-7. ABC-1 is
    /// also linked by a web worktree in no group and an open carnet in group NOTES. `web` is
    /// another project's repo with a worktree on `change-2`.
    fn items() -> Vec<TestItem> {
        vec![
            tree("api", "login", links("LOGIN", &["ABC-1"])),
            tree("api", "change-2", links("LOGIN", &[])),
            tree("web", "change-2", links("", &["ABC-1", "XYZ-9"])),
            tree("web", "solo", links("", &[])),
            carnet("2026-10-02-notes", links("NOTES", &["ABC-1"])),
            carnet("2026-09-01-old", links("LOGIN", &["ORD-7"])),
        ]
    }

    fn repo(name: &str) -> Repo {
        Repo {
            path: format!("/src/{name}").into(),
            alias: None,
            default_workspace: "default".into(),
        }
    }

    struct Fixture {
        items: Vec<TestItem>,
        repos: Vec<Repo>,
        forges: HashMap<PathBuf, Forge>,
        reviews: Vec<Review>,
    }

    impl Fixture {
        fn new() -> Self {
            let forge = |url: &str| Forge {
                url: url.into(),
                provider: "github".into(),
            };
            let mut linking = review(Provider::GitHub, Role::ToReview, 2, API);
            linking.issue_keys = keys(&["ABC-1"]);
            // The same review as listed in another role.
            let mut again = linking.clone();
            again.role = Role::Mine;
            let mut other = review(Provider::GitHub, Role::Mine, 3, "https://forge/org/gone");
            other.issue_keys = keys(&["XYZ-9"]);
            Self {
                items: items(),
                repos: vec![repo("api"), repo("web")],
                forges: [
                    ("/src/api".into(), forge(&format!("{API}.git"))),
                    ("/src/web".into(), forge(WEB)),
                ]
                .into(),
                reviews: vec![linking, again, other],
            }
        }

        fn linked(&self) -> LinkedWork<'_, TestItem> {
            LinkedWork::new(&self.items, &self.repos, &self.forges, &self.reviews)
        }
    }

    fn paths(items: Vec<&TestItem>) -> Vec<String> {
        (items.iter())
            .map(|item| item.path.display().to_string())
            .collect()
    }

    #[test]
    fn an_issues_linked_work_is_every_item_linking_its_key_in_any_group() {
        let fixture = Fixture::new();
        let linked = fixture.linked();
        assert_eq!(
            paths(linked.of_issue(&key("ABC-1"))),
            [
                "/src/api.login",
                "/src/web.change-2",
                "/data/2026-10-02-notes"
            ],
        );
        assert_eq!(
            paths(linked.of_issue(&key("ORD-7"))),
            ["/data/2026-09-01-old"],
            "a closed carnet too"
        );
        assert!(linked.of_issue(&key("NONE-1")).is_empty());
    }

    #[test]
    fn groups_and_their_members_span_every_item() {
        let fixture = Fixture::new();
        let linked = fixture.linked();
        assert_eq!(
            linked.groups(),
            [group("LOGIN").unwrap(), group("NOTES").unwrap()]
        );
        assert_eq!(
            paths(linked.members(&group("LOGIN").unwrap())),
            [
                "/src/api.login",
                "/src/api.change-2",
                "/data/2026-09-01-old"
            ],
            "a closed carnet too"
        );
    }

    #[test]
    fn an_items_linked_work_is_its_group_and_its_keys_linked_work() {
        let fixture = Fixture::new();
        let linked = fixture.linked();
        assert_eq!(
            paths(linked.of_links(&links("LOGIN", &["XYZ-9"]))),
            [
                "/src/api.login",
                "/src/api.change-2",
                "/src/web.change-2",
                "/data/2026-09-01-old"
            ],
        );
        assert_eq!(
            paths(linked.of_links(&links("", &["ORD-7"]))),
            ["/data/2026-09-01-old"],
            "in no group, only its keys"
        );
        assert!(
            linked.of_links(&links("", &[])).is_empty(),
            "items in no group are not each other's linked work"
        );
    }

    #[test]
    fn the_linked_group_is_the_one_group_among_the_items_linking_the_keys() {
        let fixture = Fixture::new();
        let linked = fixture.linked();
        assert_eq!(linked.linked_group(&keys(&["ORD-7"])), group("LOGIN"));
        assert_eq!(
            linked.linked_group(&keys(&["XYZ-9"])),
            None,
            "items in no group do not count"
        );
        assert_eq!(
            linked.linked_group(&keys(&["ABC-1"])),
            None,
            "LOGIN and NOTES"
        );
        assert_eq!(linked.linked_group(&keys(&[])), None);
    }

    #[test]
    fn a_review_is_placed_by_its_project_then_its_branch() {
        let fixture = Fixture::new();
        let linked = fixture.linked();
        let [review, _, other] = &fixture.reviews[..] else {
            panic!();
        };
        assert_eq!(
            linked
                .project_repo(&review.project_url)
                .map(|repo| &repo.path),
            Some(&PathBuf::from("/src/api"))
        );
        assert_eq!(
            (linked.review_worktree(review)).map(|item| item.path.display().to_string()),
            Some("/src/api.change-2".into()),
            "never web's worktree on the same branch"
        );
        assert_eq!(linked.review_group(review), group("LOGIN").as_ref());
        assert!(linked.project_repo(&other.project_url).is_none());
        assert!(linked.review_worktree(other).is_none());
        assert_eq!(linked.review_group(other), None);
    }

    #[test]
    fn a_reviews_worktree_is_none_until_its_branch_is_checked_out() {
        let mut fixture = Fixture::new();
        fixture.items.remove(1);
        let review = &fixture.reviews[0];
        assert!(fixture.linked().review_worktree(review).is_none());
    }

    #[test]
    fn the_reviews_linking_some_keys_come_once_each() {
        let fixture = Fixture::new();
        let linked = fixture.linked();
        let numbers = |texts: &[&str]| -> Vec<u64> {
            (linked.reviews_linking(&keys(texts)).iter())
                .map(|review| review.number)
                .collect()
        };
        assert_eq!(numbers(&["ABC-1"]), [2]);
        assert_eq!(numbers(&["XYZ-9", "ABC-1"]), [2, 3]);
        assert!(numbers(&["ORD-7"]).is_empty());
    }

    #[test]
    fn the_worktree_on_a_branch_is_in_its_own_repo() {
        let fixture = Fixture::new();
        let linked = fixture.linked();
        let on = |repo: &str, branch: &str| {
            (linked.worktree_on(Path::new(repo), branch))
                .map(|item| item.path.display().to_string())
        };
        assert_eq!(on("/src/web", "change-2"), Some("/src/web.change-2".into()));
        assert_eq!(on("/src/web", "login"), None);
    }
}
