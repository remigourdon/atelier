//! Runs jobs off the UI thread, each with its own database connection, and reports actions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Report, Result, eyre};
use regex::Regex;

use super::app::{Action, Job, Removal, Snapshot, Work, WorkKind};
use crate::config::{Config, group_from_name};
use crate::issues::{self, Issue, TrackerConfig};
use crate::process::{Logged, Recorder, Runner, System};
use crate::reviews::{self, Provider, Review, Role};
use crate::state::{self, ItemKind, State};
use crate::zellij::{self, Layouts, Zellij};
use crate::{carnet, hooks, sync, worktrunk};

/// What every job needs, shared across them.
pub struct Context {
    pub config: Config,
    pub db: PathBuf,
    pub layouts: Layouts,
    pub ticket: Regex,
    pub names: carnet::Names,
}

impl Context {
    /// Creates and migrates the database once; jobs then only connect to it.
    pub fn new(config: Config) -> Result<Self> {
        let db = state::db_path();
        State::open(&db, config.default_workspace())?;
        Ok(Self {
            layouts: Layouts::resolve(&config)?,
            ticket: config.ticket_regex()?,
            names: carnet::Names::new(config.ticket_pattern())?,
            db,
            config,
        })
    }

    fn zellij<'a>(&self, runner: &'a dyn Runner) -> Zellij<'a> {
        Zellij {
            runner,
            here: zellij::current_session(),
            layouts: self.layouts.clone(),
            anchor: self.config.anchor_pane().to_owned(),
            reconciled: Default::default(),
        }
    }

    fn state(&self) -> Result<State> {
        State::connect(&self.db, self.config.default_workspace())
    }
}

pub fn run(context: &Context, job: Job) -> Action {
    let recorder = Recorder::new(&System);
    let zellij = context.zellij(&recorder);
    match job {
        // Refreshes and commit listings run constantly: log only their failures.
        Job::Refresh { full } => {
            let carnets = context.config.carnets_enabled();
            let loaded = context
                .state()
                .and_then(|state| load(&state, &zellij, &context.ticket, full, carnets));
            let mut log: Vec<Logged> = recorder
                .take()
                .into_iter()
                .filter(|entry| entry.error.is_some())
                .collect();
            let snapshot = loaded
                .map(|(snapshot, problems)| {
                    log.extend(problems);
                    snapshot
                })
                .map_err(|err| err.to_string());
            Action::Loaded {
                snapshot,
                full,
                log,
            }
        }
        Job::Commits(path) => {
            let lines = commits(&recorder, &path).unwrap_or_default();
            Action::Commits(path, lines)
        }
        Job::Readme(path) => {
            let readme = std::fs::read_to_string(path.join("README.md")).ok();
            Action::Readme(path, readme)
        }
        // Like refreshes, these run constantly: log only failures.
        Job::Reviews {
            provider,
            hosts,
            force,
        } => {
            let (reviews, log) = match context.state() {
                Ok(state) => {
                    let (reviews, log) = reviews(&state, &recorder, provider, &hosts, force);
                    (Ok(reviews), log)
                }
                Err(err) => (Err(err.to_string()), Vec::new()),
            };
            Action::Reviews {
                provider,
                reviews,
                log,
            }
        }
        Job::Issues {
            tracker,
            scopes,
            force,
        } => {
            let config = &context.config.tracker;
            let (issues, log) = match context.state() {
                Ok(state) => {
                    let (issues, log) = issues(&state, &recorder, config, tracker, &scopes, force);
                    (Ok(issues), log)
                }
                Err(err) => (Err(err.to_string()), Vec::new()),
            };
            Action::Issues {
                tracker,
                issues,
                log,
            }
        }
        job => {
            let error = context
                .state()
                .and_then(|mut state| execute(context, &mut state, &zellij, job.clone()))
                .err()
                .map(|err| err.to_string());
            Action::Finished {
                job,
                log: recorder.take(),
                error,
            }
        }
    }
}

/// Attaches to a session from outside zellij; the caller hands over the terminal.
pub fn attach(context: &Context, session: &str) -> Vec<Logged> {
    let recorder = Recorder::new(&System);
    let _ = context.zellij(&recorder).open_session(session);
    recorder.take()
}

/// What a fetch adds to the command log: each command that failed, and the fetch's own error
/// only when no command failed, as when parsing.
fn fetch_failures(recorder: &Recorder, error: Option<Report>, what: String) -> Vec<Logged> {
    let mut failed: Vec<Logged> = (recorder.take().into_iter())
        .filter(|entry| entry.error.is_some())
        .collect();
    if let Some(error) = error
        && failed.is_empty()
    {
        failed.push(Logged {
            command: what,
            error: Some(format!("{error:#}")),
        });
    }
    failed
}

/// A provider's reviews in both roles on every host, with what failed for the command log.
fn reviews(
    state: &State,
    recorder: &Recorder,
    provider: Provider,
    hosts: &[String],
    force: bool,
) -> (Vec<Review>, Vec<Logged>) {
    let mut reviews = Vec::new();
    let mut log = Vec::new();
    for host in hosts {
        let api = provider.reviews(recorder, host.clone());
        for role in Role::ALL {
            let (found, error) = reviews::fetch(state, api.as_ref(), role, force);
            reviews.extend(found);
            log.extend(fetch_failures(
                recorder,
                error,
                format!("{} reviews", provider.cli()),
            ));
        }
    }
    (reviews, log)
}

/// A tracker's issues in every scope, with what failed for the command log.
fn issues(
    state: &State,
    recorder: &Recorder,
    config: &TrackerConfig,
    tracker: issues::Tracker,
    scopes: &[String],
    force: bool,
) -> (Vec<Issue>, Vec<Logged>) {
    let mut issues = Vec::new();
    let mut log = Vec::new();
    for scope in scopes {
        let api = tracker.issues(recorder, scope.clone(), config);
        let (found, error) = issues::fetch(state, api.as_ref(), force);
        issues.extend(found);
        log.extend(fetch_failures(
            recorder,
            error,
            format!("{} issues", tracker.cli()),
        ));
    }
    (issues, log)
}

fn commits(runner: &dyn Runner, path: &Path) -> Result<Vec<String>> {
    let path = path.to_string_lossy();
    let log = runner.output(
        "git",
        &["-C", &path, "log", "-n", "20", "--format=%h %s (%cr, %an)"],
    )?;
    Ok(log.lines().map(Into::into).collect())
}

/// Syncs the recorded items with worktrunk and gathers what the panels show, with the carnets
/// when `carnets` are enabled, and the repos that could not be listed.
pub fn load(
    state: &State,
    zellij: &Zellij,
    ticket: &Regex,
    full: bool,
    carnets: bool,
) -> Result<(Snapshot, Vec<Logged>)> {
    // A reconcile failure (zellij not running) should not hide the worktrees.
    let _ = zellij.reconcile(state);
    let synced = sync::sync(state, zellij.runner, ticket, full)?;
    let problems = synced
        .failures
        .iter()
        .map(|(repo, err)| Logged {
            command: format!("wt list in {}", repo.name()),
            error: Some(format!("{err:#}")),
        })
        .collect();
    let mut work = Vec::new();
    for tracked in synced.worktrees {
        work.push(Work {
            path: tracked.tree.path.clone(),
            tab: state.tab(&tracked.tree.path)?.is_some(),
            workspace: tracked.item.workspace,
            group: tracked.item.group,
            kind: WorkKind::Worktree {
                repo_name: tracked.repo.name(),
                repo: tracked.repo.path,
                tree: Box::new(tracked.tree),
            },
        });
    }
    // Sync forgot the carnets whose folder is gone.
    let all_carnets = state.carnets()?;
    if carnets {
        for item in &all_carnets {
            work.push(Work {
                path: item.path.clone(),
                tab: state.tab(&item.path)?.is_some(),
                workspace: item.workspace.clone(),
                group: item.group.clone(),
                kind: WorkKind::Carnet,
            });
        }
    }
    let mut workspaces = state.workspaces()?;
    if let Some(here) = &zellij.here
        && let Some(index) = workspaces.iter().position(|name| name == here)
    {
        let here = workspaces.remove(index);
        workspaces.insert(0, here);
    }
    let snapshot = Snapshot {
        here: zellij.here.clone(),
        workspaces,
        repos: state.repos()?,
        work,
        all_carnets,
        forges: synced.forges,
    };
    Ok((snapshot, problems))
}

fn execute(context: &Context, state: &mut State, zellij: &Zellij, job: Job) -> Result<()> {
    let runner = zellij.runner;
    let failures = |results: Vec<Result<()>>| -> Result<()> {
        let errors: Vec<String> = results
            .into_iter()
            .filter_map(|result| result.err().map(|err| err.to_string()))
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(eyre!(errors.join("; ")))
        }
    };
    match job {
        Job::Refresh { .. }
        | Job::Commits(_)
        | Job::Readme(_)
        | Job::Reviews { .. }
        | Job::Issues { .. } => {
            unreachable!("run handles these")
        }
        Job::Open(paths) => failures(
            paths
                .iter()
                .map(|path| zellij.open_tab(state, path).map(drop))
                .collect(),
        ),
        Job::Close(paths) => failures(
            paths
                .iter()
                .map(|path| zellij.close_tab(state, path))
                .collect(),
        ),
        Job::Pull(paths) => failures(
            paths
                .iter()
                .map(|path| {
                    let path = path.to_string_lossy();
                    runner
                        .output("git", &["-C", &path, "pull", "--ff-only"])
                        .map(drop)
                })
                .collect(),
        ),
        Job::Create {
            repo,
            branch,
            workspace,
            group,
        } => create(state, zellij, &repo, &branch, &workspace, &group).map(drop),
        Job::Start {
            repo,
            branch,
            workspace,
            issue,
        } => start(state, zellij, &repo, &branch, &workspace, &issue),
        Job::Checkout {
            repo,
            workspace,
            review,
        } => {
            // The title is a hint when the branch names no ticket.
            let mut group = group_from_name(&context.ticket, &review.branch);
            if group.is_empty() {
                group = group_from_name(&context.ticket, &review.title);
            }
            checkout(state, zellij, &repo, &review, &workspace, &group)
        }
        Job::NewCarnet {
            name,
            workspace,
            group,
        } => {
            let root = context.config.require_carnet_root()?;
            let path = carnet::create(
                state,
                runner,
                &context.names,
                &root,
                &name,
                &workspace,
                &group,
            )?;
            zellij.open_tab(state, &path).map(drop)
        }
        Job::Remove(removals) => failures(
            removals
                .iter()
                .map(|removal| remove(state, zellij, removal))
                .collect(),
        ),
        Job::Move { paths, workspace } => failures(
            paths
                .iter()
                .map(|path| {
                    let had_tab = state.tab(path)?.is_some();
                    zellij.close_tab(state, path)?;
                    state.set_workspace(path, &workspace)?;
                    if had_tab {
                        zellij.open_tab(state, path)?;
                    }
                    Ok(())
                })
                .collect(),
        ),
        Job::Regroup { paths, group } => {
            let mut repos = HashSet::new();
            for path in &paths {
                state.set_group(path, &group)?;
                match state.require_item(path)?.repo {
                    Some(repo) => drop(repos.insert(repo)),
                    None => zellij.rename_tab(state, path)?,
                }
            }
            for repo in repos {
                zellij.sync_names(state, &repo)?;
            }
            Ok(())
        }
        Job::SetAlias { repo, alias } => {
            state.update_repo(&repo.to_string_lossy(), Some(&alias), None)?;
            zellij.sync_names(state, &repo)
        }
        Job::SetRepoWorkspace { repo, workspace } => {
            state.update_repo(&repo.to_string_lossy(), None, Some(&workspace))
        }
        Job::Forget(repo) => {
            zellij.close_repo_tabs(state, &repo)?;
            state.remove_repo(&repo)
        }
        Job::AddWorkspace(name) => state.add_workspace(&name),
        Job::RemoveWorkspace { name, carnets } => state.remove_workspace(&name, &carnets),
        Job::SwitchWorkspace(name) => zellij.open_session(&name),
        Job::Browse(url) => browse(context, runner, &url),
    }
}

/// Creates a worktree through worktrunk, whose hooks record it and open its tab. Returns its
/// path.
fn create(
    state: &State,
    zellij: &Zellij,
    repo: &Path,
    branch: &str,
    workspace: &str,
    group: &str,
) -> Result<Option<PathBuf>> {
    let repo_arg = repo.to_string_lossy();
    // Succeeds either way, so a new branch does not show as a failure in the log.
    let exists = !zellij
        .runner
        .output("git", &["-C", &repo_arg, "branch", "--list", branch])?
        .is_empty();
    let target: &[&str] = if exists {
        &[branch]
    } else {
        &["--create", branch]
    };
    let path = switch(state, zellij, repo, target, branch, workspace, group)?;
    if let Some(path) = &path
        && state.tab(path)?.is_none()
    {
        zellij.open_tab(state, path)?;
    }
    Ok(path)
}

/// Creates a worktree for an issue and puts it in the issue's group, which links it to the
/// issue: the hooks take a group only from a ticket key, which a GitHub issue has none of.
fn start(
    state: &State,
    zellij: &Zellij,
    repo: &Path,
    branch: &str,
    workspace: &str,
    issue: &Issue,
) -> Result<()> {
    let path = create(state, zellij, repo, branch, workspace, &issue.key)?
        .ok_or_else(|| eyre!("wt switch left no worktree on {branch}"))?;
    if state.require_item(&path)?.group != issue.key {
        state.set_group(&path, &issue.key)?;
        zellij.sync_names(state, repo)?;
    }
    Ok(())
}

/// Checks out a review's branch through worktrunk (`pr:N` or `mr:N`) and focuses its tab,
/// whether the worktree is new or was there already.
fn checkout(
    state: &State,
    zellij: &Zellij,
    repo: &Path,
    review: &Review,
    workspace: &str,
    group: &str,
) -> Result<()> {
    let target = review.provider.shortcut(review.number);
    let branch = &review.branch;
    let path = switch(state, zellij, repo, &[&target], branch, workspace, group)?
        .ok_or_else(|| eyre!("wt switch {target} left no worktree on {branch}"))?;
    zellij.open_tab(state, &path)?;
    Ok(())
}

/// Runs `wt switch` with the workspace and group for atelier's hooks, then records the
/// worktree on `branch` in case the hooks are not installed. Returns its path.
fn switch(
    state: &State,
    zellij: &Zellij,
    repo: &Path,
    target: &[&str],
    branch: &str,
    workspace: &str,
    group: &str,
) -> Result<Option<PathBuf>> {
    let runner = zellij.runner;
    let repo_arg = repo.to_string_lossy();
    let workspace_env = format!("{}={workspace}", hooks::WORKSPACE_VAR);
    let group_env = format!("{}={group}", hooks::GROUP_HINT_VAR);
    let mut args = vec![
        workspace_env.as_str(),
        &group_env,
        "wt",
        "-C",
        &repo_arg,
        "switch",
    ];
    args.extend(target);
    args.extend(["--no-cd", "--yes"]);
    runner.output("env", &args)?;
    let listing = worktrunk::list(runner, repo, false)?;
    let Some(tree) = listing
        .worktrees
        .iter()
        .find(|tree| tree.branch.as_deref() == Some(branch))
    else {
        return Ok(None);
    };
    let path = sync::canonical(&tree.path);
    state.add_item(&path, ItemKind::Worktree, Some(repo), group, workspace)?;
    Ok(Some(path))
}

/// Removes a worktree through worktrunk, or forgets a carnet and leaves its folder.
fn remove(state: &State, zellij: &Zellij, removal: &Removal) -> Result<()> {
    if let Some(worktree) = &removal.worktree {
        let repo = worktree.repo.to_string_lossy();
        let path = removal.path.to_string_lossy();
        let target = worktree.branch.as_deref().unwrap_or(&path);
        let mut args = vec!["-C", &repo, "remove", "--foreground", "--yes"];
        if worktree.force {
            args.push("--force");
        }
        args.push(target);
        zellij.runner.output("wt", &args)?;
    }
    zellij.close_tab(state, &removal.path)?;
    state.remove_item(&removal.path)
}

fn browse(context: &Context, runner: &dyn Runner, url: &str) -> Result<()> {
    let opener = context.config.browser().unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "open".into()
        } else {
            "xdg-open".into()
        }
    });
    let mut words = opener.split_whitespace();
    let program = words
        .next()
        .ok_or_else(|| eyre!("the browser command is empty"))?;
    let mut args: Vec<&str> = words.collect();
    args.push(url);
    // Detached: a browser may not exit until its window closes.
    runner.spawn(program, &args)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::process::fake::Fake;
    use crate::tui::app::RemovedWorktree;

    fn zellij(fake: &Fake) -> Zellij<'_> {
        Zellij {
            runner: fake,
            here: Some("side".into()),
            layouts: Layouts {
                session: "S".into(),
                worktree: "W".into(),
            },
            anchor: "editor".into(),
            reconciled: Default::default(),
        }
    }

    const LISTING: &str = r#"{"repo":{"forge":{"url":"https://forge/r"}},"items":[
        {"branch":"main","worktree":{"path":"/r","main":true}},
        {"branch":"ABC-1-x","worktree":{"path":"/r.ABC-1-x"}}]}"#;

    #[test]
    fn load_puts_the_current_session_first_and_logs_failed_listings() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("side").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        state.add_repo("/broken", None, "default").unwrap();
        let fake = Fake::default()
            .always("wt -C /r ", Some(LISTING))
            .always("wt -C /broken", None);
        let ticket = Config::default().ticket_regex().unwrap();
        let (snapshot, problems) = load(&state, &zellij(&fake), &ticket, false, false).unwrap();
        assert_eq!(snapshot.workspaces, ["side", "default"]);
        let titles: Vec<_> = snapshot.work.iter().map(Work::title).collect();
        assert_eq!(titles, ["r:main", "r:ABC-1-x"]);
        assert_eq!(snapshot.forges[Path::new("/r")].url, "https://forge/r");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].command, "wt list in broken");
    }

    #[test]
    fn create_passes_the_workspace_and_group_to_the_hooks() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("side").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(LISTING))
            .always("zellij --session side action list-tabs", Some("[]"))
            .always("zellij --session side action new-tab", Some("4"))
            .always(
                "zellij --session side action list-panes",
                Some(r#"[{"id":7,"tab_id":4,"title":"editor","pane_cwd":"/r.ABC-1-x"}]"#),
            );
        create(
            &state,
            &zellij(&fake),
            Path::new("/r"),
            "ABC-1-x",
            "side",
            "ABC-1",
        )
        .unwrap();
        assert_eq!(
            state.tab("/r.ABC-1-x").unwrap().map(|tab| tab.session),
            Some("side".into()),
            "without hooks, create opens the tab itself"
        );
        assert!(fake.calls().contains(
            &"env ATELIER_WORKSPACE=side ATELIER_GROUP_HINT=ABC-1 wt -C /r switch --create ABC-1-x --no-cd --yes"
                .into()
        ));
        let item = state.require_item("/r.ABC-1-x").unwrap();
        assert_eq!(
            (item.workspace.as_str(), item.group.as_str()),
            ("side", "ABC-1")
        );
        let fake = Fake::default()
            .always("git -C /r branch", Some("  ABC-1-x"))
            .always("wt -C /r --config-set", Some(LISTING));
        create(
            &state,
            &zellij(&fake),
            Path::new("/r"),
            "ABC-1-x",
            "side",
            "",
        )
        .unwrap();
        assert!(fake.calls()[1].ends_with("switch ABC-1-x --no-cd --yes"));
        assert!(
            !fake.calls().iter().any(|call| call.contains("new-tab")),
            "the first create opened the tab"
        );
    }

    #[test]
    fn checkout_switches_to_the_review_and_focuses_its_tab() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().canonicalize().unwrap();
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_workspace("side").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        state
            .add_item(&tree, ItemKind::Worktree, Some(Path::new("/r")), "", "side")
            .unwrap();
        state
            .set_tab(&state::Tab {
                path: tree.clone(),
                session: "side".into(),
                tab_id: 4,
                pane_id: "7".into(),
            })
            .unwrap();
        let listing = format!(
            r#"{{"items":[{{"branch":"ABC-1-x","worktree":{{"path":"{}"}}}}]}}"#,
            tree.display()
        );
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(&listing))
            .always(
                "zellij --session side action list-tabs",
                Some(r#"[{"tab_id":4,"position":1,"name":"x"}]"#),
            )
            .always("zellij --session side action list-panes", Some("[]"));
        let run = |number: u64, branch: &str| {
            let review = Review {
                provider: Provider::GitHub,
                role: Role::ToReview,
                number,
                title: String::new(),
                url: String::new(),
                project: "o/r".into(),
                project_url: String::new(),
                author: String::new(),
                branch: branch.into(),
                base: "main".into(),
                draft: false,
                updated_at: String::new(),
            };
            checkout(
                &state,
                &zellij(&fake),
                Path::new("/r"),
                &review,
                "default",
                "ABC-1",
            )
        };
        run(12, "ABC-1-x").unwrap();
        let calls = fake.calls();
        assert_eq!(
            calls[0],
            "env ATELIER_WORKSPACE=default ATELIER_GROUP_HINT=ABC-1 wt -C /r switch pr:12 --no-cd --yes"
        );
        assert!(
            calls.contains(&"zellij --session side action go-to-tab-by-id 4".into()),
            "an existing worktree's tab is focused: {calls:?}"
        );
        let error = run(13, "gone").unwrap_err();
        assert!(error.to_string().contains("no worktree on gone"));
    }

    #[test]
    fn reviews_cover_every_host_and_role_through_the_cache() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        let gh = include_str!("../../tests/fixtures/gh-reviews.json");
        let fake = Fake::default()
            .always("gh api --hostname a", Some(gh))
            .always("gh api --hostname b", None);
        let recorder = Recorder::new(&fake);
        let hosts = ["a".to_owned(), "b".to_owned()];
        let (found, log) = reviews(&state, &recorder, Provider::GitHub, &hosts, false);
        assert_eq!(found.len(), 6, "both roles on host a");
        assert_eq!(log.len(), 2, "each failed command on host b, once");
        assert!(
            log.iter()
                .all(|entry| entry.command.starts_with("gh api --hostname b"))
        );
        assert_eq!(fake.calls().len(), 4);
        reviews(&state, &recorder, Provider::GitHub, &hosts, false);
        assert_eq!(fake.calls().len(), 6, "only the failed host is asked again");
        let fake = Fake::default().always("gh", Some("not json"));
        let recorder = Recorder::new(&fake);
        let (_, log) = reviews(&state, &recorder, Provider::GitHub, &["c".into()], false);
        assert_eq!(
            log.len(),
            2,
            "a parse error is logged though its command succeeded"
        );
        assert_eq!(log[0].command, "gh reviews");
        assert!(
            log[0]
                .error
                .as_ref()
                .unwrap()
                .contains("parsing gh reviews")
        );
    }

    #[test]
    fn issues_cover_every_scope_through_the_cache() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        let fake = Fake::default()
            .once("gh", Some(issues::tests::GH))
            .once("gh", Some("[]"))
            .always("gh", None);
        let recorder = Recorder::new(&fake);
        let config = TrackerConfig::default();
        let scopes = ["o/a".to_owned(), "o/b".to_owned()];
        let github = issues::Tracker::GitHub;
        let (found, log) = issues(&state, &recorder, &config, github, &scopes, false);
        assert_eq!(found.len(), 3, "o/a's open issues, and no closed ones");
        assert_eq!(log.len(), 1, "o/b's failed command");
        assert!(log[0].command.ends_with("-f owner=o -f name=b"));
        issues(&state, &recorder, &config, github, &scopes, false);
        assert_eq!(
            fake.calls().len(),
            4,
            "only the failed scope is asked again"
        );
        let fake = Fake::default().always("acli", Some("[{}]"));
        let recorder = Recorder::new(&fake);
        let jira = issues::Tracker::Jira;
        let (_, log) = issues(&state, &recorder, &config, jira, &["x".into()], false);
        assert_eq!(
            log.len(),
            1,
            "a parse error is logged though its command succeeded"
        );
        assert_eq!(log[0].command, "acli issues");
    }

    #[test]
    fn start_puts_the_new_worktree_in_the_issues_group() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        // As the hooks record it: a GitHub issue's branch names no ticket, so no group.
        state
            .add_item(
                "/r.5-fix",
                ItemKind::Worktree,
                Some(Path::new("/r")),
                "",
                "default",
            )
            .unwrap();
        state
            .set_tab(&state::Tab {
                path: "/r.5-fix".into(),
                session: "side".into(),
                tab_id: 4,
                pane_id: "7".into(),
            })
            .unwrap();
        let listing = r#"{"items":[{"branch":"5-fix","worktree":{"path":"/r.5-fix"}}]}"#;
        let fake = Fake::default()
            .always("wt -C /r --config-set", Some(listing))
            .always(
                "zellij --session side action list-tabs",
                Some(r#"[{"tab_id":4,"position":1,"name":"r:5-fix"}]"#),
            );
        let issue = issues::tests::issue("r#5", &[], false);
        start(
            &state,
            &zellij(&fake),
            Path::new("/r"),
            "5-fix",
            "default",
            &issue,
        )
        .unwrap();
        assert_eq!(state.require_item("/r.5-fix").unwrap().group, "r#5");
        assert!(
            fake.calls().iter().any(|call| call.contains("rename-tab")),
            "the tab is renamed for its group: {:?}",
            fake.calls()
        );
    }

    #[test]
    fn browse_spawns_the_configured_browser() {
        let context = Context {
            config: Config::parse("browser = \"firefox --new-tab\"").unwrap(),
            db: PathBuf::new(),
            layouts: Layouts {
                session: "S".into(),
                worktree: "W".into(),
            },
            ticket: Config::default().ticket_regex().unwrap(),
            names: carnet::Names::new(Config::default().ticket_pattern()).unwrap(),
        };
        let fake = Fake::default();
        browse(&context, &fake, "https://forge/r").unwrap();
        assert_eq!(fake.calls(), ["firefox --new-tab https://forge/r"]);
    }

    #[test]
    fn remove_forces_dirty_worktrees_and_forgets_them() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_repo("/r", None, "default").unwrap();
        state
            .add_item(
                "/r.x",
                ItemKind::Worktree,
                Some(Path::new("/r")),
                "",
                "default",
            )
            .unwrap();
        let fake = Fake::default();
        let removal = Removal {
            path: "/r.x".into(),
            worktree: Some(RemovedWorktree {
                repo: "/r".into(),
                branch: Some("x".into()),
                force: true,
            }),
        };
        remove(&state, &zellij(&fake), &removal).unwrap();
        assert_eq!(
            fake.calls(),
            ["wt -C /r remove --foreground --yes --force x"]
        );
        assert!(state.item("/r.x").unwrap().is_none());
    }

    #[test]
    fn load_lists_carnets_only_when_enabled() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("2026-10-01-notes");
        std::fs::create_dir(&notes).unwrap();
        let bare = dir.path().join("2026-10-02-bare");
        std::fs::create_dir(&bare).unwrap();
        for path in [&notes, &bare, &dir.path().join("gone")] {
            state
                .add_item(path, ItemKind::Carnet, None, "G-1", "default")
                .unwrap();
        }
        let fake = Fake::default();
        let ticket = Config::default().ticket_regex().unwrap();
        let (snapshot, _) = load(&state, &zellij(&fake), &ticket, false, false).unwrap();
        assert!(snapshot.work.is_empty());
        assert_eq!(
            snapshot.all_carnets.len(),
            2,
            "hidden carnets are still known"
        );
        let (snapshot, _) = load(&state, &zellij(&fake), &ticket, false, true).unwrap();
        let carnets: Vec<_> = (snapshot.work.iter())
            .map(|work| (work.title(), work.group.as_str()))
            .collect();
        assert_eq!(
            carnets,
            [
                ("2026-10-01-notes".into(), "G-1"),
                ("2026-10-02-bare".into(), "G-1")
            ],
            "a missing folder is forgotten"
        );
        assert!(snapshot.work.iter().all(Work::is_carnet));
    }

    #[test]
    fn removing_a_carnet_forgets_it_without_worktrunk() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        let dir = tempfile::tempdir().unwrap();
        state
            .add_item(dir.path(), ItemKind::Carnet, None, "", "default")
            .unwrap();
        let fake = Fake::default();
        let removal = Removal {
            path: dir.path().into(),
            worktree: None,
        };
        remove(&state, &zellij(&fake), &removal).unwrap();
        assert!(fake.calls().is_empty());
        assert!(state.item(dir.path()).unwrap().is_none());
        assert!(dir.path().exists());
    }
}
