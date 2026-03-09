use crate::config;
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Read the slug from .devproxy-project so we target the right compose project
    let slug = config::read_project_file(&cwd)?;
    eprintln!("project: {}", slug.cyan());

    // Find the compose file name (shared utility from config module)
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_file_name = compose_path
        .file_name()
        .context("no filename")?
        .to_string_lossy()
        .to_string();

    // Run docker compose down with the correct project name
    let status = std::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose_file_name,
            "-f",
            ".devproxy-override.yml",
            "--project-name",
            &slug,
            "down",
        ])
        .current_dir(&cwd)
        .status()
        .context("failed to run docker compose down")?;

    if !status.success() {
        eprintln!("{} docker compose down exited with error", "warn:".yellow());
    }

    // Remove override and project files
    let override_path = cwd.join(".devproxy-override.yml");
    if override_path.exists() {
        std::fs::remove_file(&override_path)?;
        eprintln!("{} removed .devproxy-override.yml", "ok:".green());
    }

    let project_path = cwd.join(".devproxy-project");
    if project_path.exists() {
        std::fs::remove_file(&project_path)?;
        eprintln!("{} removed .devproxy-project", "ok:".green());
    }

    eprintln!("{} project stopped", "ok:".green());
    Ok(())
}
