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
    /// Print this project's proxy URL (empty + exit 1 if not running)
    GetUrl,
    /// Open this project's URL in the browser
    Open,
    /// Show daemon health and active route count
    Status,
    /// Restart the daemon
    Restart,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_restart_command() {
        let cli = Cli::try_parse_from(["devproxy", "restart"]).expect("should parse restart");
        assert!(
            matches!(cli.command, Commands::Restart),
            "should parse as Restart variant"
        );
    }
}
