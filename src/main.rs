mod cli;
mod commands;
mod config;
mod ipc;
mod proxy;
mod slugs;

use clap::Parser;
use cli::{Cli, Commands};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { domain, port, no_daemon } => commands::init::run(&domain, port, no_daemon),
        Commands::Up => commands::up::run(),
        Commands::Down => commands::down::run(),
        Commands::Ls => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::ls::run())
        }
        Commands::Open => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::open::run())
        }
        Commands::Status => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::status::run())
        }
        Commands::Daemon { port } => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::daemon::run(port))
        }
    }
}
