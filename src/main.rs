mod carnet;
mod cli;
mod config;
mod context;
mod finish;
mod git;
mod hooks;
mod issues;
mod items;
mod process;
mod reviews;
mod shell;
mod state;
mod tool;
mod tui;
mod worktrunk;
mod zellij;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    cli::run()
}
