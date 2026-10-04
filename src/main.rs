mod cli;
mod config;
mod hooks;
mod process;
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
