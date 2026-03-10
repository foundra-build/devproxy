use crate::config;
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_dir = compose_path
        .parent()
        .context("compose file has no parent directory")?;

    let slug = config::read_project_file(compose_dir)?;
    eprintln!("project: {}", slug.cyan());

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
            "stop",
        ])
        .current_dir(compose_dir)
        .status()
        .context("failed to run docker compose stop")?;

    if !status.success() {
        eprintln!("{} docker compose stop exited with error", "warn:".yellow());
    }

    eprintln!("{} project stopped (slug and override preserved)", "ok:".green());
    Ok(())
}
