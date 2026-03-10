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
    Up {
        /// Custom slug prefix (e.g., --slug dirty-panda for dirty-panda-myapp.mysite.dev)
        #[arg(long)]
        slug: Option<String>,
    },
    /// Stop this project and remove override file
    Down,
    /// Stop containers without removing override (preserves slug)
    Stop,
    /// Start previously stopped containers (reuses existing slug)
    Start,
    /// Restart app containers (stop + start)
    Restart,
    /// List all running projects with slugs and URLs
    Ls,
    /// Print this project's proxy URL (empty + exit 1 if not running)
    GetUrl,
    /// Open this project's URL in the browser
    Open,
    /// Show daemon health and active route count
    Status,
    /// Check for updates and self-update the binary
    Update,
    /// Daemon management (run, restart)
    Daemon {
        #[command(subcommand)]
        subcommand: DaemonCommand,
    },
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Run the proxy daemon (internal, used by launchd/systemd)
    #[command(hide = true)]
    Run {
        /// Port to listen on (default: 443)
        #[arg(long, default_value = "443")]
        port: u16,
    },
    /// Restart the background daemon process
    Restart,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_up_no_slug() {
        let cli = Cli::try_parse_from(["devproxy", "up"]).expect("should parse up");
        match cli.command {
            Commands::Up { slug } => assert!(slug.is_none()),
            _ => panic!("expected Up"),
        }
    }

    #[test]
    fn test_parse_up_with_slug() {
        let cli = Cli::try_parse_from(["devproxy", "up", "--slug", "dirty-panda"])
            .expect("should parse up --slug");
        match cli.command {
            Commands::Up { slug } => assert_eq!(slug.as_deref(), Some("dirty-panda")),
            _ => panic!("expected Up"),
        }
    }

    #[test]
    fn test_parse_stop() {
        let cli = Cli::try_parse_from(["devproxy", "stop"]).expect("should parse stop");
        assert!(matches!(cli.command, Commands::Stop));
    }

    #[test]
    fn test_parse_start() {
        let cli = Cli::try_parse_from(["devproxy", "start"]).expect("should parse start");
        assert!(matches!(cli.command, Commands::Start));
    }

    #[test]
    fn test_parse_restart() {
        let cli = Cli::try_parse_from(["devproxy", "restart"]).expect("should parse restart");
        assert!(matches!(cli.command, Commands::Restart));
    }

    #[test]
    fn test_parse_daemon_run() {
        let cli = Cli::try_parse_from(["devproxy", "daemon", "run"])
            .expect("should parse daemon run");
        match cli.command {
            Commands::Daemon { subcommand: DaemonCommand::Run { port } } => {
                assert_eq!(port, 443);
            }
            _ => panic!("expected Daemon Run"),
        }
    }

    #[test]
    fn test_parse_daemon_run_with_port() {
        let cli = Cli::try_parse_from(["devproxy", "daemon", "run", "--port", "8443"])
            .expect("should parse daemon run --port");
        match cli.command {
            Commands::Daemon { subcommand: DaemonCommand::Run { port } } => {
                assert_eq!(port, 8443);
            }
            _ => panic!("expected Daemon Run"),
        }
    }

    #[test]
    fn test_parse_daemon_restart() {
        let cli = Cli::try_parse_from(["devproxy", "daemon", "restart"])
            .expect("should parse daemon restart");
        match cli.command {
            Commands::Daemon { subcommand: DaemonCommand::Restart } => {}
            _ => panic!("expected Daemon Restart"),
        }
    }
}
