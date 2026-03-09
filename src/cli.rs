use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "devproxy",
    about = "Local HTTPS dev subdomains for Docker Compose",
    version = env!("CARGO_PKG_VERSION")
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// One-time setup: generate certs, trust CA, start daemon
    Init {
        /// Domain for dev subdomains (e.g., mysite.dev)
        #[arg(long, default_value = "mysite.dev")]
        domain: String,
        /// Port for the daemon to listen on (default: 443)
        #[arg(long, default_value = "443")]
        port: u16,
        /// Skip starting the daemon (useful for CI or testing)
        #[arg(long)]
        no_daemon: bool,
    },
    /// Start this project and assign a dev subdomain
    Up,
    /// Stop this project and remove override file
    Down,
    /// List all running projects with slugs and URLs
    Ls,
    /// Open this project's URL in the browser
    Open,
    /// Show daemon health and active route count
    Status,
    /// Check for updates and self-update the binary
    Update,
    /// Run the proxy daemon (internal, hidden)
    #[command(hide = true)]
    Daemon {
        /// Port to listen on (default: 443)
        #[arg(long, default_value = "443")]
        port: u16,
    },
}
