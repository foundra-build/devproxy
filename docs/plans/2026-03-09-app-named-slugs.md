# App-Named Slugs — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Make devproxy URLs include the app/repo name so users can identify which project a URL belongs to. Change the URL format from `https://{slug}.{domain}` to `https://{slug}.{app-name}.{domain}` where `{app-name}` is derived from the git remote origin (GitHub repo name), falling back to the directory name. Also enhance `devproxy ls` to mark the current directory's project with `*`.

**Architecture:**

The URL format changes from `https://swift-penguin.mysite.dev` to `https://swift-penguin.devproxy.mysite.dev` where `devproxy` is the app name (derived from the repo name of the cwd where `devproxy up` was run). Multiple instances of the same repo (e.g., worktrees) get different random slugs but share the same app-name subdomain level: `https://bold-fox.devproxy.mysite.dev`, `https://calm-otter.devproxy.mysite.dev`.

**Key mechanism:** The `devproxy up` command detects the app name and passes it to Docker Compose as a label (`devproxy.app`) in the override file. The daemon's docker.rs reads this label when inspecting containers, and constructs the hostname as `{slug}.{app-name}.{domain}` instead of `{slug}.{domain}`. The `com.docker.compose.project` label remains the Docker Compose project name (used as the random slug for `--project-name`).

**App name detection:** A new function `detect_app_name(dir: &Path) -> Result<String>` in `config.rs`:
1. Run `git -C {dir} remote get-url origin` and parse the repo name from the URL (handles both HTTPS and SSH formats). Strip `.git` suffix if present.
2. If git fails or there's no remote, fall back to the directory name.
3. Sanitize the result: lowercase, replace non-alphanumeric chars with hyphens, collapse consecutive hyphens, trim leading/trailing hyphens.

**DNS note:** The wildcard DNS setup (dnsmasq) already resolves `*.mysite.dev` so `slug.app-name.mysite.dev` works without any DNS changes. The TLS wildcard cert is `*.mysite.dev` which does NOT match `a.b.mysite.dev` (wildcards only match one level). The cert generation in `cert.rs` must be updated to also include `*.*.mysite.dev` as a SAN to cover the two-level subdomain.

**`.devproxy-project` format change:** Currently stores just the slug (`swift-penguin`). Change to store `slug\napp-name` (two lines). The `read_project_file` function returns both values. For backward compatibility during transition, if only one line exists, treat the slug as the full compose project name with no app name (legacy format).

**`devproxy ls` current-directory indicator:** The `ls` command reads `.devproxy-project` from the cwd (if it exists, failing silently if not) to get the current project's slug. When printing routes, any route matching the cwd's slug gets a `*` prefix.

**Tech Stack:** No new dependencies. Uses `std::process::Command` to run `git`.

---

### Task 1: Add `detect_app_name()` to `config.rs`

**Files:**
- Modify: `src/config.rs`

**Step 1: Write the failing tests**

Add unit tests for app name detection:

```rust
#[cfg(test)]
mod tests {
    // ... existing tests ...

    #[test]
    fn detect_app_name_from_git_remote_https() {
        let dir = tempfile::tempdir().unwrap();
        // Initialize a git repo with an HTTPS remote
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "https://github.com/user/my-cool-app.git"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let name = detect_app_name(dir.path()).unwrap();
        assert_eq!(name, "my-cool-app");
    }

    #[test]
    fn detect_app_name_from_git_remote_ssh() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "git@github.com:user/another-app.git"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let name = detect_app_name(dir.path()).unwrap();
        assert_eq!(name, "another-app");
    }

    #[test]
    fn detect_app_name_falls_back_to_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-project");
        std::fs::create_dir_all(&sub).unwrap();
        let name = detect_app_name(&sub).unwrap();
        assert_eq!(name, "my-project");
    }

    #[test]
    fn detect_app_name_sanitizes() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("My Cool App!!!");
        std::fs::create_dir_all(&sub).unwrap();
        let name = detect_app_name(&sub).unwrap();
        assert_eq!(name, "my-cool-app");
    }
}
```

**Step 2: Implement**

```rust
/// Detect the app name for the given directory.
/// Tries git remote origin first (extracts repo name from URL),
/// falls back to directory name. Result is sanitized for use as a subdomain.
pub fn detect_app_name(dir: &Path) -> Result<String> {
    // Try git remote
    if let Ok(output) = std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "remote", "get-url", "origin"])
        .output()
    {
        if output.status.success() {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(name) = extract_repo_name(&url) {
                return Ok(sanitize_subdomain(&name));
            }
        }
    }

    // Fall back to directory name
    let dir_name = dir
        .file_name()
        .context("directory has no name")?
        .to_string_lossy()
        .to_string();
    Ok(sanitize_subdomain(&dir_name))
}

/// Extract the repository name from a git remote URL.
/// Handles HTTPS (https://github.com/user/repo.git) and SSH (git@github.com:user/repo.git).
fn extract_repo_name(url: &str) -> Option<String> {
    let path = if url.contains("://") {
        // HTTPS: https://github.com/user/repo.git
        url.split('/').last()?
    } else if url.contains(':') {
        // SSH: git@github.com:user/repo.git
        url.split('/').last()?
    } else {
        return None;
    };
    let name = path.strip_suffix(".git").unwrap_or(path);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Sanitize a string for use as a DNS subdomain label:
/// lowercase, replace non-alphanumeric with hyphens, collapse consecutive hyphens,
/// trim leading/trailing hyphens, truncate to 63 chars.
fn sanitize_subdomain(s: &str) -> String {
    let lower = s.to_lowercase();
    let replaced: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let collapsed = replaced
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let truncated = if collapsed.len() > 63 {
        &collapsed[..63]
    } else {
        &collapsed
    };
    truncated.trim_end_matches('-').to_string()
}
```

**Step 3: Verify**

Run `cargo test -p devproxy config::tests::detect_app_name` — all 4 tests should pass.

---

### Task 2: Update `.devproxy-project` file format to include app name

**Files:**
- Modify: `src/config.rs`

**Step 1: Write the failing tests**

```rust
#[test]
fn project_file_roundtrip_with_app_name() {
    let dir = tempfile::tempdir().unwrap();
    write_project_file(dir.path(), "swift-penguin", Some("devproxy")).unwrap();
    let (slug, app_name) = read_project_file(dir.path()).unwrap();
    assert_eq!(slug, "swift-penguin");
    assert_eq!(app_name, Some("devproxy".to_string()));
}

#[test]
fn project_file_backward_compat_no_app_name() {
    let dir = tempfile::tempdir().unwrap();
    // Simulate legacy format: just the slug, no app name
    std::fs::write(dir.path().join(".devproxy-project"), "swift-penguin\n").unwrap();
    let (slug, app_name) = read_project_file(dir.path()).unwrap();
    assert_eq!(slug, "swift-penguin");
    assert_eq!(app_name, None);
}
```

**Step 2: Implement**

Update `write_project_file` signature to accept optional app name, write two lines when present. Update `read_project_file` to return `(String, Option<String>)`. Update all callers (`up.rs`, `down.rs`, `open.rs`).

```rust
pub fn write_project_file(dir: &Path, slug: &str, app_name: Option<&str>) -> Result<PathBuf> {
    let path = dir.join(".devproxy-project");
    let content = match app_name {
        Some(name) => format!("{slug}\n{name}\n"),
        None => format!("{slug}\n"),
    };
    std::fs::write(&path, content)?;
    Ok(path)
}

pub fn read_project_file(dir: &Path) -> Result<(String, Option<String>)> {
    let path = dir.join(".devproxy-project");
    let content = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no .devproxy-project file found in {}. Is this project running via `devproxy up`?",
            dir.display()
        )
    })?;
    let mut lines = content.lines();
    let slug = lines.next().context("empty .devproxy-project file")?.trim().to_string();
    let app_name = lines.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Ok((slug, app_name))
}
```

**Step 3: Verify**

Run `cargo test -p devproxy config::tests::project_file` — both roundtrip tests pass.

---

### Task 3: Update override file to include `devproxy.app` label

**Files:**
- Modify: `src/config.rs` (`write_override_file`)

**Step 1: Write the failing test**

```rust
#[test]
fn override_file_includes_app_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_override_file(dir.path(), "web", 51234, 3000, Some("devproxy")).unwrap();
    let content = std::fs::read_to_string(path).unwrap();
    assert!(content.contains("devproxy.app: devproxy"), "override should include app label: {content}");
}

#[test]
fn override_file_without_app_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_override_file(dir.path(), "web", 51234, 3000, None).unwrap();
    let content = std::fs::read_to_string(path).unwrap();
    assert!(!content.contains("devproxy.app"), "override should not include app label when None: {content}");
}
```

**Step 2: Implement**

Add `app_name: Option<&str>` parameter to `write_override_file`. When present, add a `labels` section to the service in the override YAML:

```rust
pub fn write_override_file(
    dir: &Path,
    service_name: &str,
    host_port: u16,
    container_port: u16,
    app_name: Option<&str>,
) -> Result<PathBuf> {
    // ... existing validation ...
    let labels_section = match app_name {
        Some(name) => format!("    labels:\n      devproxy.app: {name}\n"),
        None => String::new(),
    };
    let path = dir.join(".devproxy-override.yml");
    let content = format!(
        "services:\n  {service_name}:\n    ports:\n      - \"127.0.0.1:{host_port}:{container_port}\"\n{labels_section}"
    );
    std::fs::write(&path, &content)?;
    Ok(path)
}
```

Validate `app_name` the same way as `service_name` (only alphanumeric, hyphens, underscores) to prevent YAML injection.

**Step 3: Verify**

Run `cargo test -p devproxy config::tests::override_file` — both tests pass.

---

### Task 4: Update `up.rs` to detect app name and use new signatures

**Files:**
- Modify: `src/commands/up.rs`

**Step 1: No new tests needed** — this is wiring. The unit tests for detect_app_name and the e2e test cover this.

**Step 2: Implement**

```rust
pub fn run() -> Result<()> {
    let config = Config::load().context("run `devproxy init` first")?;
    let cwd = std::env::current_dir()?;
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_dir = compose_path.parent().context("compose file has no parent directory")?;
    // ... existing output ...

    let compose = config::parse_compose_file(&compose_path)?;
    let (service_name, container_port) = config::find_devproxy_service(&compose)?;

    // Detect app name from git remote or directory name
    let app_name = config::detect_app_name(&cwd)?;
    eprintln!("app: {}", app_name.cyan());

    let slug = slugs::generate_slug();
    eprintln!("slug: {}", slug.cyan());

    let host_port = config::find_free_port()?;
    eprintln!("host port: {}", host_port.to_string().cyan());

    // Pass app_name to override file (adds devproxy.app label)
    let override_path =
        config::write_override_file(compose_dir, &service_name, host_port, container_port, Some(&app_name))?;
    eprintln!("override: {}", override_path.display().to_string().cyan());

    // Write project file with app name
    config::write_project_file(compose_dir, &slug, Some(&app_name))?;

    // ... rest of daemon check, docker compose up, etc. ...

    let url = format!("https://{slug}.{app_name}.{}", config.domain);
    eprintln!();
    eprintln!("{} {}", "->".green().bold(), url.green().bold());

    Ok(())
}
```

**Step 3: Verify**

`cargo build` succeeds. Manual test or e2e test confirms the URL format.

---

### Task 5: Update `docker.rs` to read `devproxy.app` label and construct app-named hostnames

**Files:**
- Modify: `src/proxy/docker.rs`

**Step 1: Write failing test (none practical here)** — docker.rs functions are async and require Docker. Tested via e2e.

**Step 2: Implement**

In `inspect_container`, read `devproxy.app` label in addition to `com.docker.compose.project`. Construct the slug for `router.insert` as `{project_name}.{app_name}` when the app label is present, or just `{project_name}` for backward compatibility:

```rust
async fn inspect_container(container_id: &str) -> Result<Option<(String, u16)>> {
    // ... existing code ...

    let slug = match inspect.config.labels.get("com.docker.compose.project") {
        Some(s) => s.clone(),
        None => return Ok(None),
    };

    // Read app name label if present
    let app_name = inspect.config.labels.get("devproxy.app").cloned();

    // Construct the router key: {slug}.{app_name} if app_name present, else just {slug}
    let router_key = match app_name {
        Some(ref name) => format!("{slug}.{name}"),
        None => slug,
    };

    // ... find host_port ...

    match host_port {
        Some(port) => Ok(Some((router_key, port))),
        None => Ok(None),
    }
}
```

Similarly, in `watch_events_inner`, when handling `die`/`stop`/`kill` events, read the `devproxy.app` attribute from the event to construct the correct key for `router.remove`:

```rust
"die" | "stop" | "kill" => {
    let attrs = event
        .get("Actor").or_else(|| event.get("actor"))
        .and_then(|a| a.get("Attributes").or_else(|| a.get("attributes")));

    let slug = attrs.and_then(|a| a.get("com.docker.compose.project").and_then(|v| v.as_str()));
    let app_name = attrs.and_then(|a| a.get("devproxy.app").and_then(|v| v.as_str()));

    if let Some(slug) = slug {
        let router_key = match app_name {
            Some(name) => format!("{slug}.{name}"),
            None => slug.to_string(),
        };
        eprintln!("  route removed: {router_key}");
        router.remove(&router_key);
    }
}
```

**Step 3: Verify**

`cargo build` succeeds. E2e test will verify full flow.

---

### Task 6: Update `down.rs` and `open.rs` to use new project file format

**Files:**
- Modify: `src/commands/down.rs`
- Modify: `src/commands/open.rs`

**Step 1: No new tests** — covered by existing callers and e2e.

**Step 2: Implement**

`down.rs`: Update to destructure `(slug, _app_name)` from `read_project_file`. The slug is still the compose project name, so no behavior change here.

`open.rs`: Update to construct `{slug}.{app_name}.{domain}` when app_name is present:

```rust
pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let (slug, app_name) = config::read_project_file(&cwd)?;
    let config = Config::load()?;
    let full_host = match app_name {
        Some(ref name) => format!("{slug}.{name}.{}", config.domain),
        None => format!("{slug}.{}", config.domain),
    };
    // ... rest unchanged, use full_host for lookup and URL ...
}
```

**Step 3: Verify**

`cargo build` succeeds.

---

### Task 7: Update `ls.rs` to show `*` indicator for current directory's project

**Files:**
- Modify: `src/commands/ls.rs`

**Step 1: Write the failing test**

Add a unit test for the formatting logic:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::RouteInfo;

    #[test]
    fn format_route_with_current_marker() {
        let route = RouteInfo {
            slug: "swift-penguin.devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, Some("swift-penguin.devproxy.mysite.dev"));
        assert!(line.contains("*"), "current project should have * marker");
    }

    #[test]
    fn format_route_without_current_marker() {
        let route = RouteInfo {
            slug: "bold-fox.devproxy.mysite.dev".to_string(),
            port: 51235,
        };
        let line = format_route_line(&route, Some("swift-penguin.devproxy.mysite.dev"));
        assert!(!line.contains("*"), "non-current project should not have * marker");
    }

    #[test]
    fn format_route_no_current_project() {
        let route = RouteInfo {
            slug: "swift-penguin.devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, None);
        assert!(!line.contains("*"), "no current project means no marker");
    }
}
```

**Step 2: Implement**

```rust
use crate::config::{self, Config};
use crate::ipc::{self, Request, Response, RouteInfo};
use anyhow::{Result, bail};
use colored::Colorize;

/// Format a single route line, with optional `*` marker for current project.
fn format_route_line(route: &RouteInfo, current_slug: Option<&str>) -> String {
    let marker = match current_slug {
        Some(s) if s == route.slug => "* ",
        _ => "  ",
    };
    format!(
        "{}{:<30} {:<10}",
        marker,
        format!("https://{}", route.slug),
        route.port
    )
}

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    // Try to read current project slug (silently ignore failures)
    let current_slug = std::env::current_dir()
        .ok()
        .and_then(|cwd| config::read_project_file(&cwd).ok())
        .and_then(|(slug, app_name)| {
            let config = Config::load().ok()?;
            let full = match app_name {
                Some(name) => format!("{slug}.{name}.{}", config.domain),
                None => format!("{slug}.{}", config.domain),
            };
            Some(full)
        });

    match response {
        Response::Routes { routes } => {
            if routes.is_empty() {
                println!("no active projects");
            } else {
                println!("  {:<30} {:<10}", "SLUG".bold(), "PORT".bold());
                for route in &routes {
                    println!("{}", format_route_line(route, current_slug.as_deref()));
                }
                println!();
                println!("{} active project(s)", routes.len());
            }
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        _ => bail!("unexpected response from daemon"),
    }

    Ok(())
}
```

**Step 3: Verify**

Run `cargo test -p devproxy commands::ls::tests` — all 3 tests pass.

---

### Task 8: Update TLS cert to include `*.*.<domain>` SAN

**Files:**
- Modify: `src/proxy/cert.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn wildcard_cert_covers_two_level_subdomain() {
    // The generated cert should have *.*.domain as a SAN
    let dir = tempfile::tempdir().unwrap();
    let domain = "test.dev";
    generate_ca_and_cert(dir.path(), domain).unwrap();
    let cert_pem = std::fs::read_to_string(dir.path().join("tls-cert.pem")).unwrap();
    let cert = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    let parsed = x509_parser::parse_x509_certificate(&cert).unwrap().1;
    let sans: Vec<String> = parsed
        .subject_alternative_name()
        .unwrap()
        .unwrap()
        .value
        .general_names
        .iter()
        .filter_map(|n| match n {
            x509_parser::extensions::GeneralName::DNSName(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert!(sans.contains(&format!("*.*.{domain}")), "cert should have *.*.domain SAN: {sans:?}");
}
```

Note: This test may require adding `x509-parser` as a dev dependency, or alternatively inspect the cert PEM text. A simpler approach: just verify the cert generation code adds the SAN and test the full flow via e2e curl.

**Step 2: Implement**

In `cert.rs`, where the wildcard cert SANs are set, add `*.*.{domain}` alongside the existing `*.{domain}`:

```rust
// In the cert generation function, where SANs are added:
let subject_alt_names = vec![
    format!("*.{domain}"),
    format!("*.*.{domain}"),
    domain.to_string(),
];
```

**Step 3: Verify**

`cargo build` succeeds. The e2e test will verify TLS works with the two-level subdomain.

---

### Task 9: Update e2e test and add new unit tests

**Files:**
- Modify: `tests/e2e.rs`

**Step 1: Update existing e2e**

The `test_full_e2e_workflow` test extracts the slug from `up` output. Update the slug extraction to handle the new URL format `https://{slug}.{app-name}.{domain}`:

```rust
// In test_full_e2e_workflow, update slug extraction:
// Old: splits on first dot to get slug
// New: URL is https://slug.app-name.test.devproxy.dev
// The slug is the first subdomain component
let slug = up_stderr
    .lines()
    .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
    .and_then(|l| l.split("https://").nth(1).and_then(|s| s.split('.').next()))
    .expect("should find slug in up output");
```

The slug extraction already takes the first dot-separated component, so it should work. But the `--project-name` is still the slug (e.g., `swift-penguin`), and the route in the daemon now uses `swift-penguin.{app-name}.test.devproxy.dev`. Update the ls assertion and curl resolve accordingly.

Also update the `ls` output check since routes now have the app name component, and verify the `*` indicator appears when running `ls` from the fixture directory.

**Step 2: Verify**

Run `cargo test --test e2e` (non-ignored tests) to verify compilation. Full e2e: `cargo test --test e2e -- --ignored test_full_e2e_workflow`.

---

### Task 10: Update `init.rs` DNS instructions

**Files:**
- Modify: `src/commands/init.rs`

**Step 1: No test needed** — output text change only.

**Step 2: Implement**

The DNS instructions should note that wildcard DNS covers all subdomain levels, so no additional setup is needed for the new URL format. No change may be needed if the instructions already say `*.{domain}`. Verify and update if the instructions are specific to single-level subdomains.

**Step 3: Verify**

Run `cargo test --test e2e test_init_output` — existing init output tests should still pass.
