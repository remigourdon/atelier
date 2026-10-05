mod carnet;
mod cli;
mod config;
mod git;
mod hooks;
mod issues;
mod items;
mod process;
mod reviews;
mod shell;
mod state;
mod tui;
mod worktrunk;
mod zellij;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    cli::run()
}
