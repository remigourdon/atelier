use clap::Parser;

/// A lazygit-style TUI and CLI that organise git worktrees into zellij sessions.
#[derive(Parser)]
#[command(version)]
struct Cli {}

fn main() {
    Cli::parse();
}
