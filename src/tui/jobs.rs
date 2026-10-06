//! Runs jobs off the UI thread, each with its own database connection, and reports actions.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Report, Result, eyre};

use super::app::{Action, Feed, Job, Pending, Readme, Rows};
use crate::carnet::{self, Carnets, Stamp};
use crate::config::Config;
use crate::finish::{self, Scope};
use crate::git;
use crate::issues::{self, TrackerConfig};
use crate::items::{Items, Snapshot};
use crate::process::{Logged, Recorder, Runner, System};
use crate::reviews::{self, Role};
use crate::state::{self, State};
use crate::zellij::{Layouts, Zellij};

/// What every job needs, shared across them.
pub struct Context {
    pub config: Config,
    pub db: PathBuf,
    pub layouts: Layouts,
}

impl Context {
    /// Creates and migrates the database once; jobs then only connect to it.
    pub fn new(config: Config) -> Result<Self> {
        let db = state::db_path();
        State::open(&db, config.default_workspace())?;
        Ok(Self {
            layouts: Layouts::resolve(&config)?,
            db,
            config,
        })
    }

    fn zellij<'a>(&self, runner: &'a dyn Runner) -> Zellij<'a> {
        Zellij::new(runner, &self.config, self.layouts.clone())
    }

    fn items<'a>(&'a self, state: &'a State, runner: &'a dyn Runner) -> Result<Items<'a>> {
        Items::new(state, runner, &self.config, self.layouts.clone())
    }

    fn state(&self) -> Result<State> {
        State::connect(&self.db, self.config.default_workspace())
    }
}

pub fn run(context: &Context, job: Job) -> Action {
    let recorder = Recorder::new(&System);
    match job {
        // Refreshes and commit listings run constantly: log only their failures.
        Job::Refresh { full } => {
            let loaded = context
                .state()
                .and_then(|state| context.items(&state, &recorder)?.snapshot(full));
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
            let lines = git::log(&recorder, &path).unwrap_or_default();
            Action::Commits(path, lines)
        }
        Job::Readme(path) => {
            let stamp = Stamp::of(&path);
            let text = std::fs::read_to_string(path.join("README.md")).ok();
            Action::Readme(Readme { path, stamp, text })
        }
        Job::ExportLog(ref entries) => {
            let directory = crate::config::state_home().join("atelier/logs");
            let entry = match crate::process::export_log(&directory, entries) {
                Ok(path) => Logged {
                    command: format!("export command log to {}", path.display()),
                    error: None,
                },
                Err(err) => Logged {
                    command: "export command log".into(),
                    error: Some(format!("{err:#}")),
                },
            };
            Action::Finished {
                job,
                log: vec![entry],
                error: None,
            }
        }
        Job::SearchCarnets(text) => {
            let hits =
                Carnets::new(&context.config).and_then(|carnets| carnets.hits(&recorder, &text));
            let mut log = recorder.take();
            match &hits {
                // `rg` finding nothing is no failure.
                Ok(_) => log.iter_mut().for_each(|entry| entry.error = None),
                Err(err) if log.is_empty() => log.push(Logged {
                    command: "carnet search".into(),
                    error: Some(err.to_string()),
                }),
                Err(_) => {}
            }
            Action::Searched {
                text,
                hits: hits.ok(),
                log,
            }
        }
        Job::Plan { scope, repos } => {
            let (plan, log) = plan(context, &recorder, &scope, &repos);
            Action::Planned {
                plan: plan.map_err(|err| err.to_string()),
                log,
            }
        }
        Job::LinkedGroup(pending) => {
            let group = context
                .state()
                .and_then(|state| {
                    let items = context.items(&state, &recorder)?;
                    items.linked_group(&pending.issue_keys(&items))
                })
                .map_err(|err| err.to_string());
            Action::Linked {
                pending,
                group,
                log: recorder.take(),
            }
        }
        // Like refreshes, these run constantly: log only failures.
        Job::Fetch { feed, keys, force } => {
            let config = &context.config.tracker;
            let (rows, log) = match context.state() {
                Ok(state) => {
                    let (rows, log) = fetch(&state, &recorder, config, feed, &keys, force);
                    (Ok(rows), log)
                }
                Err(err) => (Err(err.to_string()), Vec::new()),
            };
            Action::Fetched { feed, rows, log }
        }
        job => {
            let error = context
                .state()
                .and_then(|state| execute(context, &state, &recorder, job.clone()))
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

/// Fetches each repo, logging every fetch, then builds the plan from a fresh snapshot, logging
/// only its failures as a refresh does. A failed fetch leaves the last known state, which the
/// plan warns of.
fn plan(
    context: &Context,
    recorder: &Recorder,
    scope: &Scope,
    repos: &[PathBuf],
) -> (Result<finish::Plan>, Vec<Logged>) {
    let mut log = Vec::new();
    let plan = context.state().and_then(|state| {
        let items = context.items(&state, recorder)?;
        let failed = items.fetch(repos);
        log.extend(recorder.take());
        let (snapshot, problems) = items.snapshot(false)?;
        log.extend(problems);
        let names: Vec<String> = (failed.iter())
            .map(|repo| repo_name(&snapshot, repo))
            .collect();
        Ok(finish::plan(&snapshot, scope, &names))
    });
    log.extend((recorder.take().into_iter()).filter(|entry| entry.error.is_some()));
    (plan, log)
}

fn repo_name(snapshot: &Snapshot, path: &Path) -> String {
    (snapshot.repos.iter())
        .find(|repo| repo.path == path)
        .map_or_else(|| state::dir_name(path), |repo| repo.name())
}

/// Attaches to a session from outside zellij; the caller hands over the terminal.
pub fn attach(context: &Context, session: &str) -> Vec<Logged> {
    let recorder = Recorder::new(&System);
    let _ = context.zellij(&recorder).open_session(session);
    recorder.take()
}

/// Runs the configured tool in `path`; without a branch, as for a carnet, reads its current one.
/// Outside zellij the caller hands over the terminal.
pub fn tool(context: &Context, path: &Path, branch: Option<String>) -> Vec<Logged> {
    let recorder = Recorder::new(&System);
    let branch = branch
        .or_else(|| git::branch(&recorder, path))
        .unwrap_or_default();
    let command = crate::tool::command(context.config.tool(), path, &branch);
    let program = crate::tool::program(&command).to_owned();
    if !carnet::on_path(&program) {
        let mut log = recorder.take();
        log.push(Logged {
            command,
            error: Some(format!("command '{program}' not found on PATH")),
        });
        return log;
    }
    let _ = context.zellij(&recorder).run_tool(path, &command);
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

/// A feed's rows in every key, through the cache, with what failed for the command log: each
/// host's reviews in both roles, or each scope's issues.
fn fetch(
    state: &State,
    recorder: &Recorder,
    config: &TrackerConfig,
    feed: Feed,
    keys: &[String],
    force: bool,
) -> (Rows, Vec<Logged>) {
    let mut reviews = Vec::new();
    let mut issues = Vec::new();
    let mut log = Vec::new();
    for key in keys {
        match feed {
            Feed::Reviews(provider) => {
                let api = provider.reviews(recorder, key.clone());
                for role in Role::ALL {
                    let (found, error) = reviews::fetch(state, api.as_ref(), role, force);
                    reviews.extend(found);
                    log.extend(fetch_failures(recorder, error, feed.what()));
                }
            }
            Feed::Issues(tracker) => {
                let api = tracker.issues(recorder, key.clone(), config);
                let (found, error) = issues::fetch(state, api.as_ref(), force);
                issues.extend(found);
                log.extend(fetch_failures(recorder, error, feed.what()));
            }
        }
    }
    let rows = match feed {
        Feed::Reviews(_) => Rows::Reviews(reviews),
        Feed::Issues(_) => Rows::Issues(issues),
    };
    (rows, log)
}

/// Runs an action job through the item operations.
fn execute(context: &Context, state: &State, runner: &dyn Runner, job: Job) -> Result<()> {
    let items = context.items(state, runner)?;
    match job {
        Job::Refresh { .. }
        | Job::Commits(_)
        | Job::Readme(_)
        | Job::SearchCarnets(_)
        | Job::ExportLog(_)
        | Job::Plan { .. }
        | Job::LinkedGroup(_)
        | Job::Fetch { .. } => {
            unreachable!("run handles these")
        }
        Job::Open(paths) => items.open(&paths),
        Job::Close(paths) => items.close(&paths),
        Job::Pull(paths) => items.pull(&paths),
        Job::Create {
            repo,
            branch,
            workspace,
            group,
        } => items
            .create(&repo, &branch, &workspace, group.as_ref())
            .map(drop),
        Job::Make { pending, group } => match pending {
            Pending::Start {
                repo,
                branch,
                workspace,
                issue,
            } => items.start(&repo, &branch, &workspace, &issue.key, group.as_ref()),
            Pending::Checkout {
                repo,
                workspace,
                review,
            } => items.checkout(&repo, &workspace, &review, group.as_ref()),
        },
        Job::NewCarnet {
            name,
            workspace,
            group,
        } => {
            let path = items.create_carnet(&name, &workspace, group.as_ref())?;
            items.open(&[path])
        }
        Job::Remove(removals) => items.remove(&removals),
        Job::Finish(steps) => items.finish(&steps),
        Job::CloseCarnet(paths) => items.set_carnets_closed(&paths, true),
        Job::ReopenCarnet(paths) => items.set_carnets_closed(&paths, false),
        Job::Move { paths, workspace } => items.move_to(&paths, &workspace),
        Job::Regroup { paths, group } => items.regroup(&paths, group.as_ref()),
        Job::SetIssueKeys { path, issue_keys } => items.set_issue_keys(&path, &issue_keys),
        Job::SetAlias { repo, alias } => items.set_alias(&repo, &alias),
        Job::SetRepoWorkspace { repo, workspace } => items.set_repo_workspace(&repo, &workspace),
        Job::Forget(repo) => items.forget_repo(&repo),
        Job::AddWorkspace(name) => items.add_workspace(&name),
        Job::RemoveWorkspace(name) => items.remove_workspace(&name),
        Job::SwitchWorkspace(name) => context.zellij(runner).open_session(&name),
        Job::Browse(url) => browse(context, runner, &url),
    }
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
    use crate::reviews::Provider;

    #[test]
    fn reviews_cover_every_host_and_role_through_the_cache() {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        let gh = include_str!("../../tests/fixtures/gh-reviews.json");
        let fake = Fake::default()
            .always("gh api --hostname a", Some(gh))
            .always("gh api --hostname b", None);
        let recorder = Recorder::new(&fake);
        let config = TrackerConfig::default();
        let github = Feed::Reviews(Provider::GitHub);
        let hosts = ["a".to_owned(), "b".to_owned()];
        let (Rows::Reviews(found), log) = fetch(&state, &recorder, &config, github, &hosts, false)
        else {
            panic!("reviews");
        };
        assert_eq!(found.len(), 6, "both roles on host a");
        assert_eq!(log.len(), 2, "each failed command on host b, once");
        assert!(
            log.iter()
                .all(|entry| entry.command.starts_with("gh api --hostname b"))
        );
        assert_eq!(fake.calls().len(), 4);
        fetch(&state, &recorder, &config, github, &hosts, false);
        assert_eq!(fake.calls().len(), 6, "only the failed host is asked again");
        let fake = Fake::default().always("gh", Some("not json"));
        let recorder = Recorder::new(&fake);
        let (_, log) = fetch(&state, &recorder, &config, github, &["c".into()], false);
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
        let github = Feed::Issues(issues::Tracker::GitHub);
        let (Rows::Issues(found), log) = fetch(&state, &recorder, &config, github, &scopes, false)
        else {
            panic!("issues");
        };
        assert_eq!(found.len(), 3, "o/a's open issues, and no closed ones");
        assert_eq!(log.len(), 1, "o/b's failed command");
        assert!(log[0].command.ends_with("-f owner=o -f name=b"));
        fetch(&state, &recorder, &config, github, &scopes, false);
        assert_eq!(
            fake.calls().len(),
            4,
            "only the failed scope is asked again"
        );
        let fake = Fake::default().always("acli", Some("[{}]"));
        let recorder = Recorder::new(&fake);
        let jira = Feed::Issues(issues::Tracker::Jira);
        let (_, log) = fetch(&state, &recorder, &config, jira, &["x".into()], false);
        assert_eq!(
            log.len(),
            1,
            "a parse error is logged though its command succeeded"
        );
        assert_eq!(log[0].command, "acli issues");
    }

    #[test]
    fn a_failed_fetch_still_plans_from_the_last_known_state() {
        let dir = tempfile::tempdir().unwrap();
        let context = Context {
            config: Config::parse("").unwrap(),
            db: dir.path().join("atelier.db"),
            layouts: crate::zellij::layouts(),
        };
        State::open(&context.db, "default").unwrap();
        let fake = Fake::default().always("git -C /src/api fetch", None);
        let recorder = Recorder::new(&fake);
        let scope = Scope::Workspace("default".into());
        let (plan, log) = plan(&context, &recorder, &scope, &["/src/api".into()]);
        let plan = plan.unwrap();
        assert_eq!(
            plan.lines[0],
            finish::Line::Warning("fetch failed in api: showing last known state".into())
        );
        assert_eq!(log[0].command, "git -C /src/api fetch --prune");
        assert!(log[0].error.is_some(), "the failure is logged");
    }

    #[test]
    fn browse_spawns_the_configured_browser() {
        let context = Context {
            config: Config::parse("browser = \"firefox --new-tab\"").unwrap(),
            db: PathBuf::new(),
            layouts: crate::zellij::layouts(),
        };
        let fake = Fake::default();
        browse(&context, &fake, "https://forge/r").unwrap();
        assert_eq!(fake.calls(), ["firefox --new-tab https://forge/r"]);
    }
}
