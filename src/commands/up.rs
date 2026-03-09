use crate::config::{self, Config};
use crate::slugs;
use anyhow::{Context, Result, bail};
use colored::Colorize;

pub fn run() -> Result<()> {
    // Check config exists (implies init was run)
    let config = Config::load().context("run `devproxy init` first")?;

    // Find docker-compose.yml (shared utility from config module)
    let cwd = std::env::current_dir()?;
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_dir = compose_path
        .parent()
        .context("compose file has no parent directory")?;

    eprintln!(
        "found compose file: {}",
        compose_path.display().to_string().cyan()
    );

    // Parse compose file
    let compose = config::parse_compose_file(&compose_path)?;
    let (service_name, container_port) = config::find_devproxy_service(&compose)?;
    eprintln!(
        "service: {}, container port: {}",
        service_name.cyan(),
        container_port.to_string().cyan()
    );

    // Generate slug
    let slug = slugs::generate_slug();
    eprintln!("slug: {}", slug.cyan());

    // Find free port
    let host_port = config::find_free_port()?;
    eprintln!("host port: {}", host_port.to_string().cyan());

    // Write override file (port binding)
    let override_path =
        config::write_override_file(compose_dir, &service_name, host_port, container_port)?;
    eprintln!("override: {}", override_path.display().to_string().cyan());

    // Write project file (slug tracking -- used by `down` and `open`)
    config::write_project_file(compose_dir, &slug)?;

    // Verify daemon is running before starting containers.
    // Use a short timeout (2s) so we fail fast instead of hanging forever.
    let socket_path = Config::socket_path()?;
    if !socket_path.exists() {
        // Clean up files we already wrote
        let _ = std::fs::remove_file(&override_path);
        let _ = std::fs::remove_file(compose_dir.join(".devproxy-project"));
        bail!(
            "daemon is not running (no socket at {}). Run `devproxy init` first.",
            socket_path.display()
        );
    }

    // Send an actual IPC ping with a 2s timeout to verify the daemon is
    // responsive, not just that a stale socket file exists.
    if !crate::ipc::ping_sync(&socket_path, std::time::Duration::from_secs(2)) {
        let _ = std::fs::remove_file(&override_path);
        let _ = std::fs::remove_file(compose_dir.join(".devproxy-project"));
        bail!(
            "daemon is not running (no response from {}). Run `devproxy init` first.",
            socket_path.display()
        );
    }

    // Run docker compose up
    let compose_file_name = compose_path
        .file_name()
        .context("no filename")?
        .to_string_lossy();

    let status = std::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose_file_name,
            "-f",
            ".devproxy-override.yml",
            "--project-name",
            &slug,
            "up",
            "-d",
        ])
        .current_dir(compose_dir)
        .status()
        .context("failed to run docker compose")?;

    if !status.success() {
        // Clean up on failure
        let _ = std::fs::remove_file(&override_path);
        let _ = std::fs::remove_file(compose_dir.join(".devproxy-project"));
        bail!("docker compose up failed");
    }

    let url = format!("https://{slug}.{}", config.domain);
    eprintln!();
    eprintln!("{} {}", "->".green().bold(), url.green().bold());

    Ok(())
}
