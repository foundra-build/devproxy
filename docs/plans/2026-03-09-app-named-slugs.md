# App-Named Slugs — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Make devproxy URLs include the app/repo name so users can identify which project a URL belongs to. Change the URL format from `https://{slug}.{domain}` to `https://{slug}-{app-name}.{domain}` where `{app-name}` is derived from the git remote origin (GitHub repo name), falling back to the directory name. Also enhance `devproxy ls` to mark the current directory's project with `*`.

**Architecture:**

The URL format changes from `https://swift-penguin.mysite.dev` to `https://swift-penguin-devproxy.mysite.dev` where `devproxy` is the app name (derived from the repo name of the cwd where `devproxy up` was run). Multiple instances of the same repo (e.g., worktrees) get different random slugs but share the same app-name suffix: `https://bold-fox-devproxy.mysite.dev`, `https://calm-otter-devproxy.mysite.dev`.

**Why a single subdomain level:** RFC 6125 Section 6.4.3 specifies that wildcard certificates (`*.domain`) only match a single DNS label. The existing TLS cert uses `*.mysite.dev` which matches `anything.mysite.dev` but would NOT match `a.b.mysite.dev`. By keeping the format as `{slug}-{appname}.{domain}` (a single subdomain label), the existing wildcard cert works without modification. No cert changes are needed.

**Key mechanism:** The `devproxy up` command detects the app name and combines it with the random slug to form the Docker Compose project name: `{slug}-{app-name}` (e.g., `swift-penguin-devproxy`). This composite name is used as the `--project-name` for `docker compose`, which means the `com.docker.compose.project` label on the container already contains the full routing key. The daemon's docker.rs reads this label and inserts it into the router as before — no daemon-side changes needed.

This is the simplest possible approach: the app name is baked into the compose project name, which Docker propagates as a label, which the daemon already reads for routing. No new labels, no docker.rs changes, no cert changes.

**App name detection:** A new function `detect_app_name(dir: &Path) -> Result<String>` in `config.rs`:
1. Run `git -C {dir} remote get-url origin` and parse the repo name from the URL (handles both HTTPS and SSH formats). Strip `.git` suffix if present.
2. If git fails or there's no remote, fall back to the directory name.
3. Sanitize the result: lowercase, replace non-alphanumeric chars with hyphens, collapse consecutive hyphens, trim leading/trailing hyphens. No truncation at this stage — truncation happens when composing the full slug.

**Composite slug and DNS label limit:** A new function `compose_slug(random_slug: &str, app_name: &str) -> String` in `config.rs` joins them as `{random_slug}-{app_name}` and then truncates the result to 63 characters (the RFC 1035 DNS label limit), trimming any trailing hyphen that the truncation might produce. The longest random slug is `bright-penguin` (14 chars); typical app names are short (e.g., `devproxy` = 8 chars, composite = 23 chars). Truncation only fires for unusually long app names.

**Compose project name:** Currently `up.rs` generates a random slug (e.g., `swift-penguin`) and uses it as `--project-name`. The new behavior calls `compose_slug(&random_slug, &app_name)` to form the composite (e.g., `swift-penguin-devproxy`) and uses that as `--project-name`. The `.devproxy-project` file stores this composite name. The daemon reads `com.docker.compose.project` which already equals this composite name — no daemon changes needed.

**`.devproxy-project` file:** No format change needed. The file continues to store the compose project name (now the composite `{slug}-{app-name}`). The `read_project_file` and `write_project_file` signatures remain the same.

**`devproxy ls` current-directory indicator:** The `ls` command reads `.devproxy-project` from the cwd (if it exists, failing silently if not) to get the current project's composite slug. When printing routes, any route matching the cwd's slug gets a `*` prefix.

**Tech Stack:** No new dependencies. Uses `std::process::Command` to run `git`.

---

### Task 1: Add `detect_app_name()` and helpers to `config.rs`

**Files:**
- Modify: `src/config.rs`

**Step 1: Write the failing tests**

Add unit tests for app name detection, repo name extraction, and subdomain sanitization:

```rust
#[cfg(test)]
mod tests {
    // ... existing tests ...

    #[test]
    fn detect_app_name_from_git_remote_https() {
        let dir = tempfile::tempdir().unwrap();
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

    #[test]
    fn extract_repo_name_https() {
        assert_eq!(
            extract_repo_name("https://github.com/user/repo.git"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn extract_repo_name_ssh() {
        assert_eq!(
            extract_repo_name("git@github.com:user/repo.git"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn extract_repo_name_no_git_suffix() {
        assert_eq!(
            extract_repo_name("https://github.com/user/repo"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn sanitize_subdomain_basic() {
        assert_eq!(sanitize_subdomain("My Cool App!!!"), "my-cool-app");
    }

    #[test]
    fn sanitize_subdomain_already_clean() {
        assert_eq!(sanitize_subdomain("my-app"), "my-app");
    }

    #[test]
    fn sanitize_subdomain_does_not_truncate() {
        let long_name = "a".repeat(100);
        let result = sanitize_subdomain(&long_name);
        assert_eq!(result.len(), 100, "sanitize_subdomain should not truncate");
    }

    #[test]
    fn compose_slug_basic() {
        assert_eq!(compose_slug("swift-penguin", "devproxy"), "swift-penguin-devproxy");
    }

    #[test]
    fn compose_slug_truncates_to_63_chars() {
        let long_app = "a".repeat(100);
        let result = compose_slug("swift-penguin", &long_app);
        assert!(result.len() <= 63, "composite slug must fit in a DNS label: len={}", result.len());
        assert!(!result.ends_with('-'), "should not end with hyphen after truncation");
        assert!(result.starts_with("swift-penguin-"), "should preserve the random slug prefix");
    }

    #[test]
    fn compose_slug_normal_lengths_not_truncated() {
        let result = compose_slug("bold-fox", "my-cool-app");
        assert_eq!(result, "bold-fox-my-cool-app");
    }
}
```

**Step 2: Implement**

```rust
/// Detect the app name for the given directory.
/// Tries git remote origin first (extracts repo name from URL),
/// falls back to directory name. Result is sanitized for use in a subdomain label.
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
    // For SSH URLs like git@github.com:user/repo.git, split on ':' first then '/'
    let path_part = if url.contains("://") {
        url.split('/').last()?
    } else if let Some(after_colon) = url.split(':').nth(1) {
        after_colon.split('/').last()?
    } else {
        return None;
    };
    let name = path_part.strip_suffix(".git").unwrap_or(path_part);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Sanitize a string for use as part of a DNS subdomain label:
/// lowercase, replace non-alphanumeric with hyphens, collapse consecutive hyphens,
/// trim leading/trailing hyphens. Does NOT truncate — callers that need length
/// enforcement should use `compose_slug` which truncates the composite result.
fn sanitize_subdomain(s: &str) -> String {
    let lower = s.to_lowercase();
    let replaced: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    replaced
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Compose a DNS-safe slug from a random slug and app name.
/// Joins as `{random_slug}-{app_name}` and truncates the result to 63 characters
/// (the RFC 1035 DNS label limit), trimming any trailing hyphen.
pub fn compose_slug(random_slug: &str, app_name: &str) -> String {
    let composite = format!("{random_slug}-{app_name}");
    if composite.len() <= 63 {
        return composite;
    }
    composite[..63].trim_end_matches('-').to_string()
}
```

**Step 3: Verify**

Run `cargo test config::tests` — all new and existing tests should pass.

---

### Task 2: Update `up.rs` to detect app name and form composite slug

**Files:**
- Modify: `src/commands/up.rs`

**Step 1: No new unit tests needed** — this is wiring. The unit tests for `detect_app_name` and the e2e test cover the behavior.

**Step 2: Implement**

Change slug generation from:
```rust
let slug = slugs::generate_slug();
```
to:
```rust
let app_name = config::detect_app_name(&cwd)?;
eprintln!("app: {}", app_name.cyan());

let random_slug = slugs::generate_slug();
let slug = config::compose_slug(&random_slug, &app_name);
eprintln!("slug: {}", slug.cyan());
```

Everything downstream already uses `slug` as the compose project name and the value written to `.devproxy-project`. The URL output line changes from:
```rust
let url = format!("https://{slug}.{}", config.domain);
```
This remains correct since `slug` is now the composite `swift-penguin-devproxy`.

No signature changes to `write_project_file`, `write_override_file`, or any other function.

**Step 3: Verify**

`cargo build` succeeds. The `test_up_without_label` and `test_up_without_compose_file` e2e tests still pass (they fail before reaching slug generation).

---

### Task 3: Update `ls.rs` to show `*` indicator for current directory's project

**Files:**
- Modify: `src/commands/ls.rs`

**Step 1: Write the failing tests**

Add a unit test module for the formatting logic. The `format_route_line` function is extracted to be testable independently of IPC:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::RouteInfo;

    #[test]
    fn format_route_with_current_marker() {
        let route = RouteInfo {
            slug: "swift-penguin-devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, Some("swift-penguin-devproxy.mysite.dev"));
        assert!(line.contains("*"), "current project should have * marker: {line}");
    }

    #[test]
    fn format_route_without_current_marker() {
        let route = RouteInfo {
            slug: "bold-fox-devproxy.mysite.dev".to_string(),
            port: 51235,
        };
        let line = format_route_line(&route, Some("swift-penguin-devproxy.mysite.dev"));
        assert!(!line.contains("*"), "non-current project should not have * marker: {line}");
    }

    #[test]
    fn format_route_no_current_project() {
        let route = RouteInfo {
            slug: "swift-penguin-devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, None);
        assert!(!line.contains("*"), "no current project means no marker: {line}");
    }
}
```

**Step 2: Implement**

Refactor `ls.rs` to:
1. Extract a `format_route_line` function that takes a route and optional current slug.
2. In `run()`, attempt to read `.devproxy-project` from the cwd (silently ignore failures) and construct the full hostname to match against.
3. Print each route using `format_route_line`, prepending `*` for the current project or a space for others.

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
        "{}{:<40} {:<10}",
        marker,
        format!("https://{}", route.slug),
        route.port
    )
}

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    // Try to read current project slug from cwd (silently ignore failures)
    let current_slug = std::env::current_dir()
        .ok()
        .and_then(|cwd| config::read_project_file(&cwd).ok())
        .and_then(|slug| {
            let config = Config::load().ok()?;
            Some(format!("{slug}.{}", config.domain))
        });

    match response {
        Response::Routes { routes } => {
            if routes.is_empty() {
                println!("no active projects");
            } else {
                println!("  {:<40} {:<10}", "URL".bold(), "PORT".bold());
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

Note: The column header changes from `SLUG` to `URL` since the displayed value is now the full `https://` URL which includes the app name. The column width increases from 30 to 40 to accommodate longer composite slugs.

**Step 3: Verify**

Run `cargo test ls::tests` — all 3 tests pass.

---

### Task 4: Update e2e tests

**Files:**
- Modify: `tests/e2e.rs`

**Step 1: Update existing e2e tests**

The `test_full_e2e_workflow` test extracts the slug from `up` output by splitting on the first dot. This still works because the composite slug (`swift-penguin-devproxy`) is the first dot-separated component of `swift-penguin-devproxy.test.devproxy.dev`.

However, several things need updating:
1. The slug is now composite (e.g., `swift-penguin-devproxy`) — it includes the app name suffix derived from the fixtures directory name. Since `copy_fixtures` copies into a temp dir with a name like `devproxy-fixtures-e2e-{pid}`, the app name will be derived from that directory name (there's no git remote in the fixture copy). Account for this in assertions.
2. The `ls` output now shows `*` for the current directory's project. Add an assertion for this.
3. The `curl` resolve and host must use the composite slug.

The e2e test runs `devproxy up` from the fixtures directory. Since the fixtures directory has no git remote, `detect_app_name` falls back to the directory name. The copy is at a path like `/tmp/devproxy-fixtures-e2e-{pid}`, so the sanitized directory name becomes something like `devproxy-fixtures-e2e-{pid}`. The slug in the `up` output will be `{adj}-{animal}-devproxy-fixtures-e2e-{pid}`.

To make e2e testing cleaner, we can initialize a git repo with a known remote in the fixture copy. Add this after `copy_fixtures`:

```rust
// Initialize a git repo with a known remote so detect_app_name is predictable
std::process::Command::new("git")
    .args(["init"])
    .current_dir(&fixtures)
    .output()
    .expect("git init failed");
std::process::Command::new("git")
    .args(["remote", "add", "origin", "https://github.com/test/e2e-fixture.git"])
    .current_dir(&fixtures)
    .output()
    .expect("git remote add failed");
```

Then the app name will always be `e2e-fixture` and the slug will be like `swift-penguin-e2e-fixture`.

Update slug extraction to handle the composite format. The existing extraction (`split('.').next()`) already returns the full first label, which is now `swift-penguin-e2e-fixture`. Update assertions accordingly.

For the `ls` check from the fixture directory, verify that the `*` indicator appears:
```rust
let ls_output = Command::new(devproxy_bin())
    .args(["ls"])
    .current_dir(&fixtures)  // Run from fixtures dir to get * indicator
    .env("DEVPROXY_CONFIG_DIR", &config_dir)
    .output()
    .expect("failed to run devproxy ls");
let ls_stdout = String::from_utf8_lossy(&ls_output.stdout);
assert!(ls_stdout.contains(slug), "ls should show our slug '{slug}': {ls_stdout}");
assert!(ls_stdout.contains("*"), "ls should show * for current project: {ls_stdout}");
```

Also update the `test_self_healing_route_removed_on_container_die` and `test_daemon_restart_rebuilds_routes` tests similarly (add git init + remote to fixture copies, update slug extraction).

**Step 2: Verify**

Run `cargo test --test e2e` (non-ignored) to verify compilation. Full e2e: `cargo test --test e2e -- --ignored test_full_e2e_workflow`.
