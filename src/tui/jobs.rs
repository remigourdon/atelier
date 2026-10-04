//! Runs jobs off the UI thread, each with its own database connection, and reports actions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};
use regex::Regex;

use super::app::{Action, Job, LogEntry, Removal, Snapshot, Work};
use crate::config::Config;
use crate::process::{Recorder, Runner, System};
use crate::state::{self, State};
use crate::zellij::{self, Layouts, Zellij};
use crate::{sync, worktrunk};

/// What every job needs, shared across them.
pub struct Context {
    pub config: Config,
    pub db: PathBuf,
    pub layouts: Layouts,
    pub ticket: Regex,
}

impl Context {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            layouts: Layouts::resolve(&config)?,
            ticket: config.ticket_regex()?,
            db: state::db_path(),
            config,
        })
    }

    fn zellij<'a>(&self, runner: &'a dyn Runner) -> Zellij<'a> {
        Zellij {
            runner,
            here: zellij::current_session(),
            layouts: self.layouts.clone(),
            anchor: self.config.anchor_pane().to_owned(),
        }
    }

    fn state(&self) -> Result<State> {
        State::open(&self.db, self.config.default_workspace())
    }
}

pub fn run(context: &Context, job: Job) -> Action {
    let recorder = Recorder::new(&System);
    let zellij = context.zellij(&recorder);
    let log = |recorder: &Recorder| -> Vec<LogEntry> {
        recorder.take().into_iter().map(Into::into).collect()
    };
    match job {
        // Refreshes and commit listings run constantly: log only their failures.
        Job::Refresh { full } => {
            let loaded = context
                .state()
                .and_then(|state| load(&state, &zellij, &context.ticket, full));
            let mut log: Vec<LogEntry> = log(&recorder)
                .into_iter()
                .filter(|entry| entry.error.is_some())
                .collect();
            let snapshot = loaded
                .map(|(snapshot, problems)| {
                    log.extend(problems);
                    snapshot
                })
                .map_err(|err| err.to_string());
            Action::Loaded { snapshot, log }
        }
        Job::Commits(path) => {
            let lines = commits(&recorder, &path).unwrap_or_default();
            Action::Commits(path, lines)
        }
        job => {
            let error = context
                .state()
                .and_then(|mut state| execute(context, &mut state, &zellij, job.clone()))
                .err()
                .map(|err| err.to_string());
            Action::Finished {
                job,
                log: log(&recorder),
                error,
            }
        }
    }
}

/// Attaches to a session from outside zellij; the caller hands over the terminal.
pub fn attach(context: &Context, session: &str) -> Vec<LogEntry> {
    let recorder = Recorder::new(&System);
    let _ = context.zellij(&recorder).open_session(session);
    recorder.take().into_iter().map(Into::into).collect()
}

fn commits(runner: &dyn Runner, path: &Path) -> Result<Vec<String>> {
    let path = path.to_string_lossy();
    let log = runner.output(
        "git",
        &["-C", &path, "log", "-n", "20", "--format=%h %s (%cr, %an)"],
    )?;
    Ok(log.lines().map(Into::into).collect())
}

/// Syncs the recorded items with worktrunk and gathers what the panels show,
/// with the repos that could not be listed.
pub fn load(
    state: &State,
    zellij: &Zellij,
    ticket: &Regex,
    full: bool,
) -> Result<(Snapshot, Vec<LogEntry>)> {
    // A reconcile failure (zellij not running) should not hide the worktrees.
    let _ = zellij.reconcile(state);
    let synced = sync::sync(state, zellij.runner, ticket, full)?;
    let problems = synced
        .failures
        .iter()
        .map(|(repo, err)| LogEntry {
            command: format!("wt list in {}", repo.name()),
            error: Some(format!("{err:#}")),
        })
        .collect();
    let mut work = Vec::new();
    for tracked in synced.worktrees {
        work.push(Work {
            tab: state.tab(&tracked.tree.path)?.is_some(),
            repo_name: tracked.repo.name(),
            repo: tracked.repo.path,
            workspace: tracked.item.workspace,
            group: tracked.item.group,
            tree: tracked.tree,
        });
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
        Job::Refresh { .. } | Job::Commits(_) => unreachable!("run handles these"),
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
        } => create(state, zellij, &repo, &branch, &workspace, &group),
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
                repos.extend(state.require_item(path)?.repo);
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
        Job::SwitchWorkspace(name) => zellij.open_session(&name),
        Job::Browse(url) => browse(context, runner, &url),
    }
}

/// Creates a worktree through worktrunk, whose hooks record it and open its tab.
fn create(
    state: &State,
    zellij: &Zellij,
    repo: &Path,
    branch: &str,
    workspace: &str,
    group: &str,
) -> Result<()> {
    let runner = zellij.runner;
    let repo_arg = repo.to_string_lossy();
    // Succeeds either way, so a new branch does not show as a failure in the log.
    let exists = !runner
        .output("git", &["-C", &repo_arg, "branch", "--list", branch])?
        .is_empty();
    let workspace_env = format!("ATELIER_WORKSPACE={workspace}");
    let group_env = format!("ATELIER_GROUP_HINT={group}");
    let mut args = vec![
        workspace_env.as_str(),
        &group_env,
        "wt",
        "-C",
        &repo_arg,
        "switch",
    ];
    if !exists {
        args.push("--create");
    }
    args.extend([branch, "--no-cd", "--yes"]);
    runner.output("env", &args)?;
    // Without atelier's hooks installed nothing recorded it or opened its tab: do it here.
    let listing = worktrunk::list(runner, repo, false)?;
    if let Some(tree) = listing
        .worktrees
        .iter()
        .find(|tree| tree.branch.as_deref() == Some(branch))
    {
        let path = sync::canonical(&tree.path);
        state.add_item(&path, "worktree", Some(repo), group, workspace)?;
        if state.tab(&path)?.is_none() {
            zellij.open_tab(state, &path)?;
        }
    }
    Ok(())
}

fn remove(state: &State, zellij: &Zellij, removal: &Removal) -> Result<()> {
    let repo = removal.repo.to_string_lossy();
    let path = removal.path.to_string_lossy();
    let target = removal.branch.as_deref().unwrap_or(&path);
    let mut args = vec!["-C", &repo, "remove", "--foreground", "--yes"];
    if removal.force {
        args.push("--force");
    }
    args.push(target);
    zellij.runner.output("wt", &args)?;
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

    fn zellij(fake: &Fake) -> Zellij<'_> {
        Zellij {
            runner: fake,
            here: Some("side".into()),
            layouts: Layouts {
                session: "S".into(),
                worktree: "W".into(),
            },
            anchor: "editor".into(),
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
        let (snapshot, problems) = load(&state, &zellij(&fake), &ticket, false).unwrap();
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
    fn browse_spawns_the_configured_browser() {
        let context = Context {
            config: Config::parse("browser = \"firefox --new-tab\"").unwrap(),
            db: PathBuf::new(),
            layouts: Layouts {
                session: "S".into(),
                worktree: "W".into(),
            },
            ticket: Config::default().ticket_regex().unwrap(),
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
            .add_item("/r.x", "worktree", Some(Path::new("/r")), "", "default")
            .unwrap();
        let fake = Fake::default();
        let removal = Removal {
            repo: "/r".into(),
            path: "/r.x".into(),
            branch: Some("x".into()),
            force: true,
        };
        remove(&state, &zellij(&fake), &removal).unwrap();
        assert_eq!(
            fake.calls(),
            ["wt -C /r remove --foreground --yes --force x"]
        );
        assert!(state.item("/r.x").unwrap().is_none());
    }
}
