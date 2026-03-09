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
    let override_path = config::write_override_file(compose_dir, &service_name, host_port, container_port)?;
    eprintln!(
        "override: {}",
        override_path.display().to_string().cyan()
    );

    // Write project file (slug tracking -- used by `down` and `open`)
    config::write_project_file(compose_dir, &slug)?;

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
