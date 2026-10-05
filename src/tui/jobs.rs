//! Runs jobs off the UI thread, each with its own database connection, and reports actions.

use std::path::PathBuf;

use color_eyre::eyre::{Report, Result, eyre};

use super::app::{Action, Job};
use crate::config::Config;
use crate::issues::{self, Issue, TrackerConfig};
use crate::items::Items;
use crate::process::{Logged, Recorder, Runner, System};
use crate::reviews::{self, Provider, Review, Role};
use crate::state::{self, State};
use crate::zellij::{Layouts, Zellij};
use crate::{carnet, git};

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
            let readme = std::fs::read_to_string(path.join("README.md")).ok();
            Action::Readme(path, readme)
        }
        Job::SearchCarnets(text) => {
            let hits = match context.config.carnet_root() {
                Some(root) => carnet::hits(&recorder, &root, &text),
                None => Err(eyre!("carnets are disabled")),
            };
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

/// Runs an action job through the item operations.
fn execute(context: &Context, state: &State, runner: &dyn Runner, job: Job) -> Result<()> {
    let items = context.items(state, runner)?;
    match job {
        Job::Refresh { .. }
        | Job::Commits(_)
        | Job::Readme(_)
        | Job::SearchCarnets(_)
        | Job::Reviews { .. }
        | Job::Issues { .. } => {
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
        } => items.create(&repo, &branch, &workspace, &group).map(drop),
        Job::Start {
            repo,
            branch,
            workspace,
            issue,
        } => items.start(&repo, &branch, &workspace, &issue.key),
        Job::Checkout {
            repo,
            workspace,
            review,
        } => items.checkout(&repo, &workspace, &review),
        Job::NewCarnet {
            name,
            workspace,
            group,
        } => {
            let path = items.create_carnet(&name, &workspace, &group)?;
            items.open(&[path])
        }
        Job::Remove(removals) => items.remove(&removals),
        Job::CloseCarnet(paths) => items.set_carnets_closed(&paths, true),
        Job::ReopenCarnet(paths) => items.set_carnets_closed(&paths, false),
        Job::Move { paths, workspace } => items.move_to(&paths, &workspace),
        Job::Regroup { paths, group } => items.regroup(&paths, &group),
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
