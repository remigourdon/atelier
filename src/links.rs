//! What links items to the person and to trackers ([ADR 0002]): a free-form group, and the
//! ordered issue keys an item links.
//!
//! [ADR 0002]: ../docs/adr/0002-groups-are-labels-links-live-on-items.md

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};

use color_eyre::eyre::Result;

use crate::config::{Config, issue_keys};
use crate::issues::TrackerConfig;

/// A free-form label, never empty, trimmed and uppercased wherever it comes in, so groups
/// compare exactly.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Group(String);

impl Group {
    /// `text` normalised, or `None` when it is blank: no group.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        (!text.is_empty()).then(|| Self(text.to_uppercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An optional group as text, `""` for none, as the database, front matter, the environment
/// and prompts write it.
pub fn group_text(group: Option<&Group>) -> &str {
    group.map_or("", Group::as_str)
}

/// A GitHub issue key, `owner/repo#12`.
static GITHUB_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([\w.-]+)/([\w.-]+)#([1-9][0-9]*)$").unwrap());

/// A GitHub issue key without its owner, `repo#12`.
static SHORT_GITHUB_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([\w.-]+)#([1-9][0-9]*)$").unwrap());

/// An issue's identifier in its canonical form: `ABC-5` on Jira, `owner/repo#12` on GitHub.
/// Built from a tracker's own listing, or resolved from text a person typed or wrote; read
/// back from storage as it was stored.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueKey(String);

impl IssueKey {
    /// A key as a tracker lists it, already canonical.
    pub fn listed(key: String) -> Self {
        Self(key)
    }

    /// Text a person typed or wrote, trimmed, a short GitHub key `repo#12` made `owner/repo#12`
    /// when exactly one configured `[tracker]` repo has that name (a registered repo's alias
    /// never counts); any other key as written. `None` when blank.
    pub fn resolve(text: &str, tracker: &TrackerConfig) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let Some(found) = SHORT_GITHUB_KEY.captures(text) else {
            return Some(Self(text.to_owned()));
        };
        let (name, number) = (&found[1], &found[2]);
        let mut matching = (tracker.github_repos()).filter(|repo| {
            (repo.split_once('/')).is_some_and(|(_, other)| other.eq_ignore_ascii_case(name))
        });
        Some(Self(match (matching.next(), matching.next()) {
            (Some(repo), None) => format!("{repo}#{number}"),
            _ => text.to_owned(),
        }))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// How it is shown: a GitHub key `owner/repo#12` as `repo#12` unless another configured
    /// repo shares that name; any other key as it is.
    pub fn display(&self, tracker: &TrackerConfig) -> String {
        let Some(found) = GITHUB_KEY.captures(&self.0) else {
            return self.0.clone();
        };
        let (owner, name, number) = (&found[1], &found[2], &found[3]);
        let ambiguous = (tracker.github_repos()).any(|repo| match repo.split_once('/') {
            Some((other_owner, other_name)) => {
                other_name.eq_ignore_ascii_case(name) && !other_owner.eq_ignore_ascii_case(owner)
            }
            None => false,
        });
        match ambiguous {
            true => self.0.clone(),
            false => format!("{name}#{number}"),
        }
    }
}

impl fmt::Display for IssueKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The issue keys an item links, in order, never twice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct IssueKeys(Vec<IssueKey>);

impl IssueKeys {
    /// Each text resolved as [`IssueKey::resolve`] does, the blank ones dropped.
    pub fn resolve<'a>(texts: impl IntoIterator<Item = &'a str>, tracker: &TrackerConfig) -> Self {
        (texts.into_iter())
            .filter_map(|text| IssueKey::resolve(text, tracker))
            .collect()
    }

    /// Adds `key` last, unless it is already linked.
    pub fn push(&mut self, key: IssueKey) {
        if !self.links(&key) {
            self.0.push(key);
        }
    }

    /// Adds each of `keys` last, in order, as [`Self::push`] does.
    pub fn extend(&mut self, keys: impl IntoIterator<Item = IssueKey>) {
        for key in keys {
            self.push(key);
        }
    }

    pub fn links(&self, key: &IssueKey) -> bool {
        self.0.contains(key)
    }

    /// Whether it links any of `other`.
    pub fn shares(&self, other: &IssueKeys) -> bool {
        self.iter().any(|key| other.links(key))
    }

    pub fn first(&self) -> Option<&IssueKey> {
        self.0.first()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, IssueKey> {
        self.0.iter()
    }

    /// Every key as shown, joined by `separator`.
    pub fn display(&self, tracker: &TrackerConfig, separator: &str) -> String {
        let shown: Vec<String> = self.iter().map(|key| key.display(tracker)).collect();
        shown.join(separator)
    }

    /// Every key as stored, joined by `separator`.
    pub fn join(&self, separator: &str) -> String {
        let keys: Vec<&str> = self.iter().map(IssueKey::as_str).collect();
        keys.join(separator)
    }
}

impl FromIterator<IssueKey> for IssueKeys {
    fn from_iter<I: IntoIterator<Item = IssueKey>>(keys: I) -> Self {
        let mut linked = Self::default();
        linked.extend(keys);
        linked
    }
}

impl<'a> IntoIterator for &'a IssueKeys {
    type Item = &'a IssueKey;
    type IntoIter = std::slice::Iter<'a, IssueKey>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Read back as stored, a key stored twice kept once.
impl<'de> Deserialize<'de> for IssueKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<IssueKey>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

/// Finds the issue keys written in free text, such as a branch, a title or a review's body,
/// and resolves them against the tracker config.
#[derive(Debug, Clone)]
pub struct KeyFinder<'a> {
    pattern: Regex,
    tracker: &'a TrackerConfig,
}

impl<'a> KeyFinder<'a> {
    pub fn new(config: &'a Config) -> Result<Self> {
        Ok(Self {
            pattern: config.issue_key_regex()?,
            tracker: &config.tracker,
        })
    }

    /// Every issue key in `texts`, in order, never twice.
    pub fn find(&self, texts: &[&str]) -> IssueKeys {
        let found: Vec<String> = (texts.iter())
            .flat_map(|text| issue_keys(&self.pattern, text))
            .collect();
        IssueKeys::resolve(found.iter().map(String::as_str), self.tracker)
    }
}

/// An item's group and the issue keys it links.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Links {
    pub group: Option<Group>,
    pub issue_keys: IssueKeys,
}

impl Links {
    /// Whether it links the issue `key`.
    pub fn links(&self, key: &IssueKey) -> bool {
        self.issue_keys.links(key)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A key as a test writes it, taken as canonical.
    pub fn key(text: &str) -> IssueKey {
        IssueKey::listed(text.to_owned())
    }

    /// Keys as a test writes them, taken as canonical.
    pub fn keys(texts: &[&str]) -> IssueKeys {
        texts.iter().map(|&text| key(text)).collect()
    }

    /// A group as a test writes it, `""` for none.
    pub fn group(text: &str) -> Option<Group> {
        Group::parse(text)
    }

    /// Links as a test writes them, `""` for no group.
    pub fn links(group: &str, issue_keys: &[&str]) -> Links {
        Links {
            group: self::group(group),
            issue_keys: keys(issue_keys),
        }
    }

    fn tracker(text: &str) -> TrackerConfig {
        crate::config::Config::parse(text).unwrap().tracker
    }

    #[test]
    fn a_group_is_trimmed_and_uppercased_and_never_blank() {
        assert_eq!(
            Group::parse("  login rewrite ").unwrap().as_str(),
            "LOGIN REWRITE"
        );
        assert_eq!(Group::parse(" \t"), None);
        assert_eq!(group_text(Group::parse("x").as_ref()), "X");
        assert_eq!(group_text(None), "");
        assert_eq!(serde_json::to_string(&Group::parse("a")).unwrap(), "\"A\"");
    }

    #[test]
    fn github_keys_show_without_their_owner_unless_another_repo_shares_the_name() {
        let tracker = tracker("[tracker.github]\nrepos = [\"o/a\", \"o/b\", \"p/A\"]\n");
        assert_eq!(key("o/b#3").display(&tracker), "b#3");
        assert_eq!(
            key("o/a#3").display(&tracker),
            "o/a#3",
            "p/A shares the name"
        );
        assert_eq!(
            key("q/c#3").display(&tracker),
            "c#3",
            "an unconfigured repo"
        );
        assert_eq!(key("ABC-1").display(&tracker), "ABC-1");
        assert_eq!(key("o/a#3").display(&self::tracker("")), "a#3");
        assert_eq!(
            keys(&["o/b#3", "ABC-1"]).display(&tracker, ", "),
            "b#3, ABC-1"
        );
    }

    #[test]
    fn short_github_keys_resolve_against_one_configured_repo() {
        let tracker = tracker("[tracker.github]\nrepos = [\"o/a\", \"o/Web\", \"p/a\"]\n");
        let resolve = |text| IssueKey::resolve(text, &tracker).map(|key| key.0);
        assert_eq!(resolve(" web#4 ").as_deref(), Some("o/Web#4"));
        assert_eq!(resolve("a#4").as_deref(), Some("a#4"), "ambiguous: kept");
        assert_eq!(resolve("api#4").as_deref(), Some("api#4"), "no such repo");
        assert_eq!(resolve("p/a#4").as_deref(), Some("p/a#4"));
        assert_eq!(resolve("ABC-1").as_deref(), Some("ABC-1"));
        assert_eq!(resolve(" "), None);
        assert_eq!(
            IssueKeys::resolve(["web#4", " ", "ABC-1", "o/Web#4"], &tracker),
            keys(&["o/Web#4", "ABC-1"])
        );
    }

    #[test]
    fn issue_keys_keep_their_order_and_never_hold_a_key_twice() {
        let mut linked = keys(&["B-2", "A-1", "B-2"]);
        assert_eq!(linked.join(","), "B-2,A-1");
        linked.push(key("A-1"));
        linked.push(key("C-3"));
        assert_eq!(linked.join(","), "B-2,A-1,C-3");
        assert!(linked.links(&key("C-3")) && !linked.links(&key("D-4")));
        assert!(linked.shares(&keys(&["D-4", "A-1"])) && !linked.shares(&keys(&["D-4"])));
        assert_eq!(linked.first(), Some(&key("B-2")));
        let json = serde_json::to_string(&linked).unwrap();
        assert_eq!(json, r#"["B-2","A-1","C-3"]"#);
        let read: IssueKeys = serde_json::from_str(r#"["X-1","X-1","o/r#2"]"#).unwrap();
        assert_eq!(read, keys(&["X-1", "o/r#2"]), "read as stored, once each");
    }

    #[test]
    fn links_serialise_flat_with_no_group_as_null() {
        let links = Links {
            group: None,
            issue_keys: keys(&["A-1"]),
        };
        assert_eq!(
            serde_json::to_value(&links).unwrap(),
            serde_json::json!({"group": null, "issue_keys": ["A-1"]})
        );
        assert!(links.links(&key("A-1")));
    }
}
