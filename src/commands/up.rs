use crate::config::{self, Config};
use crate::slugs;
use anyhow::{Context, Result, bail};
use colored::Colorize;

pub fn run(custom_slug: Option<&str>) -> Result<()> {
    let config = Config::load().context("run `devproxy init` first")?;

    let cwd = std::env::current_dir()?;
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_dir = compose_path
        .parent()
        .context("compose file has no parent directory")?;

    eprintln!(
        "found compose file: {}",
        compose_path.display().to_string().cyan()
    );

    let compose = config::parse_compose_file(&compose_path)?;
    let (service_name, container_port) = config::find_devproxy_service(&compose)?;
    eprintln!(
        "service: {}, container port: {}",
        service_name.cyan(),
        container_port.to_string().cyan()
    );

    // Check for existing project state (reuse if present)
    let project_path = compose_dir.join(".devproxy-project");
    let override_path = compose_dir.join(".devproxy-override.yml");
    let reusing = project_path.exists() && override_path.exists();

    let slug = if reusing {
        let existing_slug = config::read_project_file(compose_dir)?;
        if custom_slug.is_some() {
            eprintln!(
                "{} ignoring --slug, reusing existing slug. Run `devproxy down` first to change slug.",
                "warn:".yellow()
            );
        }
        eprintln!("slug: {} (reusing)", existing_slug.cyan());
        existing_slug
    } else {
        let app_name = config::detect_app_name(&cwd)?;
        eprintln!("app: {}", app_name.cyan());

        let slug_prefix = match custom_slug {
            Some(s) => {
                config::validate_custom_slug_with_app(s, &app_name)?;
                s.to_string()
            }
            None => slugs::generate_slug(),
        };
        let slug = config::compose_slug(&slug_prefix, &app_name);
        eprintln!("slug: {}", slug.cyan());

        let host_port = config::find_free_port()?;
        eprintln!("host port: {}", host_port.to_string().cyan());

        config::write_override_file(compose_dir, &service_name, host_port, container_port)?;
        eprintln!(
            "override: {}",
            override_path.display().to_string().cyan()
        );

        config::write_project_file(compose_dir, &slug)?;
        slug
    };

    // Verify daemon is running.
    // On the !reusing path, clean up freshly-written files on failure.
    // On the reusing path, files pre-existed so leave them alone.
    let socket_path = Config::socket_path()?;
    if !socket_path.exists() {
        if !reusing {
            let _ = std::fs::remove_file(&override_path);
            let _ = std::fs::remove_file(&project_path);
        }
        bail!(
            "daemon is not running (no socket at {}). Run `devproxy init` first.",
            socket_path.display()
        );
    }

    if !crate::ipc::ping_sync(&socket_path, std::time::Duration::from_secs(2)) {
        if !reusing {
            let _ = std::fs::remove_file(&override_path);
            let _ = std::fs::remove_file(&project_path);
        }
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
        // Only clean up files we just created (not reused ones)
        if !reusing {
            let _ = std::fs::remove_file(&override_path);
            let _ = std::fs::remove_file(&project_path);
        }
        bail!("docker compose up failed");
    }

    let url = format!("https://{slug}.{}", config.domain);
    eprintln!();
    eprintln!("{} {}", "->".green().bold(), url.green().bold());

    Ok(())
}
