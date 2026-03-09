mod cli;
mod commands;
mod config;
mod ipc;
mod platform;
mod proxy;
mod slugs;

use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init {
            domain,
            port,
            no_daemon,
        } => commands::init::run(&domain, port, no_daemon),
        Commands::Up => commands::up::run(),
        Commands::Down => commands::down::run(),
        Commands::Ls => commands::ls::run().await,
        Commands::Open => commands::open::run().await,
        Commands::Status => commands::status::run().await,
        Commands::Update => commands::update::run().await,
        Commands::Daemon { port } => commands::daemon::run(port).await,
    }
}
