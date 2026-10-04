mod cli;
mod config;
mod hooks;
mod issues;
mod process;
mod reviews;
mod shell;
mod state;
mod sync;
mod tui;
mod worktrunk;
mod zellij;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    cli::run()
}
