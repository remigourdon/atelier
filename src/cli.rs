//! The command line.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use color_eyre::eyre::{Result, WrapErr, bail};

use crate::config::Config;
use crate::hooks::{self, Phase};
use crate::process::{Runner, System};
use crate::state::{self, State};
use crate::worktrunk::{self, HooksConfig};
use crate::zellij::{self, Layouts, Zellij};

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
    /// Remove an unused workspace.
    Rm {
        #[arg(add = ArgValueCandidates::new(complete_workspaces))]
        name: String,
        /// Forget the carnets it owns; their folders stay on disk.
        #[arg(long)]
        forget_carnets: bool,
    },
    /// List workspaces.
    Ls,
}

#[derive(Subcommand)]
enum Carnet {
    /// Create a carnet `<root>/YYYY-MM-DD-[KEY-]<name>`: a git repo with a README.
    New {
        name: String,
        /// Its workspace (default: the current session's, else the default workspace).
        #[arg(short, long, add = ArgValueCandidates::new(complete_workspaces))]
        workspace: Option<String>,
    },
    /// Record a dated git repo directly under the root as a carnet.
    Add {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Its workspace (default: the current session's, else the default workspace).
        #[arg(short, long, add = ArgValueCandidates::new(complete_workspaces))]
        workspace: Option<String>,
    },
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
        Command::Hook { phase } => {
            // A hook must never abort worktrunk: report and succeed.
            if let Err(err) = run_hook(phase) {
                report_hook_error(&err);
            }
            Ok(())
        }
        command => {
            let config = Config::load()?;
            let mut state = State::open(&state::db_path(), config.default_workspace())?;
            run_state(command, &config, &mut state)
        }
    }
}

fn zellij_for<'a>(config: &Config, runner: &'a dyn Runner) -> Result<Zellij<'a>> {
    Ok(Zellij {
        runner,
        here: zellij::current_session(),
        layouts: Layouts::resolve(config)?,
        anchor: config.anchor_pane().to_owned(),
    })
}

fn run_state(command: Command, config: &Config, state: &mut State) -> Result<()> {
    match command {
        Command::Ws(Ws::Add { name }) => state.add_workspace(&name),
        Command::Ws(Ws::Rm {
            name,
            forget_carnets,
        }) => {
            let forget = match forget_carnets {
                true => state.workspace_carnets(&name)?,
                false => Vec::new(),
            };
            state.remove_workspace(&name, &forget)
        }
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
            let root = main_worktree(&System, &path)?;
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
            state.update_repo(&repo, alias.as_deref(), workspace.as_deref())
        }
        Command::Rm { repo } => {
            let path = state.repo(&repo)?.path;
            zellij_for(config, &System)?.close_repo_tabs(state, &path)?;
            state.remove_repo(&path)
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
            zellij_for(config, &System)?.open_session(&workspace)
        }
        Command::Carnet(command) => {
            let root = config.require_carnet_root()?;
            let names = crate::carnet::Names::new(config.ticket_pattern())?;
            match command {
                Carnet::New { name, workspace } => {
                    let workspace = item_workspace(state, workspace)?;
                    let path = crate::carnet::create(
                        state, &System, &names, &root, &name, &workspace, "",
                    )?;
                    println!("created {} in {workspace}", path.display());
                }
                Carnet::Add { path, workspace } => {
                    let workspace = item_workspace(state, workspace)?;
                    let path = crate::carnet::add(state, &names, &root, &path, &workspace)?;
                    println!("recorded {} in {workspace}", path.display());
                }
            }
            Ok(())
        }
        Command::Hooks(_) | Command::Shell(_) | Command::Hook { .. } | Command::Tui => {
            unreachable!()
        }
    }
}

/// The workspace a new item goes to: the one given, else the current session's when it is a
/// workspace, else the default one.
fn item_workspace(state: &State, workspace: Option<String>) -> Result<String> {
    if let Some(workspace) = workspace {
        state.require_workspace(&workspace)?;
        return Ok(workspace);
    }
    let here = zellij::current_session().filter(|name| state.has_workspace(name).unwrap_or(false));
    Ok(here.unwrap_or_else(|| state.default_workspace().to_owned()))
}

/// The main worktree of the repository containing `path`.
fn main_worktree(runner: &dyn Runner, path: &Path) -> Result<PathBuf> {
    let path = path.to_string_lossy();
    let listing = runner
        .output("git", &["-C", &path, "worktree", "list", "--porcelain"])
        .wrap_err_with(|| format!("{path} is not in a git repository"))?;
    let Some(main) = listing
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("worktree "))
    else {
        bail!("git worktree list printed nothing for {path}");
    };
    Ok(std::fs::canonicalize(main)?)
}

fn run_hook(phase: Phase) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let payload: hooks::Payload =
        serde_json::from_str(&input).wrap_err("reading the worktrunk hook context")?;
    let config = Config::load()?;
    let state = State::open(&state::db_path(), config.default_workspace())?;
    let zellij = zellij_for(&config, &System)?;
    let tab = hooks::handle(
        &state,
        &zellij,
        &config.ticket_regex()?,
        phase,
        &payload,
        &hooks::Hints::from_env(),
    )?;
    if let (Some(tab), Some(target)) = (tab, std::env::var_os("ATELIER_HOOK_TARGET")) {
        std::fs::write(target, format!("{}\n", tab.session))?;
    }
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
    use crate::process::fake::Fake;

    #[test]
    fn main_worktree_is_the_first_listed() {
        let dir = tempfile::tempdir().unwrap();
        let listing = format!(
            "worktree {}\nHEAD abc\n\nworktree /elsewhere\n",
            dir.path().display()
        );
        let fake = Fake::default().always("git -C .", Some(&listing));
        let main = main_worktree(&fake, Path::new(".")).unwrap();
        assert_eq!(main, dir.path().canonicalize().unwrap());
        assert!(main_worktree(&Fake::default().always("git", None), Path::new(".")).is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        <Cli as clap::CommandFactory>::command().debug_assert();
    }
}
