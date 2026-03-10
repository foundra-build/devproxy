use crate::config::{self, Config};
use anyhow::{Context, Result, bail};
use colored::Colorize;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_dir = compose_path
        .parent()
        .context("compose file has no parent directory")?;

    let slug = config::read_project_file(compose_dir)?;
    eprintln!("project: {}", slug.cyan());

    let override_path = compose_dir.join(".devproxy-override.yml");
    if !override_path.exists() {
        bail!("override file missing. Run `devproxy up` to reconfigure.");
    }

    // Verify daemon is running
    let socket_path = Config::socket_path()?;
    if !socket_path.exists()
        || !crate::ipc::ping_sync(&socket_path, std::time::Duration::from_secs(2))
    {
        bail!("daemon is not running. Run `devproxy init` first.");
    }

    let compose_file_name = compose_path
        .file_name()
        .context("no filename")?
        .to_string_lossy()
        .to_string();

    let status = std::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose_file_name,
            "-f",
            ".devproxy-override.yml",
            "--project-name",
            &slug,
            "start",
        ])
        .current_dir(compose_dir)
        .status()
        .context("failed to run docker compose start")?;

    if !status.success() {
        bail!("docker compose start failed");
    }

    let config = Config::load().context("run `devproxy init` first")?;
    let url = format!("https://{slug}.{}", config.domain);
    eprintln!();
    eprintln!("{} {}", "->".green().bold(), url.green().bold());

    Ok(())
}
