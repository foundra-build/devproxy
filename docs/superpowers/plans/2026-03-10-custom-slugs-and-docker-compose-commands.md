# Custom Slugs & Docker Compose Command Parity — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--slug` flag to `devproxy up` for predictable URLs, and introduce `stop`, `start`, `restart` (app stack), and `daemon restart` commands to mirror docker compose lifecycle.

**Architecture:** Existing file-based state (`.devproxy-project`, `.devproxy-override.yml`) becomes the source of truth for whether a project is "configured." `up` checks for these files before generating new slugs/ports. New `stop`/`start` commands mirror docker compose stop/start without touching these files. `daemon` becomes a clap subcommand group with `run` (hidden) and `restart`.

**Tech Stack:** Rust, clap (derive), anyhow, colored, Docker Compose CLI

**Spec:** `docs/superpowers/specs/2026-03-10-custom-slugs-and-docker-compose-commands-design.md`

---

## Chunk 1: Validation + CLI Structure

### Task 1: Add `validate_custom_slug()` to config.rs

**Files:**
- Modify: `src/config.rs:317-328` (after `compose_slug`)

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/config.rs`:

```rust
#[test]
fn validate_custom_slug_accepts_valid() {
    assert!(validate_custom_slug("dirty-panda").is_ok());
    assert!(validate_custom_slug("my-app").is_ok());
    assert!(validate_custom_slug("a").is_ok());
    assert!(validate_custom_slug("abc123").is_ok());
}

#[test]
fn validate_custom_slug_rejects_empty() {
    assert!(validate_custom_slug("").is_err());
}

#[test]
fn validate_custom_slug_rejects_uppercase() {
    assert!(validate_custom_slug("Dirty-Panda").is_err());
}

#[test]
fn validate_custom_slug_rejects_special_chars() {
    assert!(validate_custom_slug("dirty_panda").is_err());
    assert!(validate_custom_slug("dirty.panda").is_err());
    assert!(validate_custom_slug("dirty panda").is_err());
}

#[test]
fn validate_custom_slug_rejects_leading_trailing_hyphens() {
    assert!(validate_custom_slug("-dirty").is_err());
    assert!(validate_custom_slug("dirty-").is_err());
    assert!(validate_custom_slug("-dirty-").is_err());
}

#[test]
fn validate_custom_slug_rejects_too_long_composite() {
    // compose_slug joins as "{slug}-{app_name}" and must be <= 63
    // Use a slug that when combined with a reasonable app name exceeds 63
    let long_slug = "a".repeat(60);
    assert!(validate_custom_slug_with_app(&long_slug, "my-app").is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config::tests::validate_custom_slug 2>&1 | head -30`
Expected: compilation errors — functions don't exist yet

- [ ] **Step 3: Write minimal implementation**

Add to `src/config.rs` after the `compose_slug` function (after line 328):

```rust
/// Validate a user-provided custom slug prefix.
/// Unlike `sanitize_subdomain` which transforms input, this rejects invalid input.
/// Rules: lowercase alphanumeric + hyphens, no leading/trailing hyphens, non-empty.
pub fn validate_custom_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("slug cannot be empty");
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        bail!("slug cannot start or end with a hyphen: '{slug}'");
    }
    if !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("slug must contain only lowercase letters, digits, and hyphens: '{slug}'");
    }
    Ok(())
}

/// Validate a custom slug and check the composite length with app name.
pub fn validate_custom_slug_with_app(slug: &str, app_name: &str) -> Result<()> {
    validate_custom_slug(slug)?;
    let composite = compose_slug(slug, app_name);
    if composite.len() > 63 {
        bail!(
            "slug '{slug}' combined with app name '{app_name}' is {} chars (max 63)",
            format!("{slug}-{app_name}").len()
        );
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::tests::validate_custom_slug`
Expected: all 7 tests pass

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add validate_custom_slug() for custom slug validation"
```

---

### Task 2: Restructure CLI — add `--slug`, `Stop`, `Start`, `Daemon` subcommand group

**Files:**
- Modify: `src/cli.rs`

- [ ] **Step 1: Write the failing tests**

Replace the existing test and add new ones in `src/cli.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib cli::tests 2>&1 | head -20`
Expected: compilation errors

- [ ] **Step 3: Write the implementation**

Replace the entire `src/cli.rs` content:

```rust
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
```

- [ ] **Step 4: Update main.rs dispatch**

Replace the match block in `src/main.rs`:

```rust
match cli.command {
    Commands::Init {
        domain,
        port,
        no_daemon,
    } => commands::init::run(&domain, port, no_daemon),
    Commands::Up { slug } => commands::up::run(slug.as_deref()),
    Commands::Down => commands::down::run(),
    Commands::Stop => commands::stop::run(),
    Commands::Start => commands::start::run(),
    Commands::Restart => commands::restart::run(),
    Commands::GetUrl => commands::get_url::run(),
    Commands::Ls => commands::ls::run().await,
    Commands::Open => commands::open::run().await,
    Commands::Status => commands::status::run().await,
    Commands::Update => commands::update::run().await,
    Commands::Daemon { subcommand } => match subcommand {
        cli::DaemonCommand::Run { port } => commands::daemon::run(port).await,
        cli::DaemonCommand::Restart => commands::daemon::restart(),
    },
}
```

- [ ] **Step 5: Update commands/mod.rs**

Add the new modules:

```rust
pub mod daemon;
pub mod down;
pub mod get_url;
pub mod init;
pub mod ls;
pub mod open;
pub mod restart;
pub mod start;
pub mod status;
pub mod stop;
pub mod up;
pub mod update;
```

- [ ] **Step 6: Create stub command files so it compiles**

Create `src/commands/stop.rs`:
```rust
use anyhow::{Result, bail};

pub fn run() -> Result<()> {
    bail!("not yet implemented")
}
```

Create `src/commands/start.rs`:
```rust
use anyhow::{Result, bail};

pub fn run() -> Result<()> {
    bail!("not yet implemented")
}
```

- [ ] **Step 7: Update up.rs signature**

Change the signature in `src/commands/up.rs` from `pub fn run() -> Result<()>` to `pub fn run(_slug: Option<&str>) -> Result<()>`. The `_slug` parameter is unused for now.

- [ ] **Step 8: Add daemon restart function**

Add to `src/commands/daemon.rs`:

```rust
pub fn restart() -> anyhow::Result<()> {
    use colored::Colorize;
    match crate::platform::restart_daemon() {
        Ok(true) => {
            eprintln!("{} daemon restarted", "ok:".green());
            Ok(())
        }
        Ok(false) => {
            eprintln!(
                "{} no platform-managed daemon found. Run {} to set one up",
                "error:".red(),
                "devproxy init".bold()
            );
            std::process::exit(1);
        }
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test --lib cli::tests`
Expected: all 8 tests pass

Run: `cargo test --lib config::tests`
Expected: all existing + new tests pass

- [ ] **Step 10: Commit**

```bash
git add src/cli.rs src/main.rs src/commands/mod.rs src/commands/stop.rs src/commands/start.rs src/commands/up.rs src/commands/daemon.rs
git commit -m "feat: restructure CLI for stop/start/restart and daemon subcommands

Add --slug flag to up, stop/start commands (stubs), daemon run/restart
subcommands. Moves daemon restart from top-level restart to daemon restart."
```

---

## Chunk 2: Command Implementations

### Task 3: Implement `devproxy up` with slug reuse and `--slug` flag

**Files:**
- Modify: `src/commands/up.rs`

- [ ] **Step 1: Rewrite up.rs with slug resolution logic**

Replace the entire `src/commands/up.rs`:

```rust
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

    // Verify daemon is running
    let socket_path = Config::socket_path()?;
    if !socket_path.exists() {
        bail!(
            "daemon is not running (no socket at {}). Run `devproxy init` first.",
            socket_path.display()
        );
    }

    if !crate::ipc::ping_sync(&socket_path, std::time::Duration::from_secs(2)) {
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
```

**Note on cleanup logic change:** The `reusing` boolean is captured before any file writes. On `docker compose up` failure, we only clean up files we freshly created (`!reusing`), not files that existed beforehand. For daemon-not-running errors, we bail without cleanup since the files may be intentionally there from a previous `stop`.

- [ ] **Step 2: Run clippy and tests**

Run: `cargo clippy --all-targets 2>&1 | tail -20`
Run: `cargo test --lib 2>&1 | tail -20`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add src/commands/up.rs
git commit -m "feat: up command reuses existing slug/override and supports --slug"
```

---

### Task 4: Implement `devproxy stop`

**Files:**
- Modify: `src/commands/stop.rs`

- [ ] **Step 1: Implement stop.rs**

Replace `src/commands/stop.rs`:

```rust
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
```

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -10`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add src/commands/stop.rs
git commit -m "feat: add devproxy stop command (preserves slug and override)"
```

---

### Task 5: Implement `devproxy start`

**Files:**
- Modify: `src/commands/start.rs`

- [ ] **Step 1: Implement start.rs**

Replace `src/commands/start.rs`:

```rust
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
```

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -10`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add src/commands/start.rs
git commit -m "feat: add devproxy start command (resumes stopped containers)"
```

---

### Task 6: Rewrite `devproxy restart` for app stack

**Files:**
- Modify: `src/commands/restart.rs`

- [ ] **Step 1: Rewrite restart.rs**

Replace `src/commands/restart.rs`:

```rust
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

    // Verify daemon is running (same checks as start, per spec)
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
            "restart",
        ])
        .current_dir(compose_dir)
        .status()
        .context("failed to run docker compose restart")?;

    if !status.success() {
        bail!("docker compose restart failed");
    }

    let config = Config::load().context("run `devproxy init` first")?;
    let url = format!("https://{slug}.{}", config.domain);
    eprintln!();
    eprintln!("{} {}", "->".green().bold(), url.green().bold());

    Ok(())
}
```

- [ ] **Step 2: Run clippy and all lib tests**

Run: `cargo clippy --all-targets 2>&1 | tail -10`
Run: `cargo test --lib 2>&1 | tail -20`
Expected: all pass

- [ ] **Step 3: Commit**

```bash
git add src/commands/restart.rs
git commit -m "feat: restart now restarts app stack instead of daemon

Daemon restart moved to devproxy daemon restart."
```

---

## Chunk 3: Platform Updates + Launchd/Systemd Compatibility

### Task 7: Update platform plist/unit generation for `daemon run` subcommand

**Files:**
- Modify: `src/platform.rs`

The plist currently generates `<string>daemon</string><string>--port</string>`. It needs to become `<string>daemon</string><string>run</string><string>--port</string>`. Same for systemd ExecStart.

- [ ] **Step 1: Update the existing platform tests first**

In `src/platform.rs`, find the test assertions that check for `"daemon --port"` and update them:

In `test_systemd_service_unit_contains_binary_and_port`:
```rust
// Change: assert!(unit.contains("daemon --port 443"), ...);
// To:
assert!(unit.contains("daemon run --port 443"), "should run daemon run subcommand with port");
```

In `test_systemd_service_unit_custom_port`:
```rust
// Change: assert!(unit.contains("daemon --port 8443"), ...);
// To:
assert!(unit.contains("daemon run --port 8443"), "should use custom port in ExecStart");
```

The launchd plist tests check for individual `<string>` elements, so they need a new assertion for the `run` argument. Add to `test_launchagent_plist_contains_required_fields`:
```rust
assert!(plist.contains("<string>run</string>"), "should have run subcommand");
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib platform::tests 2>&1 | tail -20`
Expected: assertion failures on the updated strings

- [ ] **Step 3: Update plist generation**

In `src/platform.rs` `generate_launchagent_plist()`, update the ProgramArguments array (around line 108-113):

Change:
```xml
    <key>ProgramArguments</key>
    <array>
        <string>{binary_path}</string>
        <string>daemon</string>
        <string>--port</string>
        <string>{port}</string>
    </array>
```
To:
```xml
    <key>ProgramArguments</key>
    <array>
        <string>{binary_path}</string>
        <string>daemon</string>
        <string>run</string>
        <string>--port</string>
        <string>{port}</string>
    </array>
```

- [ ] **Step 4: Update systemd service generation**

In `generate_systemd_service_unit()`, change the ExecStart line (around line 180):

Change: `ExecStart="{binary_path}" daemon --port {port}`
To: `ExecStart="{binary_path}" daemon run --port {port}`

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib platform::tests`
Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add src/platform.rs
git commit -m "feat: update plist/unit templates for daemon run subcommand"
```

---

## Chunk 4: Documentation + Plugin Sync

### Task 8: Update README.md

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the example and commands table**

Update the example at the top to show `--slug`:

```bash
devproxy up
# → https://swift-penguin-myapp.mysite.dev

devproxy up --slug my-app
# → https://my-app-myapp.mysite.dev
```

Update the commands table:

```markdown
| Command              | Description                                       |
|----------------------|---------------------------------------------------|
| `devproxy init`      | One-time setup: certs, CA trust, daemon            |
| `devproxy up`        | Start project, assign slug, proxy it               |
| `devproxy up --slug` | Start project with a custom slug prefix            |
| `devproxy down`      | Stop project, remove override and slug             |
| `devproxy stop`      | Stop containers (preserves slug for restart)       |
| `devproxy start`     | Start previously stopped containers                |
| `devproxy restart`   | Restart app containers                             |
| `devproxy ls`        | List running projects with URLs                    |
| `devproxy open`      | Open project URL in browser                        |
| `devproxy status`    | Daemon health check                                |
| `devproxy daemon restart` | Restart the background daemon               |
| `devproxy update`    | Check for updates and self-update                  |
| `devproxy --version` | Show installed version                             |
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: update README with stop/start/restart and --slug flag"
```

---

### Task 9: Update skills/devproxy/SKILL.md

**Files:**
- Modify: `skills/devproxy/SKILL.md`

- [ ] **Step 1: Update the command table and descriptions**

Update the commands table to add new commands and update `restart`:

```markdown
| Command                          | What it does                                    |
|----------------------------------|-------------------------------------------------|
| `devproxy init --domain X`       | One-time: certs, CA trust, start daemon         |
| `devproxy init --port 8443`      | Use non-privileged port (avoids sudo on Linux)  |
| `devproxy up`                    | Assign slug, bind port, `docker compose up -d`  |
| `devproxy up --slug NAME`        | Use custom slug prefix for predictable URLs     |
| `devproxy down`                  | `docker compose down` + remove override & slug  |
| `devproxy stop`                  | `docker compose stop` (preserves slug/override) |
| `devproxy start`                 | `docker compose start` (reuses existing slug)   |
| `devproxy restart`               | Restart app containers (stop + start)           |
| `devproxy ls`                    | List running projects with slugs and URLs       |
| `devproxy get-url`               | Print this project's proxy URL (for scripting)  |
| `devproxy open`                  | Open this project's URL in browser              |
| `devproxy daemon restart`        | Restart the background daemon process           |
| `devproxy update`                | Check for updates and self-update the binary    |
| `devproxy --version`             | Show installed version                          |
| `devproxy status`                | Daemon health + active route count              |
```

Update the "Daemon Lifecycle" section to change `devproxy restart` reference to `devproxy daemon restart`.

Update the "Common Issues" table — change the "Slug changed after restart" row:
```markdown
| Slug changed after restart | Use `devproxy stop`/`start` to preserve slug, or `devproxy up --slug NAME` for a predictable slug |
```

Update the trigger description in the frontmatter to include the new commands:
```
"devproxy stop", "devproxy start", "devproxy daemon restart"
```

- [ ] **Step 2: Commit**

```bash
git add skills/devproxy/SKILL.md
git commit -m "docs: update devproxy skill with new commands and --slug"
```

---

### Task 10: Bump version in Cargo.toml and plugin.json

**Files:**
- Modify: `Cargo.toml`
- Modify: `.claude-plugin/plugin.json`

- [ ] **Step 1: Bump version to 0.5.0**

In `Cargo.toml`, change `version = "0.4.4"` to `version = "0.5.0"`.

In `.claude-plugin/plugin.json`, change `"version": "0.4.4"` to `"version": "0.5.0"`.

- [ ] **Step 2: Commit**

```bash
git add Cargo.toml .claude-plugin/plugin.json
git commit -m "chore: bump version to 0.5.0 for breaking restart change"
```

---

### Task 11: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo clippy --all-targets 2>&1`
Run: `cargo test --lib 2>&1`
Expected: all pass, no warnings

- [ ] **Step 2: Build release binary**

Run: `cargo build --release 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 3: Verify help output**

Run: `cargo run -- --help 2>&1`
Run: `cargo run -- up --help 2>&1`
Run: `cargo run -- daemon --help 2>&1`
Run: `cargo run -- daemon restart --help 2>&1`
Expected: all show correct descriptions and options
