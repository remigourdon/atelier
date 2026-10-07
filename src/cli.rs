//! The command line.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use color_eyre::eyre::{Result, WrapErr, bail};

use crate::carnet::Carnets;
use crate::config::Config;
use crate::context::{self, Target};
use crate::git;
use crate::hooks::{self, Phase};
use crate::items::Items;
use crate::links::{Group, IssueKeys, Links, group_text};
use crate::process::System;
use crate::state::{self, State};
use crate::worktrunk::{self, HooksConfig};
use crate::zellij::{Layouts, Zellij};

/// A lazygit-style TUI and CLI that organise git worktrees into zellij sessions.
#[derive(Parser)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage workspaces.
    #[command(subcommand)]
    Ws(Ws),
    /// Register a repository.
    Add {
        /// Any worktree of the repository.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// A short name for the repository.
        #[arg(short, long)]
        alias: Option<String>,
        /// The workspace its new worktrees go to (default: the default workspace).
        #[arg(short, long, add = ArgValueCandidates::new(complete_workspaces))]
        workspace: Option<String>,
    },
    /// Change a repository's alias or default workspace.
    Update {
        #[arg(add = ArgValueCandidates::new(complete_repos))]
        repo: String,
        /// A new alias; empty clears it.
        #[arg(short, long)]
        alias: Option<String>,
        #[arg(short, long, add = ArgValueCandidates::new(complete_workspaces))]
        workspace: Option<String>,
    },
    /// Forget a repository and close its tabs. Its worktrees stay on disk.
    Rm {
        #[arg(add = ArgValueCandidates::new(complete_repos))]
        repo: String,
    },
    /// List repositories: name, default workspace and path.
    Ls,
    /// Switch to a workspace's session, creating it when needed.
    Open {
        #[arg(add = ArgValueCandidates::new(complete_workspaces))]
        workspace: String,
    },
    /// Describe a directory's worktree or carnet: its workspace, group, issues and reviews,
    /// the worktrees and carnets of its group or sharing its issue keys, and the carnet to
    /// write notes in. Reads atelier's records, the caches and `wt list`; changes nothing.
    Context {
        /// A directory inside the item to describe.
        #[arg(default_value = ".", conflicts_with = "issue_key")]
        path: PathBuf,
        /// Describe an issue key's issue, linked work and reviews instead.
        #[arg(short, long)]
        issue_key: Option<String>,
        /// Print JSON, for scripts and coding agents.
        #[arg(long)]
        json: bool,
    },
    /// Print one ANSI line for zjstatus about the worktree or carnet holding the current
    /// directory: its group, its repo or `carnet`, worktrunk's cells, then its issue keys.
    /// Empty outside one.
    Statusline,
    /// Manage carnets, the investigation folders under `[carnets] root`.
    #[command(subcommand)]
    Carnet(Carnet),
    /// Open the lazygit-style interface.
    Tui,
    /// Manage atelier's hooks in worktrunk's user config.
    #[command(subcommand)]
    Hooks(Hooks),
    /// Shell integration.
    #[command(subcommand)]
    Shell(Shell),
    /// Run by worktrunk with the hook context on stdin.
    #[command(hide = true)]
    Hook { phase: Phase },
}

#[derive(Subcommand)]
enum Ws {
    /// Create a workspace.
    Add { name: String },
    /// Remove a workspace that owns no worktree; its carnets move to the default workspace.
    Rm {
        #[arg(add = ArgValueCandidates::new(complete_workspaces))]
        name: String,
    },
    /// List workspaces.
    Ls,
}

#[derive(Subcommand)]
enum Carnet {
    /// Create a carnet `<root>/YYYY-MM-DD-<name>`: a git repo with a README. Prints its path.
    New {
        /// Its name, which becomes its folder name; it links nothing.
        name: String,
        /// Its workspace (default: the current session's, else the default workspace).
        #[arg(short, long, add = ArgValueCandidates::new(complete_workspaces))]
        workspace: Option<String>,
        /// Its group.
        #[arg(short, long)]
        group: Option<String>,
        /// An issue key it links; repeat for more.
        #[arg(short, long = "issues", value_name = "ISSUE_KEY")]
        issues: Vec<String>,
        /// Its one-line summary.
        #[arg(short, long)]
        summary: Option<String>,
        /// Print the new carnet as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change the group, issue keys or summary of the carnet holding a directory, in one
    /// commit. Only the flags given change.
    Set {
        /// A directory inside the carnet.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Its group; empty for none.
        #[arg(short, long)]
        group: Option<String>,
        /// The issue keys it links, replacing them all; repeat for more.
        #[arg(short, long = "issues", value_name = "ISSUE_KEY")]
        issues: Vec<String>,
        /// Its one-line summary.
        #[arg(short, long)]
        summary: Option<String>,
    },
    /// Close the carnet holding a directory, and its tab: its investigation is over.
    Close {
        /// A directory inside the carnet.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Reopen the closed carnet holding a directory.
    Reopen {
        /// A directory inside the carnet.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// List open carnets, newest first: folder, group, issue keys and summary.
    Ls {
        /// Include closed carnets.
        #[arg(long)]
        closed: bool,
    },
    /// Search every carnet for a text with ripgrep.
    Search { text: String },
}

#[derive(Subcommand)]
enum Hooks {
    /// Add atelier's hooks to worktrunk's user config.
    Install,
    /// Remove atelier's hooks from worktrunk's user config.
    Uninstall,
    /// Show which hooks are installed.
    Status,
}

#[derive(Subcommand)]
enum Shell {
    /// Print the shell integration: the `wt` wrapper and completions.
    Init { shell: ShellKind },
}

#[derive(Clone, ValueEnum)]
enum ShellKind {
    Fish,
}

pub fn run() -> Result<()> {
    clap_complete::CompleteEnv::with_factory(<Cli as clap::CommandFactory>::command).complete();
    let cli = Cli::parse();
    match cli.command {
        Command::Shell(Shell::Init {
            shell: ShellKind::Fish,
        }) => {
            print!("{}", crate::shell::FISH);
            Ok(())
        }
        Command::Hooks(command) => run_hooks(command),
        Command::Tui => crate::tui::run(Config::load()?),
        Command::Context {
            path,
            issue_key,
            json,
        } => {
            let config = Config::load()?;
            // Read-only: describing a directory must not create or migrate the database.
            let state = State::read(&state::db_path(), config.default_workspace())?;
            let target = match &issue_key {
                Some(key) => Target::IssueKey(key),
                None => Target::Dir(&path),
            };
            let here = crate::zellij::current_session();
            let context = context::describe(&state, &config, &System, here.as_deref(), target)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&context)?);
            } else {
                print!("{}", context::render(&context, &config.tracker));
            }
            Ok(())
        }
        Command::Statusline => {
            let config = Config::load()?;
            // Read-only, as for `context`: drawing a status bar must not create the database.
            let state = State::read(&state::db_path(), config.default_workspace())?;
            let dir = std::env::current_dir()?;
            let line = crate::tui::statusline::line(&state, &config, &System, &dir)?;
            if !line.is_empty() {
                println!("{line}");
            }
            Ok(())
        }
        Command::Hook { phase } => {
            // A hook must never abort worktrunk: report and succeed.
            if let Err(err) = run_hook(phase) {
                report_hook_error(&err);
            }
            Ok(())
        }
        command => {
            let config = Config::load()?;
            let state = State::open(&state::db_path(), config.default_workspace())?;
            let edits = edits_items(&command);
            let ran = run_state(command, &config, &state);
            // A failed edit may still have changed some items.
            if edits {
                name_tabs(&state, &config);
            }
            ran
        }
    }
}

/// Whether `command` changes what names a tab: an item's links, workspace or tab, or a repo's
/// name.
fn edits_items(command: &Command) -> bool {
    matches!(
        command,
        Command::Ws(Ws::Rm { .. })
            | Command::Update { .. }
            | Command::Rm { .. }
            | Command::Carnet(Carnet::Set { .. })
            | Command::Carnet(Carnet::Close { .. })
            | Command::Carnet(Carnet::Reopen { .. })
    )
}

/// Names the open tabs after an edit, which stands whether or not they can be renamed: a tab
/// whose session is gone gets its name when reopened.
fn name_tabs(state: &State, config: &Config) {
    if let Err(err) = items(state, config).and_then(|items| items.name_tabs()) {
        eprintln!("atelier: could not rename the tabs: {err:#}");
    }
}

fn items<'a>(state: &'a State, config: &'a Config) -> Result<Items<'a>> {
    Items::new(state, &System, config, Layouts::resolve(config)?)
}

fn run_state(command: Command, config: &Config, state: &State) -> Result<()> {
    match command {
        Command::Ws(Ws::Add { name }) => state.add_workspace(&name),
        Command::Ws(Ws::Rm { name }) => items(state, config)?.remove_workspace(&name),
        Command::Ws(Ws::Ls) => {
            for name in state.workspaces()? {
                println!("{name}");
            }
            Ok(())
        }
        Command::Add {
            path,
            alias,
            workspace,
        } => {
            let root = git::main_worktree(&System, &path)?;
            let workspace = workspace
                .as_deref()
                .unwrap_or(state.default_workspace())
                .to_owned();
            state.add_repo(&root, alias.as_deref(), &workspace)?;
            println!("registered {} in {workspace}", root.display());
            Ok(())
        }
        Command::Update {
            repo,
            alias,
            workspace,
        } => {
            if alias.is_none() && workspace.is_none() {
                bail!("provide --alias, --workspace, or both");
            }
            let path = state.repo(&repo)?.path;
            items(state, config)?.update_repo(&path, alias.as_deref(), workspace.as_deref())
        }
        Command::Rm { repo } => {
            let path = state.repo(&repo)?.path;
            items(state, config)?.forget_repo(&path)
        }
        Command::Ls => {
            for repo in state.repos()? {
                println!(
                    "{}\t{}\t{}",
                    repo.name(),
                    repo.default_workspace,
                    repo.path.display()
                );
            }
            Ok(())
        }
        Command::Open { workspace } => {
            state.require_workspace(&workspace)?;
            Zellij::new(&System, config, Layouts::resolve(config)?).open_session(&workspace)
        }
        Command::Carnet(command) => {
            let carnets = Carnets::new(config)?;
            // Disabled carnets are an error here, not an empty list.
            carnets.root()?;
            match command {
                Carnet::New {
                    name,
                    workspace,
                    group,
                    issues,
                    summary,
                    json,
                } => {
                    let items = items(state, config)?;
                    // The workspace given, which must exist, else the current session's when
                    // it is one, else the default workspace.
                    if let Some(workspace) = &workspace {
                        state.require_workspace(workspace)?;
                    }
                    let workspace =
                        items.workspace(workspace.as_deref(), state.default_workspace());
                    let links = Links {
                        group: group.as_deref().and_then(Group::parse),
                        issue_keys: resolve(&issues, config),
                    };
                    let summary = summary.as_deref().unwrap_or_default();
                    let carnet = items.create_carnet(&name, &workspace, &links, summary)?;
                    if json {
                        let record = context::CarnetInfo::new(&carnet, workspace, None);
                        println!("{}", serde_json::to_string_pretty(&record)?);
                    } else {
                        println!("{}", carnet.path.display());
                    }
                }
                Carnet::Set {
                    path,
                    group,
                    issues,
                    summary,
                } => {
                    let carnet = holding_carnet(state, config, &path)?;
                    let issue_keys = (!issues.is_empty()).then(|| resolve(&issues, config));
                    items(state, config)?.amend_carnet(&carnet, |links, kept| {
                        if let Some(group) = &group {
                            links.group = Group::parse(group);
                        }
                        if let Some(issue_keys) = issue_keys {
                            links.issue_keys = issue_keys;
                        }
                        if let Some(summary) = summary {
                            *kept = summary;
                        }
                    })?;
                }
                Carnet::Close { path } => {
                    let carnet = holding_carnet(state, config, &path)?;
                    items(state, config)?.set_carnets_closed(&[carnet], true)?;
                }
                Carnet::Reopen { path } => {
                    let carnet = holding_carnet(state, config, &path)?;
                    items(state, config)?.set_carnets_closed(&[carnet], false)?;
                }
                Carnet::Ls { closed } => {
                    for carnet in carnets.scan()? {
                        if closed || !carnet.closed {
                            println!(
                                "{}\t{}\t{}\t{}",
                                state::dir_name(&carnet.path),
                                group_text(carnet.links.group.as_ref()),
                                carnet.links.issue_keys.display(&config.tracker, ","),
                                carnet.summary
                            );
                        }
                    }
                }
                Carnet::Search { text } => carnets.search(&System, &text)?,
            }
            Ok(())
        }
        Command::Hooks(_)
        | Command::Shell(_)
        | Command::Hook { .. }
        | Command::Tui
        | Command::Context { .. }
        | Command::Statusline => {
            unreachable!()
        }
    }
}

/// The recorded carnet holding `dir`, found as `context` finds an item; anything else is
/// refused.
fn holding_carnet(state: &State, config: &Config, dir: &Path) -> Result<PathBuf> {
    let located = context::locate(state, config, dir)?;
    match located.filter(|located| located.is_carnet()) {
        Some(located) => Ok(located.path),
        None => bail!("{} is not in a carnet", dir.display()),
    }
}

/// Issue keys typed on the command line, resolved as front matter's are, blank ones dropped.
fn resolve(issues: &[String], config: &Config) -> IssueKeys {
    IssueKeys::resolve(issues.iter().map(String::as_str), &config.tracker)
}

fn run_hook(phase: Phase) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let payload: hooks::Payload =
        serde_json::from_str(&input).wrap_err("reading the worktrunk hook context")?;
    let config = Config::load()?;
    let state = State::open(&state::db_path(), config.default_workspace())?;
    let items = items(&state, &config)?;
    let tab = hooks::handle(&items, &System, phase, &payload, &hooks::Hints::from_env())?;
    if let (Some(tab), Some(target)) = (tab, std::env::var_os("ATELIER_HOOK_TARGET")) {
        std::fs::write(target, format!("{}\n", tab.session))?;
    }
    name_tabs(&state, &config);
    Ok(())
}

fn report_hook_error(err: &color_eyre::Report) {
    let log = crate::config::state_home().join("atelier/hooks.log");
    let logged = std::fs::create_dir_all(log.parent().unwrap())
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
        })
        .and_then(|mut file| writeln!(file, "{err:?}"));
    match logged {
        Ok(()) => eprintln!("atelier: hook error: {err} (see {})", log.display()),
        Err(_) => eprintln!("atelier: hook error: {err:?}"),
    }
}

fn run_hooks(command: Hooks) -> Result<()> {
    let mut config = HooksConfig::load(&worktrunk::config_path())?;
    match command {
        Hooks::Status => {
            for phase in Phase::ALL {
                let status = if config.installed(phase) {
                    "installed"
                } else {
                    "missing"
                };
                println!("{}\t{status}", phase.name());
            }
            return Ok(());
        }
        Hooks::Install => config.install()?,
        Hooks::Uninstall => config.uninstall(),
    }
    if config.save()? {
        println!("updated {}", config.path().display());
    }
    Ok(())
}

/// The registry for completions, without creating or migrating the database.
fn registry() -> Option<State> {
    let config = Config::load().unwrap_or_default();
    State::open_read_only(&state::db_path(), config.default_workspace()).ok()
}

fn complete_workspaces() -> Vec<CompletionCandidate> {
    let mut names = registry()
        .and_then(|state| state.workspaces().ok())
        .unwrap_or_default();
    if names.is_empty() {
        names.push(
            Config::load()
                .unwrap_or_default()
                .default_workspace()
                .to_owned(),
        );
    }
    names.into_iter().map(CompletionCandidate::new).collect()
}

fn complete_repos() -> Vec<CompletionCandidate> {
    let repos = registry()
        .and_then(|state| state.repos().ok())
        .unwrap_or_default();
    repos
        .iter()
        .map(|repo| {
            let unique = repos
                .iter()
                .filter(|other| other.name() == repo.name())
                .count()
                == 1;
            let path = repo.path.to_string_lossy().into_owned();
            let value = if unique { repo.name() } else { path.clone() };
            CompletionCandidate::new(value).help(Some(path.into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        <Cli as clap::CommandFactory>::command().debug_assert();
    }
}
