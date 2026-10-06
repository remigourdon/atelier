//! Finishing work once its review merged: which worktrees are finished, and the plan that
//! removes them, closes their group's carnet and pulls the main worktrees. Building the plan is
//! pure: the caller fetches first, then builds it from a fresh snapshot.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::items::{Removal, Snapshot, Work};
use crate::links::{Group, IssueKey};
use crate::worktrunk::Worktree;

/// Why a worktree is finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    /// worktrunk finds its branch integrated into the default branch.
    Integrated,
    /// Its upstream branch is gone, and it is not integrated.
    #[serde(rename = "upstream_gone")]
    Gone,
}

impl Signal {
    pub fn label(self) -> &'static str {
        match self {
            Signal::Integrated => "integrated",
            Signal::Gone => "upstream gone",
        }
    }
}

/// Why a worktree other than its repo's main one is finished, if it is. Integrated wins over
/// gone. As fresh as the last fetch.
pub fn signal(work: &Work) -> Option<Signal> {
    tree_signal(work.tree()?)
}

/// Why a worktree is finished, as [`signal`].
pub fn tree_signal(tree: &Worktree) -> Option<Signal> {
    if tree.main {
        None
    } else if tree.integrated {
        Some(Signal::Integrated)
    } else if tree.gone {
        Some(Signal::Gone)
    } else {
        None
    }
}

/// What a plan finishes.
#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    /// Whole groups, across repos and workspaces, and items in no group on their own.
    Work {
        groups: Vec<Group>,
        items: Vec<PathBuf>,
    },
    /// A sweep of a workspace: each of its groups with a finished worktree, and its finished
    /// worktrees in no group. It touches only the workspace's own items.
    Workspace(String),
    /// An issue's linked work: the whole groups of the items linking `key`, and those in no
    /// group on their own. `label` is the key as shown, and `state` the issue's, for the title.
    Issue {
        key: IssueKey,
        label: String,
        state: String,
    },
}

/// What a plan covers: a group, or an item in no group.
enum Cover {
    Group(Group),
    Item(PathBuf),
}

impl Scope {
    /// One group's scope, as `f` on a group's row makes it, for tests.
    #[cfg(test)]
    pub fn group(name: &str) -> Self {
        Scope::Work {
            groups: Group::parse(name).into_iter().collect(),
            items: Vec::new(),
        }
    }

    fn covers(&self, snapshot: &Snapshot) -> Vec<Cover> {
        match self {
            Scope::Work { groups, items } => (groups.iter().cloned().map(Cover::Group))
                .chain(items.iter().cloned().map(Cover::Item))
                .collect(),
            Scope::Workspace(_) => {
                let own = (snapshot.work.iter()).filter(|work| self.touches(work));
                covers(own, |work| work.removable())
            }
            Scope::Issue { key, .. } => {
                let linked = (snapshot.work.iter()).filter(|work| work.links_to(key));
                covers(linked, |_| true)
            }
        }
    }

    /// Whether it may touch `work`: a sweep only its workspace's items.
    fn touches(&self, work: &Work) -> bool {
        match self {
            Scope::Workspace(name) => work.workspace == *name,
            _ => true,
        }
    }

    /// The repos of the worktrees it may touch, to fetch before building its plan.
    pub fn repos(&self, snapshot: &Snapshot) -> Vec<PathBuf> {
        let repos: BTreeSet<&PathBuf> = (self.covers(snapshot).iter())
            .flat_map(|cover| self.own_members(snapshot, cover))
            .filter_map(Work::repo)
            .collect();
        repos.into_iter().cloned().collect()
    }

    /// What `cover` holds that the plan may touch.
    fn own_members<'a>(&self, snapshot: &'a Snapshot, cover: &Cover) -> Vec<&'a Work> {
        (members(snapshot, cover).into_iter())
            .filter(|work| self.touches(work))
            .collect()
    }

    fn title(&self, snapshot: &Snapshot) -> String {
        match self {
            Scope::Work { .. } => {
                let names: Vec<String> = (self.covers(snapshot).iter())
                    .map(|cover| cover_name(snapshot, cover))
                    .collect();
                format!("Finish {}", names.join(", "))
            }
            Scope::Workspace(name) => format!("Finish workspace {name}"),
            Scope::Issue { label, state, .. } => format!("Finish {label} · issue {state}"),
        }
    }
}

/// The groups of `work`, each once, then the items in no group that `alone` keeps, each on its
/// own.
fn covers<'a>(work: impl Iterator<Item = &'a Work>, alone: impl Fn(&Work) -> bool) -> Vec<Cover> {
    let (grouped, ungrouped): (Vec<&Work>, Vec<&Work>) =
        work.partition(|work| work.group().is_some());
    let groups: BTreeSet<&Group> = grouped.into_iter().filter_map(Work::group).collect();
    let items = (ungrouped.into_iter())
        .filter(|work| alone(work))
        .map(|work| Cover::Item(work.path.clone()));
    (groups.into_iter().cloned().map(Cover::Group))
        .chain(items)
        .collect()
}

/// Everything `cover` holds, in any workspace.
fn members<'a>(snapshot: &'a Snapshot, cover: &Cover) -> Vec<&'a Work> {
    (snapshot.work.iter())
        .filter(|work| match cover {
            Cover::Group(group) => work.group() == Some(group),
            Cover::Item(path) => work.path == *path,
        })
        .collect()
}

fn cover_name(snapshot: &Snapshot, cover: &Cover) -> String {
    match cover {
        Cover::Group(group) => group.to_string(),
        Cover::Item(path) => (members(snapshot, cover).first())
            .map_or_else(|| path.display().to_string(), |work| work.title()),
    }
}

/// What a checked line runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// `wt remove`, with `--force` when dirty and never `-D`: worktrunk deletes an integrated
    /// branch and keeps any other, so unpushed commits survive.
    Remove { removal: Removal, signal: Signal },
    /// Sets `closed = true`, as `c` does.
    CloseCarnet(PathBuf),
    /// `git pull --ff-only --prune` on a main worktree.
    Pull(PathBuf),
}

impl Step {
    /// Removals run first, then carnet closes, then pulls.
    pub fn order(&self) -> u8 {
        match self {
            Step::Remove { .. } => 0,
            Step::CloseCarnet(_) => 1,
            Step::Pull(_) => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// A fetch that failed, so the plan shows the last known state.
    Warning(String),
    /// A group's heading, when the plan holds several.
    Heading(String),
    /// What the plan does not do, and why; never checkable.
    Info { label: String, note: String },
    /// What the plan can do, run when checked.
    Step {
        step: Step,
        label: String,
        note: String,
        checked: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub title: String,
    pub lines: Vec<Line>,
}

impl Plan {
    /// The indices of the lines that can be checked.
    pub fn checkable(&self) -> Vec<usize> {
        (self.lines.iter().enumerate())
            .filter(|(_, line)| matches!(line, Line::Step { .. }))
            .map(|(index, _)| index)
            .collect()
    }

    /// Checks or unchecks a step line; any other line stays as it is.
    pub fn toggle(&mut self, index: usize) {
        if let Some(Line::Step { checked, .. }) = self.lines.get_mut(index) {
            *checked = !*checked;
        }
    }

    /// The checked steps, in the plan's order.
    pub fn checked(&self) -> Vec<Step> {
        (self.lines.iter())
            .filter_map(|line| match line {
                Line::Step {
                    step,
                    checked: true,
                    ..
                } => Some(step.clone()),
                _ => None,
            })
            .collect()
    }
}

/// A group's or an item's lines under its heading, whether any worktree in it is finished, and
/// the repos it touches.
struct Part {
    heading: String,
    lines: Vec<Line>,
    finished: bool,
    repos: BTreeSet<PathBuf>,
}

fn part(snapshot: &Snapshot, scope: &Scope, cover: &Cover) -> Part {
    let own = scope.own_members(snapshot, cover);
    let mut lines = Vec::new();
    let mut repos = BTreeSet::new();
    let mut finished = false;
    // Whether every worktree of the group that is not a main one, in any workspace, has a
    // checked remove line.
    let mut all_removed = (members(snapshot, cover).iter())
        .filter(|work| work.removable())
        .all(|work| scope.touches(work));
    for work in &own {
        let (Some(tree), Some(repo)) = (work.tree(), work.repo()) else {
            continue;
        };
        repos.insert(repo.clone());
        let Some(removal) = Removal::of(work) else {
            continue;
        };
        let Some(signal) = signal(work) else {
            all_removed = false;
            let upstream = if tree.upstream.is_some() {
                "upstream present"
            } else {
                "no upstream"
            };
            lines.push(Line::Info {
                label: work.title(),
                note: format!("not integrated, {upstream}"),
            });
            continue;
        };
        finished = true;
        let dirty = removal.force;
        all_removed &= !dirty;
        let mut notes = vec![signal.label().to_owned()];
        if dirty {
            notes.push("uncommitted changes will be lost".into());
        }
        if signal == Signal::Gone {
            let default = tree
                .default_branch
                .as_deref()
                .unwrap_or("the default branch");
            notes.push(match tree.ahead_of_default {
                Some(1) => format!("branch kept: 1 commit not in {default}"),
                Some(ahead) if ahead > 1 => {
                    format!("branch kept: {ahead} commits not in {default}")
                }
                _ => "branch kept".into(),
            });
        }
        if work.tab {
            notes.push("tab open".into());
        }
        lines.push(Line::Step {
            step: Step::Remove { removal, signal },
            label: format!("remove {}", work.title()),
            note: notes.join(" · "),
            checked: !dirty,
        });
    }
    // Every open carnet: in a group, each of its carnets.
    for carnet in (own.iter()).filter(|work| work.is_carnet() && !work.closed()) {
        lines.push(Line::Step {
            step: Step::CloseCarnet(carnet.path.clone()),
            label: format!("close carnet {}", carnet.title()),
            note: if all_removed {
                String::new()
            } else {
                "work still in flight".into()
            },
            checked: all_removed,
        });
    }
    Part {
        heading: cover_name(snapshot, cover),
        lines,
        finished,
        repos,
    }
}

/// The line pulling a repo's main worktree: checked when it is clean, on the default branch
/// and behind its upstream, else why not.
fn pull(main: &Work) -> Option<Line> {
    let tree = main.tree()?;
    let label = format!("pull {}", main.title());
    let info = |note: &str| {
        Some(Line::Info {
            label: label.clone(),
            note: note.into(),
        })
    };
    if tree.dirty {
        return info("uncommitted changes");
    }
    if !tree.on_default {
        return info("not on the default branch");
    }
    match tree.upstream {
        None => info("no upstream"),
        Some((_, 0)) => info("up to date"),
        Some((_, behind)) => Some(Line::Step {
            step: Step::Pull(main.path.clone()),
            label,
            note: format!("↓{behind}"),
            checked: true,
        }),
    }
}

/// The plan for `scope` from a snapshot taken after fetching, warning first of each repo,
/// by name, whose fetch failed.
pub fn plan(snapshot: &Snapshot, scope: &Scope, failed: &[String]) -> Plan {
    let mut lines: Vec<Line> = (failed.iter())
        .map(|repo| Line::Warning(format!("fetch failed in {repo}: showing last known state")))
        .collect();
    let mut parts: Vec<Part> = (scope.covers(snapshot).iter())
        .map(|cover| part(snapshot, scope, cover))
        .collect();
    // A sweep shows only what is finished, unless nothing is: then every group says why.
    if matches!(scope, Scope::Workspace(_)) && parts.iter().any(|part| part.finished) {
        parts.retain(|part| part.finished);
    }
    let several = parts.len() > 1;
    let mut repos = BTreeSet::new();
    for part in parts {
        if several {
            lines.push(Line::Heading(part.heading));
        }
        lines.extend(part.lines);
        repos.extend(part.repos);
    }
    for repo in &repos {
        let main = (snapshot.work.iter()).find(|work| {
            scope.touches(work)
                && work.repo() == Some(repo)
                && work.tree().is_some_and(|tree| tree.main)
        });
        lines.extend(main.and_then(pull));
    }
    if lines.iter().all(|line| matches!(line, Line::Warning(_))) {
        lines.push(Line::Info {
            label: "nothing to finish".into(),
            note: String::new(),
        });
    }
    Plan {
        title: scope.title(snapshot),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::items::WorkKind;
    use crate::links::tests::{key, links};
    use crate::worktrunk::Worktree;

    fn work(repo: &str, branch: &str, group: &str, workspace: &str) -> Work {
        linking(repo, branch, group, &[], workspace)
    }

    fn linking(repo: &str, branch: &str, group: &str, keys: &[&str], workspace: &str) -> Work {
        let main = branch == "main";
        let path = if main {
            PathBuf::from(format!("/src/{repo}"))
        } else {
            PathBuf::from(format!("/src/{repo}.{branch}"))
        };
        Work {
            path: path.clone(),
            workspace: workspace.into(),
            links: links(group, keys),
            tab: false,
            kind: WorkKind::Worktree {
                repo: PathBuf::from(format!("/src/{repo}")),
                repo_name: repo.into(),
                tree: Box::new(Worktree {
                    path,
                    branch: Some(branch.into()),
                    main,
                    on_default: main,
                    default_branch: Some("main".into()),
                    upstream: Some((0, 0)),
                    ..Worktree::default()
                }),
            },
        }
    }

    fn integrated(mut work: Work) -> Work {
        work.tree_mut().integrated = true;
        work
    }

    fn gone(mut work: Work) -> Work {
        work.tree_mut().gone = true;
        work.tree_mut().upstream = None;
        work
    }

    fn dirty(mut work: Work) -> Work {
        work.tree_mut().dirty = true;
        work
    }

    fn carnet(name: &str, group: &str, keys: &[&str]) -> Work {
        Work {
            path: PathBuf::from(format!("/data/{name}")),
            workspace: "default".into(),
            links: links(group, keys),
            tab: false,
            kind: WorkKind::Carnet {
                closed: false,
                summary: String::new(),
                readme: None,
            },
        }
    }

    fn snapshot(work: Vec<Work>) -> Snapshot {
        Snapshot {
            work,
            ..Snapshot::default()
        }
    }

    /// Each line as `[x] label · note`, `[ ]` unchecked, four spaces for info.
    fn shown(plan: &Plan) -> Vec<String> {
        (plan.lines.iter())
            .map(|line| match line {
                Line::Warning(text) => format!("! {text}"),
                Line::Heading(name) => format!("# {name}"),
                Line::Info { label, note } => format!("    {label} · {note}"),
                Line::Step {
                    label,
                    note,
                    checked,
                    ..
                } => {
                    let mark = if *checked { "[x]" } else { "[ ]" };
                    format!("{mark} {label} · {note}")
                }
            })
            .collect()
    }

    #[test]
    fn signals_mark_only_worktrees_other_than_main() {
        assert_eq!(
            signal(&integrated(work("api", "a", "", "d"))),
            Some(Signal::Integrated)
        );
        assert_eq!(signal(&gone(work("api", "a", "", "d"))), Some(Signal::Gone));
        assert_eq!(
            signal(&gone(integrated(work("api", "a", "", "d")))),
            Some(Signal::Integrated),
            "integrated wins"
        );
        assert_eq!(signal(&work("api", "a", "", "d")), None);
        assert_eq!(signal(&integrated(work("api", "main", "", "d"))), None);
        assert_eq!(signal(&carnet("2026-10-01-x", "", &[])), None);
    }

    #[test]
    fn each_kind_of_worktree_gets_its_default() {
        let mut tabbed = integrated(work("api", "ABC-1-login", "ABC-1", "default"));
        tabbed.tab = true;
        let mut ahead = gone(work("api", "ABC-1-fix", "ABC-1", "default"));
        ahead.tree_mut().ahead_of_default = Some(2);
        let snapshot = snapshot(vec![
            tabbed,
            ahead,
            dirty(integrated(work("web", "ABC-1-form", "ABC-1", "side"))),
            dirty(gone(work("web", "ABC-1-old", "ABC-1", "side"))),
            work("web", "ABC-1-wip", "ABC-1", "side"),
            work("web", "XYZ-1", "XYZ-1", "side"),
        ]);
        let plan = plan(&snapshot, &Scope::group("ABC-1"), &[]);
        assert_eq!(plan.title, "Finish ABC-1");
        assert_eq!(
            shown(&plan),
            [
                "[x] remove api:ABC-1-login · integrated · tab open",
                "[x] remove api:ABC-1-fix · upstream gone · branch kept: 2 commits not in main",
                "[ ] remove web:ABC-1-form · integrated · uncommitted changes will be lost",
                "[ ] remove web:ABC-1-old · upstream gone · uncommitted changes will be lost · branch kept",
                "    web:ABC-1-wip · not integrated, upstream present",
            ],
            "across repos and workspaces; no main worktree is listed to pull"
        );
    }

    #[test]
    fn a_gone_branch_counts_its_commits_not_in_the_default_branch() {
        let note = |ahead, default: Option<&str>| {
            let mut fix = gone(work("api", "fix", "", "d"));
            fix.tree_mut().ahead_of_default = ahead;
            fix.tree_mut().default_branch = default.map(Into::into);
            let scope = Scope::Work {
                groups: Vec::new(),
                items: vec!["/src/api.fix".into()],
            };
            shown(&plan(&snapshot(vec![fix]), &scope, &[])).remove(0)
        };
        let lines = [
            note(Some(1), Some("trunk")),
            note(Some(3), None),
            note(Some(0), Some("main")),
            note(None, Some("main")),
        ];
        assert_eq!(
            lines.map(|line| line.rsplit(" · ").next().unwrap().to_owned()),
            [
                "branch kept: 1 commit not in trunk",
                "branch kept: 3 commits not in the default branch",
                "branch kept",
                "branch kept",
            ]
        );
    }

    #[test]
    fn removals_force_only_dirty_worktrees() {
        let snapshot = snapshot(vec![
            integrated(work("api", "a", "G-1", "d")),
            dirty(integrated(work("api", "b", "G-1", "d"))),
        ]);
        let mut plan = plan(&snapshot, &Scope::group("G-1"), &[]);
        plan.toggle(1);
        let forced: Vec<bool> = (plan.checked().into_iter())
            .map(|step| match step {
                Step::Remove { removal, .. } => removal.force,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(forced, [false, true]);
    }

    #[test]
    fn the_carnets_close_only_once_all_the_groups_work_goes() {
        let all = snapshot(vec![
            integrated(work("api", "ABC-1-a", "ABC-1", "default")),
            gone(work("api", "ABC-1-b", "ABC-1", "default")),
            carnet("2026-10-01-ABC-1-flake", "ABC-1", &[]),
            carnet("2026-10-02-more", "ABC-1", &["XYZ-9"]),
            carnet("2026-10-03-other", "XYZ-9", &["ABC-1"]),
        ]);
        assert_eq!(
            shown(&plan(&all, &Scope::group("ABC-1"), &[]))[2..],
            [
                "[x] close carnet 2026-10-01-ABC-1-flake · ",
                "[x] close carnet 2026-10-02-more · ",
            ],
            "every open carnet of the group, none of another group linking the same key"
        );
        let mut in_flight = all.clone();
        in_flight
            .work
            .push(work("web", "ABC-1-c", "ABC-1", "default"));
        let lines = shown(&plan(&in_flight, &Scope::group("ABC-1"), &[]));
        assert_eq!(
            lines.last().unwrap(),
            "[ ] close carnet 2026-10-02-more · work still in flight"
        );
        let mut kept_dirty = all;
        kept_dirty.work[0] = dirty(kept_dirty.work[0].clone());
        let lines = shown(&plan(&kept_dirty, &Scope::group("ABC-1"), &[]));
        assert!(lines.last().unwrap().starts_with("[ ] close carnet"));
        let only_main = snapshot(vec![
            work("api", "main", "ABC-1", "default"),
            carnet("2026-10-01-ABC-1-flake", "ABC-1", &[]),
        ]);
        assert_eq!(
            shown(&plan(&only_main, &Scope::group("ABC-1"), &[])),
            [
                "[x] close carnet 2026-10-01-ABC-1-flake · ",
                "    pull api:main · up to date",
            ],
            "main worktrees are never removed"
        );
    }

    #[test]
    fn main_worktrees_are_pulled_when_clean_on_the_default_branch_and_behind() {
        let mut behind = work("api", "main", "", "default");
        behind.tree_mut().upstream = Some((0, 3));
        let mut elsewhere = behind.clone();
        elsewhere.tree_mut().on_default = false;
        let mut no_upstream = behind.clone();
        no_upstream.tree_mut().upstream = None;
        for (main, expected) in [
            (behind.clone(), "[x] pull api:main · ↓3"),
            (dirty(behind), "    pull api:main · uncommitted changes"),
            (elsewhere, "    pull api:main · not on the default branch"),
            (no_upstream, "    pull api:main · no upstream"),
            (
                work("api", "main", "", "side"),
                "    pull api:main · up to date",
            ),
        ] {
            let snapshot = snapshot(vec![integrated(work("api", "a", "G-1", "d")), main]);
            let lines = shown(&plan(&snapshot, &Scope::group("G-1"), &[]));
            assert_eq!(lines[1], expected);
        }
    }

    #[test]
    fn a_sweep_has_a_part_per_group_with_finished_work_in_its_workspace() {
        let mut snapshot = snapshot(vec![
            work("api", "main", "", "default"),
            integrated(work("api", "ABC-1-a", "ABC-1", "default")),
            work("web", "ABC-1-b", "ABC-1", "side"),
            integrated(work("web", "ABC-1-c", "ABC-1", "side")),
            carnet("2026-10-01-ABC-1-x", "ABC-1", &[]),
            work("api", "XYZ-2", "XYZ-2", "default"),
            gone(work("api", "DEF-3", "DEF-3", "default")),
            integrated(work("api", "loose", "", "default")),
            integrated(work("web", "GHI-4", "GHI-4", "side")),
        ]);
        let sweep = Scope::Workspace("default".into());
        assert_eq!(
            sweep.repos(&snapshot),
            [Path::new("/src/api")],
            "only the workspace's own worktrees"
        );
        let swept = plan(&snapshot, &sweep, &[]);
        assert_eq!(swept.title, "Finish workspace default");
        assert_eq!(
            shown(&swept),
            [
                "# ABC-1",
                "[x] remove api:ABC-1-a · integrated",
                "[ ] close carnet 2026-10-01-ABC-1-x · work still in flight",
                "# DEF-3",
                "[x] remove api:DEF-3 · upstream gone · branch kept",
                "# api:loose",
                "[x] remove api:loose · integrated",
                "    pull api:main · up to date",
            ],
            "XYZ-2 has nothing finished; ABC-1's worktrees in side stay, and so does its carnet"
        );
        snapshot.work.retain(|work| signal(work).is_none());
        assert_eq!(
            shown(&plan(&snapshot, &sweep, &[])),
            [
                "# ABC-1",
                "[ ] close carnet 2026-10-01-ABC-1-x · work still in flight",
                "# XYZ-2",
                "    api:XYZ-2 · not integrated, upstream present",
                "    pull api:main · up to date",
            ],
            "with nothing finished, every group says why"
        );
    }

    #[test]
    fn an_issue_plans_the_whole_groups_of_its_linked_work() {
        let snapshot = snapshot(vec![
            integrated(linking("api", "1-login", "LOGIN", &["o/api#1"], "default")),
            integrated(work("web", "form", "LOGIN", "side")),
            carnet("2026-10-01-login", "LOGIN", &[]),
            integrated(linking(
                "api",
                "fix",
                "",
                &["o/api#2", "o/api#1"],
                "default",
            )),
            integrated(work("api", "loose", "", "default")),
            carnet("2026-10-02-notes", "", &["o/api#1"]),
            integrated(work("api", "2-other", "OTHER", "default")),
        ]);
        let scope = Scope::Issue {
            key: key("o/api#1"),
            label: "api#1".into(),
            state: "done".into(),
        };
        let plan = plan(&snapshot, &scope, &[]);
        assert_eq!(plan.title, "Finish api#1 · issue done");
        assert_eq!(
            shown(&plan),
            [
                "# LOGIN",
                "[x] remove api:1-login · integrated",
                "[x] remove web:form · integrated",
                "[x] close carnet 2026-10-01-login · ",
                "# api:fix",
                "[x] remove api:fix · integrated",
                "# 2026-10-02-notes",
                "[x] close carnet 2026-10-02-notes · ",
            ],
            "the group's members linking no key too; ungrouped items linking it alone; \
             no main worktree in the snapshot, no pull line"
        );
    }

    #[test]
    fn an_item_in_no_group_is_planned_alone() {
        let snapshot = snapshot(vec![
            integrated(work("api", "fix", "", "default")),
            integrated(work("api", "other", "", "default")),
        ]);
        let scope = Scope::Work {
            groups: Vec::new(),
            items: vec!["/src/api.fix".into()],
        };
        let plan = plan(&snapshot, &scope, &[]);
        assert_eq!(plan.title, "Finish api:fix");
        assert_eq!(shown(&plan), ["[x] remove api:fix · integrated"]);
    }

    #[test]
    fn a_failed_fetch_warns_and_an_empty_plan_says_so() {
        let plan = plan(&snapshot(Vec::new()), &Scope::group("G-1"), &["api".into()]);
        assert_eq!(
            shown(&plan),
            [
                "! fetch failed in api: showing last known state",
                "    nothing to finish · ",
            ]
        );
        assert!(plan.checkable().is_empty());
    }

    #[test]
    fn toggling_checks_and_unchecks_steps_only() {
        let snapshot = snapshot(vec![
            integrated(work("api", "a", "G-1", "d")),
            work("api", "b", "G-1", "d"),
        ]);
        let mut plan = plan(&snapshot, &Scope::group("G-1"), &[]);
        assert_eq!(plan.checkable(), [0]);
        plan.toggle(1);
        plan.toggle(0);
        assert!(plan.checked().is_empty());
        plan.toggle(0);
        assert_eq!(plan.checked().len(), 1);
    }
}
