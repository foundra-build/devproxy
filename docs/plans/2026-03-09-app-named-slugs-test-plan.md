# App-Named Slugs — Test Plan

## Strategy reconciliation

The agreed strategy was **Medium fidelity**: unit tests for repo name detection, slug composition, and ls formatting, plus update existing e2e tests. After reviewing the implementation plan against the codebase:

- **Strategy holds.** The plan's architecture is straightforward: new pure functions in `config.rs`, wiring in `up.rs`, formatting in `ls.rs`. No new dependencies, no daemon-side changes, no cert changes.
- **One adjustment (no cost change):** The implementation plan's `ls` tests compare `route.slug` against a `current_slug` parameter. In the actual codebase, `RouteInfo.slug` contains the **full hostname** (e.g., `swift-penguin-devproxy.mysite.dev`), not just the compose project name. The plan already accounts for this (constructing `format!("{slug}.{}", config.domain)` in the `run()` function). The test plan below uses full hostnames in `RouteInfo.slug` to match reality.

## Sources of truth

- **S1**: Implementation plan (`docs/plans/2026-03-09-app-named-slugs.md`) — defines URL format, detection logic, composition rules, ls behavior
- **S2**: RFC 1035 Section 2.3.4 — DNS label limit of 63 characters
- **S3**: Git remote URL formats — HTTPS (`https://github.com/user/repo.git`) and SSH (`git@github.com:user/repo.git`)
- **S4**: Current codebase behavior — `RouteInfo.slug` contains full hostname, `.devproxy-project` stores compose project name

---

## Test plan

### 1. Scenario: Full workflow produces app-named URL and ls shows current marker

- **Type**: scenario
- **Harness**: e2e test binary (`tests/e2e.rs`), requires Docker
- **Preconditions**: Test config dir with certs, running test daemon, fixture directory with git remote set to `https://github.com/test/e2e-fixture.git`
- **Actions**:
  1. Run `devproxy up` from the fixture directory
  2. Extract slug from output — should be `{adj}-{animal}-e2e-fixture`
  3. Verify `.devproxy-project` contains the composite slug
  4. Run `devproxy ls` from the fixture directory
  5. Verify ls output contains the composite slug
  6. Verify ls output contains `*` marker for current project
  7. Run curl through the proxy using the composite hostname
  8. Run `devproxy down`
- **Expected outcome**:
  - Up output contains URL of form `https://{adj}-{animal}-e2e-fixture.test.devproxy.dev` [S1]
  - `.devproxy-project` stores the composite slug [S1, S4]
  - ls shows the composite slug with `*` marker when run from the fixture dir [S1]
  - curl succeeds through the proxy using the composite hostname [S1]
  - Down cleans up `.devproxy-project` and `.devproxy-override.yml` [S4]
- **Interactions**: Docker compose project naming, daemon route table, TLS cert wildcard matching

### 2. Scenario: Self-healing works with composite slugs

- **Type**: scenario
- **Harness**: e2e test binary (`tests/e2e.rs`), requires Docker
- **Preconditions**: Test config dir, running daemon, fixture with git remote
- **Actions**:
  1. Run `devproxy up`, extract composite slug
  2. Verify route appears in `devproxy ls`
  3. Kill container externally via `docker compose kill`
  4. Wait for event watcher to process
  5. Verify route removed from `devproxy ls`
- **Expected outcome**: Route with composite slug is added on start and removed on die [S1, S4]
- **Interactions**: Docker event watcher reads `com.docker.compose.project` label which now contains the composite slug

### 3. Scenario: Daemon restart rebuilds routes with composite slugs

- **Type**: scenario
- **Harness**: e2e test binary (`tests/e2e.rs`), requires Docker
- **Preconditions**: Test config dir, running daemon, fixture with git remote, container running
- **Actions**:
  1. Run `devproxy up`, extract composite slug
  2. Kill daemon process
  3. Start new daemon
  4. Verify composite slug appears in `devproxy ls`
- **Expected outcome**: Route with composite slug is rebuilt from running container [S1, S4]
- **Interactions**: Docker inspect reads `com.docker.compose.project` which contains composite slug

### 4. Integration: ls without current project shows no marker

- **Type**: integration
- **Harness**: e2e test binary (`tests/e2e.rs`), requires Docker
- **Preconditions**: Test config dir, running daemon, container running via `devproxy up` from fixture dir
- **Actions**:
  1. Run `devproxy ls` from a **different** directory (not the fixture dir)
- **Expected outcome**: ls output shows the route but without `*` marker [S1]
- **Interactions**: ls reads `.devproxy-project` from cwd — should fail silently when not present

### 5. Unit: extract_repo_name from HTTPS URL

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `extract_repo_name("https://github.com/user/repo.git")`
- **Expected outcome**: Returns `Some("repo")` [S3]
- **Interactions**: None

### 6. Unit: extract_repo_name from SSH URL

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `extract_repo_name("git@github.com:user/repo.git")`
- **Expected outcome**: Returns `Some("repo")` [S3]
- **Interactions**: None

### 7. Unit: extract_repo_name from URL without .git suffix

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `extract_repo_name("https://github.com/user/repo")`
- **Expected outcome**: Returns `Some("repo")` [S3]
- **Interactions**: None

### 8. Unit: detect_app_name from git remote (HTTPS)

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`, creates temp dir with `git init` + remote
- **Preconditions**: Temp dir initialized as git repo with HTTPS remote `https://github.com/user/my-cool-app.git`
- **Actions**: Call `detect_app_name(dir)`
- **Expected outcome**: Returns `"my-cool-app"` [S1, S3]
- **Interactions**: Spawns `git` subprocess

### 9. Unit: detect_app_name from git remote (SSH)

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`, creates temp dir with `git init` + remote
- **Preconditions**: Temp dir initialized as git repo with SSH remote `git@github.com:user/another-app.git`
- **Actions**: Call `detect_app_name(dir)`
- **Expected outcome**: Returns `"another-app"` [S1, S3]
- **Interactions**: Spawns `git` subprocess

### 10. Unit: detect_app_name falls back to directory name

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`, creates temp dir without git
- **Preconditions**: Temp dir named `my-project`, no git repo
- **Actions**: Call `detect_app_name(dir)`
- **Expected outcome**: Returns `"my-project"` [S1]
- **Interactions**: Spawns `git` subprocess (which fails), then reads dir name

### 11. Unit: detect_app_name sanitizes directory name

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: Temp dir named `My Cool App!!!`
- **Actions**: Call `detect_app_name(dir)`
- **Expected outcome**: Returns `"my-cool-app"` [S1]
- **Interactions**: Spawns `git` subprocess (fails), sanitizes dir name

### 12. Unit: sanitize_subdomain basic case

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `sanitize_subdomain("My Cool App!!!")`
- **Expected outcome**: Returns `"my-cool-app"` [S1]
- **Interactions**: None

### 13. Unit: sanitize_subdomain already clean

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `sanitize_subdomain("my-app")`
- **Expected outcome**: Returns `"my-app"` [S1]
- **Interactions**: None

### 14. Unit: sanitize_subdomain does not truncate

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `sanitize_subdomain(&"a".repeat(100))`
- **Expected outcome**: Returns string of length 100 [S1 — truncation is compose_slug's job]
- **Interactions**: None

### 15. Unit: compose_slug basic case

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `compose_slug("swift-penguin", "devproxy")`
- **Expected outcome**: Returns `"swift-penguin-devproxy"` [S1]
- **Interactions**: None

### 16. Boundary: compose_slug truncates to 63 chars

- **Type**: boundary
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `compose_slug("swift-penguin", &"a".repeat(100))`
- **Expected outcome**: Result length <= 63, does not end with `-`, starts with `"swift-penguin-"` [S1, S2]
- **Interactions**: None

### 17. Unit: compose_slug normal lengths not truncated

- **Type**: unit
- **Harness**: `#[test]` in `config.rs`
- **Preconditions**: None
- **Actions**: Call `compose_slug("bold-fox", "my-cool-app")`
- **Expected outcome**: Returns `"bold-fox-my-cool-app"` [S1]
- **Interactions**: None

### 18. Unit: format_route_line with current marker

- **Type**: unit
- **Harness**: `#[test]` in `ls.rs`
- **Preconditions**: None
- **Actions**: Call `format_route_line` with a route whose slug matches the current slug
- **Expected outcome**: Output contains `*` [S1]
- **Interactions**: None

### 19. Unit: format_route_line without current marker

- **Type**: unit
- **Harness**: `#[test]` in `ls.rs`
- **Preconditions**: None
- **Actions**: Call `format_route_line` with a route whose slug does NOT match the current slug
- **Expected outcome**: Output does not contain `*` [S1]
- **Interactions**: None

### 20. Unit: format_route_line with no current project

- **Type**: unit
- **Harness**: `#[test]` in `ls.rs`
- **Preconditions**: None
- **Actions**: Call `format_route_line` with `current_slug = None`
- **Expected outcome**: Output does not contain `*` [S1]
- **Interactions**: None

---

## Coverage summary

### Covered

- App name detection from git remote (HTTPS and SSH formats)
- App name fallback to directory name
- Subdomain sanitization (special chars, case, consecutive hyphens)
- Composite slug composition (join and truncation)
- DNS label length limit (63 chars) enforcement
- ls current-directory `*` marker (match, no match, no project)
- Full e2e workflow with composite slugs (up, ls, curl, down)
- Self-healing with composite slugs
- Daemon restart route rebuilding with composite slugs
- ls behavior from non-project directory

### Explicitly excluded per strategy

- **Daemon-side routing logic**: The plan explicitly states no daemon changes are needed. The composite slug flows through `com.docker.compose.project` label which the daemon already reads. Covered indirectly by e2e tests.
- **TLS cert changes**: No cert changes needed (single subdomain level maintained). Verified indirectly by curl in e2e test.
- **Performance**: No performance-critical changes. The only new work is one `git` subprocess call during `devproxy up`, which is already dominated by `docker compose up` time.

### Risks of exclusions

- If the composite slug somehow exceeds the wildcard cert's matching, the e2e curl test would catch this.
- If Docker Compose handles the longer project name differently (e.g., container naming), the e2e tests would catch this since they run real Docker containers.
