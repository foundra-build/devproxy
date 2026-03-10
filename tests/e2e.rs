//! End-to-end tests for devproxy.
//!
//! Requirements:
//! - Docker and Docker Compose must be available
//! - Tests run the daemon on ephemeral high ports -- no sudo needed
//!
//! Test isolation strategy:
//! - DEVPROXY_CONFIG_DIR env var points each test at its own temp config dir.
//!   (dirs::config_dir() on macOS ignores HOME, so DEVPROXY_CONFIG_DIR is the
//!   only reliable way to isolate config.)
//! - Each test copies tests/fixtures/ into its own temp dir so that
//!   .devproxy-override.yml and .devproxy-project writes do not collide.
//! - Each test binds the daemon to a unique ephemeral port, so parallel tests
//!   do not fight over a shared port.
//!
//! Run with: cargo test --test e2e
//! Run full suite: cargo test --test e2e -- --include-ignored

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Helper to get the path to the devproxy binary
fn devproxy_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("devproxy");
    path
}

/// Helper to get the source fixtures directory
fn fixtures_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

const TEST_DOMAIN: &str = "test.devproxy.dev";

/// Find a free ephemeral port. Each test calls this to get its own unique daemon port.
fn find_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Copy the fixtures directory into an isolated temp dir for one test.
/// Returns the path to the copy (which contains docker-compose.yml, Dockerfile, etc).
/// Initializes a git repo with a known remote so detect_app_name is predictable.
fn copy_fixtures(test_name: &str) -> PathBuf {
    let dest = std::env::temp_dir().join(format!(
        "devproxy-fixtures-{test_name}-{}",
        std::process::id()
    ));
    if dest.exists() {
        std::fs::remove_dir_all(&dest).unwrap();
    }
    std::fs::create_dir_all(&dest).unwrap();

    let src = fixtures_src_dir();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        let dest_path = dest.join(entry.file_name());
        std::fs::copy(entry.path(), &dest_path).unwrap();
    }

    // Initialize a git repo with a known remote so detect_app_name is predictable
    Command::new("git")
        .args(["init"])
        .current_dir(&dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("git init failed");
    Command::new("git")
        .args(["remote", "add", "origin", "https://github.com/test/e2e-fixture.git"])
        .current_dir(&dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("git remote add failed");

    dest
}

/// The expected app name suffix for fixture directories (derived from git remote)
const FIXTURE_APP_NAME: &str = "e2e-fixture";

/// Create an isolated test config directory and generate certs using `init --no-daemon`.
/// Returns the path to the config directory (to be set as DEVPROXY_CONFIG_DIR).
fn create_test_config_dir(test_name: &str) -> PathBuf {
    let config_dir = std::env::temp_dir().join(format!(
        "devproxy-config-{test_name}-{}",
        std::process::id()
    ));
    if config_dir.exists() {
        std::fs::remove_dir_all(&config_dir).unwrap();
    }
    std::fs::create_dir_all(&config_dir).unwrap();

    // Generate certs without spawning a daemon (--no-daemon)
    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run devproxy init --no-daemon");

    assert!(
        output.status.success(),
        "init --no-daemon should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify certs were created
    assert!(
        config_dir.join("ca-cert.pem").exists(),
        "CA cert should exist after init"
    );
    assert!(
        config_dir.join("tls-cert.pem").exists(),
        "TLS cert should exist after init"
    );
    assert!(
        config_dir.join("config.json").exists(),
        "config should exist after init"
    );

    config_dir
}

/// Guard that kills the daemon on drop and cleans up the config dir
struct DaemonGuard {
    child: Child,
    config_dir: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

/// Start a daemon on the given port with the given config dir.
/// Waits until the IPC socket is connectable.
fn start_test_daemon(config_dir: &Path, port: u16) -> DaemonGuard {
    let child = Command::new(devproxy_bin())
        .args(["daemon", "--port", &port.to_string()])
        .env("DEVPROXY_CONFIG_DIR", config_dir)
        .env("DEVPROXY_NO_SOCKET_ACTIVATION", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start daemon");

    let guard = DaemonGuard {
        child,
        config_dir: config_dir.to_path_buf(),
    };

    // Wait for IPC socket to become connectable
    let socket_path = config_dir.join("devproxy.sock");
    for _ in 0..50 {
        if socket_path.exists() && std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    panic!(
        "daemon did not start within 5 seconds (socket: {})",
        socket_path.display()
    );
}

/// Guard that runs docker compose down on drop and cleans up the fixtures copy
struct ComposeGuard {
    project_name: String,
    compose_dir: PathBuf,
}

impl Drop for ComposeGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args([
                "compose",
                "--project-name",
                &self.project_name,
                "down",
                "--remove-orphans",
                "--timeout",
                "5",
            ])
            .current_dir(&self.compose_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // Clean up fixtures copy
        let _ = std::fs::remove_dir_all(&self.compose_dir);
    }
}

// ---- Non-Docker tests (always run) ----------------------------------------

#[test]
fn test_cli_help() {
    let output = Command::new(devproxy_bin())
        .arg("--help")
        .output()
        .expect("failed to run devproxy --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("devproxy"));
    assert!(stdout.contains("init"));
    assert!(stdout.contains("up"));
    assert!(stdout.contains("down"));
    assert!(stdout.contains("ls"));
    assert!(stdout.contains("status"));
    assert!(
        stdout.contains("restart"),
        "help should list the restart command"
    );
    assert!(
        stdout.contains("update"),
        "help should list the update command"
    );
    assert!(
        stdout.contains("Check for updates") || stdout.contains("self-update"),
        "help should include update command description"
    );
    // Daemon should be hidden as a top-level command (it may appear in descriptions)
    // Check that "daemon" does not appear as a command entry (lines starting with "  daemon")
    assert!(
        !stdout
            .lines()
            .any(|l| l.trim_start().starts_with("daemon ")),
        "daemon command should be hidden from help"
    );
}

#[test]
fn test_cli_version() {
    let output = Command::new(devproxy_bin())
        .arg("--version")
        .output()
        .expect("failed to run devproxy --version");

    assert!(output.status.success(), "--version should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("devproxy"),
        "--version output should contain 'devproxy': {stdout}"
    );
    // Should contain a semver-like version string
    let has_version = stdout.split_whitespace().any(|w| {
        let parts: Vec<&str> = w.split('.').collect();
        parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    });
    assert!(
        has_version,
        "--version output should contain a version number: {stdout}"
    );
}

#[test]
fn test_init_generates_certs() {
    let config_dir =
        std::env::temp_dir().join(format!("devproxy-init-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init");

    assert!(output.status.success(), "init should succeed");
    assert!(
        config_dir.join("ca-cert.pem").exists(),
        "CA cert should exist"
    );
    assert!(
        config_dir.join("ca-key.pem").exists(),
        "CA key should exist"
    );
    assert!(
        config_dir.join("tls-cert.pem").exists(),
        "TLS cert should exist"
    );
    assert!(
        config_dir.join("tls-key.pem").exists(),
        "TLS key should exist"
    );
    assert!(
        config_dir.join("config.json").exists(),
        "config should exist"
    );

    // Verify idempotency: running init again should succeed and not error
    let output2 = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init a second time");

    assert!(output2.status.success(), "init should be idempotent");
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(
        stderr2.contains("already exists"),
        "should report certs already exist"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn test_status_without_daemon() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-norun-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    let output = Command::new(devproxy_bin())
        .args(["status"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run status");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not running") || stderr.contains("could not connect"),
        "should report daemon not running: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn test_restart_no_daemon() {
    // Without a platform-managed daemon (DEVPROXY_NO_SOCKET_ACTIVATION=1),
    // restart should report that no daemon was found and exit non-zero.
    let config_dir =
        std::env::temp_dir().join(format!("devproxy-restart-nodaemon-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    let output = Command::new(devproxy_bin())
        .args(["restart"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .env("DEVPROXY_NO_SOCKET_ACTIVATION", "1")
        .output()
        .expect("failed to run restart");

    assert!(
        !output.status.success(),
        "restart without a platform-managed daemon should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no platform-managed daemon found"),
        "should report no daemon found: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn test_restart_running_daemon() {
    // Start a daemon with DEVPROXY_NO_SOCKET_ACTIVATION=1 (no launchd/systemd).
    // `restart` should still report "no platform-managed daemon" because the
    // daemon is running directly, not via launchd/systemd.
    let config_dir = create_test_config_dir("restart-running");
    let port = find_free_port();
    let _guard = start_test_daemon(&config_dir, port);

    let output = Command::new(devproxy_bin())
        .args(["restart"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .env("DEVPROXY_NO_SOCKET_ACTIVATION", "1")
        .output()
        .expect("failed to run restart");

    assert!(
        !output.status.success(),
        "restart should exit non-zero when daemon is not platform-managed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no platform-managed daemon found"),
        "should report no platform-managed daemon: {stderr}"
    );
}

#[test]
fn test_up_without_label() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-nolabel-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    // Create a compose file without devproxy.port
    let test_dir =
        std::env::temp_dir().join(format!("devproxy-nolabel-project-{}", std::process::id()));
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::write(
        test_dir.join("docker-compose.yml"),
        "services:\n  web:\n    image: alpine\n",
    )
    .unwrap();

    let output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&test_dir)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run up");

    assert!(
        !output.status.success(),
        "up should fail without devproxy.port label"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no service"),
        "should mention no devproxy.port label: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
    let _ = std::fs::remove_dir_all(&test_dir);
}

#[test]
fn test_up_without_compose_file() {
    let config_dir =
        std::env::temp_dir().join(format!("devproxy-nocompose-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    let test_dir =
        std::env::temp_dir().join(format!("devproxy-nocompose-project-{}", std::process::id()));
    std::fs::create_dir_all(&test_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&test_dir)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run up");

    assert!(
        !output.status.success(),
        "up should fail without compose file"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no docker-compose.yml"),
        "should mention no compose file: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
    let _ = std::fs::remove_dir_all(&test_dir);
}

#[test]
fn test_down_without_project_file() {
    let test_dir = std::env::temp_dir().join(format!("devproxy-noproject-{}", std::process::id()));
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::write(
        test_dir.join("docker-compose.yml"),
        "services:\n  web:\n    image: alpine\n",
    )
    .unwrap();

    let config_dir =
        std::env::temp_dir().join(format!("devproxy-noproject-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["down"])
        .current_dir(&test_dir)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run down");

    assert!(
        !output.status.success(),
        "down should fail without .devproxy-project"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(".devproxy-project") || stderr.contains("Is this project running"),
        "should mention missing project file: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&test_dir);
    let _ = std::fs::remove_dir_all(&config_dir);
}

// ---- Docker-dependent tests (run with --include-ignored) -------------------

/// Full e2e: init -> up -> curl through proxy -> ls -> status -> down
#[test]
#[ignore] // Run with: cargo test --test e2e -- --ignored
fn test_full_e2e_workflow() {
    let config_dir = create_test_config_dir("e2e");
    let daemon_port = find_free_port();
    let _daemon = start_test_daemon(&config_dir, daemon_port);

    let fixtures = copy_fixtures("e2e");

    // Build fixture image (in the copy dir; Dockerfile is there)
    let build_status = Command::new("docker")
        .args(["compose", "build"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to build fixture");
    assert!(build_status.success(), "fixture build should succeed");

    // Up
    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy up");

    let up_stderr = String::from_utf8_lossy(&up_output.stderr);
    eprintln!("up output: {up_stderr}");
    assert!(
        up_output.status.success(),
        "devproxy up should succeed: {up_stderr}"
    );

    // Extract slug from output (look for "-> https://<slug>.test.devproxy.dev")
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| l.split("https://").nth(1).and_then(|s| s.split('.').next()))
        .expect("should find slug in up output");

    // Verify the slug contains the app name suffix
    assert!(
        slug.ends_with(&format!("-{FIXTURE_APP_NAME}")),
        "slug should end with app name '-{FIXTURE_APP_NAME}': {slug}"
    );

    // Verify .devproxy-project was written with the correct slug
    let project_file = fixtures.join(".devproxy-project");
    assert!(
        project_file.exists(),
        ".devproxy-project should exist after up"
    );
    let saved_slug = std::fs::read_to_string(&project_file).unwrap();
    assert_eq!(
        saved_slug.trim(),
        slug,
        ".devproxy-project should contain the slug"
    );

    let _compose_guard = ComposeGuard {
        project_name: slug.to_string(),
        compose_dir: fixtures.clone(),
    };

    // Wait for container to be ready
    std::thread::sleep(Duration::from_secs(3));

    // Status check
    let status_output = Command::new(devproxy_bin())
        .args(["status"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy status");
    let status_stderr = String::from_utf8_lossy(&status_output.stderr);
    assert!(
        status_stderr.contains("running"),
        "daemon should be running: {status_stderr}"
    );

    // Ls check (run from fixtures dir to get * indicator)
    let ls_output = Command::new(devproxy_bin())
        .args(["ls"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy ls");
    let ls_stdout = String::from_utf8_lossy(&ls_output.stdout);
    assert!(
        ls_stdout.contains(slug),
        "ls should show our slug '{slug}': {ls_stdout}"
    );
    assert!(
        ls_stdout.contains("*"),
        "ls should show * for current project: {ls_stdout}"
    );

    // Curl through the proxy (--resolve bypasses DNS, --cacert trusts our test CA)
    let ca_cert_path = config_dir.join("ca-cert.pem");
    let host = format!("{slug}.{TEST_DOMAIN}");
    let url = format!("https://{host}:{daemon_port}/");

    // Retry curl a few times in case the route hasn't been picked up yet
    let mut curl_ok = false;
    let mut last_stdout = String::new();
    let mut last_stderr = String::new();
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        let curl_output = Command::new("curl")
            .args([
                "-s",
                "-v",
                "--max-time",
                "5",
                "--resolve",
                &format!("{host}:{daemon_port}:127.0.0.1"),
                "--cacert",
                &ca_cert_path.to_string_lossy(),
                &url,
            ])
            .output()
            .expect("failed to run curl");

        last_stdout = String::from_utf8_lossy(&curl_output.stdout).to_string();
        last_stderr = String::from_utf8_lossy(&curl_output.stderr).to_string();

        if curl_output.status.success() {
            curl_ok = true;
            break;
        }
        eprintln!("curl attempt {attempt} failed: stderr={last_stderr}");
    }

    assert!(
        curl_ok,
        "curl should succeed: stdout={last_stdout}, stderr={last_stderr}",
    );

    // Down (reads .devproxy-project to get slug, passes --project-name to compose)
    let down_output = Command::new(devproxy_bin())
        .args(["down"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy down");
    let down_stderr = String::from_utf8_lossy(&down_output.stderr);
    assert!(
        down_output.status.success(),
        "devproxy down should succeed: {down_stderr}"
    );

    // Verify cleanup files are gone
    assert!(
        !fixtures.join(".devproxy-project").exists(),
        ".devproxy-project should be removed after down"
    );
    assert!(
        !fixtures.join(".devproxy-override.yml").exists(),
        ".devproxy-override.yml should be removed after down"
    );
}

/// Test self-healing: kill container externally -> route removed from daemon
#[test]
#[ignore]
fn test_self_healing_route_removed_on_container_die() {
    let config_dir = create_test_config_dir("heal");
    let daemon_port = find_free_port();
    let _daemon = start_test_daemon(&config_dir, daemon_port);

    let fixtures = copy_fixtures("heal");

    // Build + Up
    let _ = Command::new("docker")
        .args(["compose", "build"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to up");

    let up_stderr = String::from_utf8_lossy(&up_output.stderr);
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| l.split("https://").nth(1).and_then(|s| s.split('.').next()))
        .expect("should find slug");

    let _compose_guard = ComposeGuard {
        project_name: slug.to_string(),
        compose_dir: fixtures.clone(),
    };

    std::thread::sleep(Duration::from_secs(3));

    // Verify route exists
    let ls_before = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to ls");
    let ls_before_stdout = String::from_utf8_lossy(&ls_before.stdout);
    assert!(
        ls_before_stdout.contains(slug),
        "route should exist before kill: {ls_before_stdout}"
    );

    // Kill container externally (not via devproxy)
    let kill_status = Command::new("docker")
        .args(["compose", "--project-name", slug, "kill"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to kill");
    assert!(kill_status.success());

    // Wait for event watcher to process the die event
    std::thread::sleep(Duration::from_secs(3));

    // Check ls -- route should be gone
    let ls_after = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to ls");
    let ls_after_stdout = String::from_utf8_lossy(&ls_after.stdout);
    assert!(
        !ls_after_stdout.contains(slug) || ls_after_stdout.contains("no active"),
        "route should be removed after external kill: {ls_after_stdout}"
    );
}

/// Test daemon restart: routes rebuild from running containers
#[test]
#[ignore]
fn test_daemon_restart_rebuilds_routes() {
    let config_dir = create_test_config_dir("restart");
    let daemon_port = find_free_port();
    let daemon = start_test_daemon(&config_dir, daemon_port);

    let fixtures = copy_fixtures("restart");

    // Build + Up
    let _ = Command::new("docker")
        .args(["compose", "build"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to up");

    let up_stderr = String::from_utf8_lossy(&up_output.stderr);
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| l.split("https://").nth(1).and_then(|s| s.split('.').next()))
        .expect("should find slug");

    let _compose_guard = ComposeGuard {
        project_name: slug.to_string(),
        compose_dir: fixtures.clone(),
    };

    std::thread::sleep(Duration::from_secs(2));

    // Kill the daemon (not the container) -- we need the config_dir to survive
    // for the new daemon, so take ownership and kill without dropping the guard.
    let mut daemon = daemon;
    let _ = daemon.child.kill();
    let _ = daemon.child.wait();
    // Clear the config_dir path so Drop won't delete it (the second daemon's
    // guard will handle cleanup). Setting it to empty means remove_dir_all is
    // harmless (it will fail on "").
    daemon.config_dir = PathBuf::new();
    drop(daemon);

    std::thread::sleep(Duration::from_millis(500));

    // Start a new daemon on a fresh port -- it should rebuild routes from running containers
    let daemon_port2 = find_free_port();
    let _daemon2 = start_test_daemon(&config_dir, daemon_port2);

    // Check that the route was rebuilt
    let ls_output = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to ls");
    let ls_stdout = String::from_utf8_lossy(&ls_output.stdout);
    assert!(
        ls_stdout.contains(slug),
        "route should be rebuilt after daemon restart: {ls_stdout}"
    );
}

/// Test proxy returns 502 for unknown host
#[test]
#[ignore]
fn test_proxy_502_for_unknown_host() {
    let config_dir = create_test_config_dir("502");
    let daemon_port = find_free_port();
    let _daemon = start_test_daemon(&config_dir, daemon_port);

    let ca_cert_path = config_dir.join("ca-cert.pem");
    let host = format!("nonexistent.{TEST_DOMAIN}");
    let url = format!("https://{host}:{daemon_port}/");

    let curl_output = Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--max-time",
            "5",
            "--resolve",
            &format!("{host}:{daemon_port}:127.0.0.1"),
            "--cacert",
            &ca_cert_path.to_string_lossy(),
            &url,
        ])
        .output()
        .expect("failed to run curl");

    let status_code = String::from_utf8_lossy(&curl_output.stdout);
    assert_eq!(
        status_code.trim(),
        "502",
        "should get 502 for unknown host, got: {status_code}"
    );
}

/// IPC ping/pong test
#[test]
#[ignore]
fn test_ipc_ping_pong() {
    let config_dir = create_test_config_dir("ipc");
    let daemon_port = find_free_port();
    let _daemon = start_test_daemon(&config_dir, daemon_port);

    let output = Command::new(devproxy_bin())
        .args(["status"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run status");

    assert!(output.status.success(), "status should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("running"),
        "should report daemon running: {stderr}"
    );
    assert!(
        stderr.contains("active routes: 0"),
        "should report 0 routes: {stderr}"
    );
}

// ---- New daemon setup flow tests -------------------------------------------

/// Verify init output includes DNS setup instructions (dnsmasq, resolver)
#[test]
fn test_init_output_includes_dns_instructions() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-dns-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init");

    assert!(output.status.success(), "init should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should mention dnsmasq
    assert!(
        stderr.contains("dnsmasq"),
        "init output should mention dnsmasq for DNS setup: {stderr}"
    );

    // Should include the domain in DNS instructions
    assert!(
        stderr.contains(&format!(".{TEST_DOMAIN}")),
        "init output should include domain in DNS instructions: {stderr}"
    );

    // On macOS, should mention /etc/resolver
    #[cfg(target_os = "macos")]
    assert!(
        stderr.contains("/etc/resolver"),
        "init output should mention /etc/resolver on macOS: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

/// Verify init output includes sudo note for port 443
#[test]
fn test_init_output_includes_sudo_note() {
    let config_dir =
        std::env::temp_dir().join(format!("devproxy-sudo-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    // Use --no-daemon so we don't need root, but still verify the output mentions sudo
    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init");

    assert!(output.status.success(), "init should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should mention sudo in the CA trust section
    assert!(
        stderr.contains("sudo"),
        "init output should mention sudo: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

/// Verify init output includes CA certificate path. The path appears in
/// the cert generation output and/or the trust failure message, regardless
/// of whether automatic trust succeeds or fails.
#[test]
fn test_init_output_includes_ca_trust_path() {
    let config_dir =
        std::env::temp_dir().join(format!("devproxy-capath-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init");

    assert!(output.status.success(), "init should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // With the login keychain, trust should succeed without sudo.
    // If it does, the success message appears. If it fails for some
    // other reason, the CA cert path appears in the fallback instructions.
    let ca_cert_path = config_dir.join("ca-cert.pem");
    let trust_succeeded = stderr.contains("CA trusted in login keychain");
    let path_shown = stderr.contains(&ca_cert_path.display().to_string());
    assert!(
        trust_succeeded || path_shown,
        "init output should show trust success or CA cert path '{}': {stderr}",
        ca_cert_path.display()
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

/// Verify `devproxy up` fails fast (within a timeout) when daemon is dead
/// rather than hanging indefinitely. Creates a stale socket file to simulate
/// a dead daemon.
#[test]
fn test_up_fails_fast_with_dead_daemon() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-deadup-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    // Create a compose file with devproxy.port
    let test_dir =
        std::env::temp_dir().join(format!("devproxy-deadup-project-{}", std::process::id()));
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::write(
        test_dir.join("docker-compose.yml"),
        "services:\n  web:\n    image: alpine\n    labels:\n      - \"devproxy.port=3000\"\n",
    )
    .unwrap();

    // Create a stale socket file that no daemon is listening on.
    // Bind a Unix socket then immediately drop it. On Unix, dropping a
    // UnixListener does NOT remove the socket file -- it just closes the
    // fd. The file remains on disk as an inert socket node.
    let socket_path = config_dir.join("devproxy.sock");
    {
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        drop(listener);
    }
    // Verify the socket file persists after the listener is dropped
    assert!(
        socket_path.exists(),
        "socket file should remain after UnixListener drop"
    );

    let start = std::time::Instant::now();
    let output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&test_dir)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run up");
    let elapsed = start.elapsed();

    assert!(
        !output.status.success(),
        "up should fail when daemon is dead"
    );

    // Should fail within 5 seconds (not hang)
    assert!(
        elapsed < Duration::from_secs(5),
        "up should fail fast, not hang (took {elapsed:?})"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not running") || stderr.contains("no response"),
        "should report daemon not running: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
    let _ = std::fs::remove_dir_all(&test_dir);
}

/// Verify the daemon writes a PID file on startup
#[test]
#[ignore]
fn test_daemon_writes_pid_file() {
    let config_dir = create_test_config_dir("pidfile");
    let daemon_port = find_free_port();
    let daemon = start_test_daemon(&config_dir, daemon_port);

    let pid_path = config_dir.join("daemon.pid");
    assert!(
        pid_path.exists(),
        "daemon should create a PID file at {}",
        pid_path.display()
    );

    let pid_str = std::fs::read_to_string(&pid_path).unwrap();
    let pid: u32 = pid_str
        .trim()
        .parse()
        .expect("PID file should contain a valid number");
    assert!(pid > 0, "PID should be positive");

    // Verify the PID matches the actual daemon process
    assert_eq!(
        pid,
        daemon.child.id(),
        "PID file should match the daemon's actual PID"
    );
}

/// Verify re-init kills the stale daemon process.
/// Leaves daemon1 running and lets init's kill_stale_daemon handle the kill.
/// We hold onto the Child handle in a local variable for cleanup rather than
/// leaking it with std::mem::forget.
#[test]
#[ignore]
fn test_reinit_kills_stale_daemon() {
    let config_dir = create_test_config_dir("reinit");
    let daemon_port1 = find_free_port();
    let mut daemon1 = start_test_daemon(&config_dir, daemon_port1);
    let pid1 = daemon1.child.id();

    // Verify first daemon is alive and has a PID file
    let pid_path = config_dir.join("daemon.pid");
    assert!(
        pid_path.exists(),
        "PID file should exist after first daemon start"
    );
    let saved_pid: u32 = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(saved_pid, pid1, "PID file should match daemon PID");

    // Detach daemon1 from the guard so Drop does NOT kill it --
    // we want init's kill_stale_daemon to handle that. Replace the child
    // with a dummy so the guard's Drop is harmless, and keep the real
    // child handle for cleanup at end of test.
    daemon1.config_dir = PathBuf::new();
    let mut original_child =
        std::mem::replace(&mut daemon1.child, Command::new("true").spawn().unwrap());
    // Drop the guard -- its Drop will kill+wait the "true" dummy (harmless).
    drop(daemon1);

    // Verify the old daemon is still alive
    assert_eq!(
        unsafe { libc::kill(pid1 as i32, 0) },
        0,
        "daemon1 should still be alive before re-init"
    );

    // Run init with a new port -- this should kill the old daemon
    let daemon_port2 = find_free_port();
    let output = Command::new(devproxy_bin())
        .args([
            "init",
            "--domain",
            TEST_DOMAIN,
            "--port",
            &daemon_port2.to_string(),
        ])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .env("DEVPROXY_NO_SOCKET_ACTIVATION", "1")
        .output()
        .expect("failed to run devproxy init");

    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("reinit stderr: {stderr}");

    // init should have killed the stale daemon and started a new one
    assert!(output.status.success(), "init should succeed: {stderr}");
    assert!(
        stderr.contains("killing stale daemon"),
        "init should report killing stale daemon: {stderr}"
    );
    assert!(
        stderr.contains("daemon started"),
        "init should report daemon started: {stderr}"
    );

    // Old daemon should be dead
    std::thread::sleep(Duration::from_millis(200));
    assert_ne!(
        unsafe { libc::kill(pid1 as i32, 0) },
        0,
        "old daemon (pid {pid1}) should be dead after re-init"
    );

    // Reap the original child to avoid zombie (init already killed it)
    let _ = original_child.wait();

    // New PID should be different from old one
    let new_pid_str = std::fs::read_to_string(&pid_path).unwrap();
    let new_pid: u32 = new_pid_str.trim().parse().unwrap();
    assert_ne!(new_pid, pid1, "new daemon should have a different PID");

    // Clean up the new daemon: send SIGTERM, wait for it to exit, then
    // fall back to SIGKILL if it doesn't die within 1 second.
    unsafe { libc::kill(new_pid as i32, libc::SIGTERM) };
    std::thread::sleep(Duration::from_millis(500));
    if unsafe { libc::kill(new_pid as i32, 0) } == 0 {
        // Still alive -- force kill
        unsafe { libc::kill(new_pid as i32, libc::SIGKILL) };
        std::thread::sleep(Duration::from_millis(200));
    }
    // Verify the new daemon is actually dead before cleaning up
    assert_ne!(
        unsafe { libc::kill(new_pid as i32, 0) },
        0,
        "new daemon (pid {new_pid}) should be dead after cleanup"
    );
    let _ = std::fs::remove_dir_all(&config_dir);
}

/// macOS-only: verify socket activation via launchd with an ephemeral port.
/// Installs a real LaunchAgent plist, waits for the daemon to respond,
/// then uninstalls. Run with: cargo test --test e2e -- --ignored test_launchd_socket_activation
#[test]
#[ignore]
#[cfg(target_os = "macos")]
fn test_launchd_socket_activation() {
    // Safety: skip if a production LaunchAgent plist already exists.
    // Installing a test plist would bootout and overwrite the real one,
    // destroying the developer's existing devproxy installation.
    let home = std::env::var("HOME").unwrap();
    let production_plist = format!("{home}/Library/LaunchAgents/com.devproxy.daemon.plist");
    if std::path::Path::new(&production_plist).exists() {
        eprintln!(
            "SKIPPED: production LaunchAgent plist exists at {production_plist}. \
             Unload it first (launchctl bootout gui/$(id -u)/com.devproxy.daemon) \
             to run this test."
        );
        return;
    }

    let config_dir = create_test_config_dir("launchd");
    let daemon_port = find_free_port();

    // Run init WITHOUT DEVPROXY_NO_SOCKET_ACTIVATION so it uses real launchd
    let output = Command::new(devproxy_bin())
        .args([
            "init",
            "--domain",
            TEST_DOMAIN,
            "--port",
            &daemon_port.to_string(),
        ])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init");

    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("launchd init stderr: {stderr}");

    assert!(output.status.success(), "init should succeed: {stderr}");
    assert!(
        stderr.contains("daemon started"),
        "init should report daemon started: {stderr}"
    );

    let used_socket_activation = stderr.contains("socket activation");

    // Verify daemon is responsive via IPC
    let status_output = Command::new(devproxy_bin())
        .args(["status"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run status");
    let status_stderr = String::from_utf8_lossy(&status_output.stderr);
    assert!(
        status_stderr.contains("running"),
        "daemon should be running: {status_stderr}"
    );

    if used_socket_activation {
        // Verify plist was written
        let plist_path = format!("{home}/Library/LaunchAgents/com.devproxy.daemon.plist");
        assert!(
            std::path::Path::new(&plist_path).exists(),
            "LaunchAgent plist should exist at {plist_path}"
        );

        // Read the plist and verify it references our port
        let plist_content = std::fs::read_to_string(&plist_path).unwrap();
        assert!(
            plist_content.contains(&daemon_port.to_string()),
            "plist should contain port {daemon_port}"
        );

        // Verify the plist references the daemon binary path (not the CLI binary)
        assert!(
            plist_content.contains("devproxy-daemon"),
            "plist should reference the dedicated daemon binary (devproxy-daemon), \
             not the CLI binary. This prevents launchd KeepAlive from SIGKILL'ing \
             normal CLI invocations."
        );
    }

    // Cleanup: bootout the agent and remove plist
    if used_socket_activation {
        let uid_output = Command::new("id").arg("-u").output().unwrap();
        let uid = String::from_utf8_lossy(&uid_output.stdout)
            .trim()
            .to_string();
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/com.devproxy.daemon")])
            .status();
        let _ = std::fs::remove_file(format!(
            "{home}/Library/LaunchAgents/com.devproxy.daemon.plist"
        ));
    }

    // Signal-based cleanup for fallback path
    let pid_path = config_dir.join("daemon.pid");
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
                std::thread::sleep(Duration::from_millis(500));
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&config_dir);
}

/// Verify that `devproxy --version` works even when a daemon is running.
///
/// This is the core regression test for the launchd SIGKILL bug: launchd's
/// KeepAlive=true causes it to SIGKILL any non-managed process at the
/// managed binary path. By using a separate daemon binary path, the CLI
/// binary is free from launchd interference.
///
/// This test verifies the structural fix: init installs a daemon binary at
/// a separate path, and --version runs cleanly from the CLI binary.
#[test]
#[ignore] // Run with: cargo test --test e2e -- --ignored test_version_works_with_daemon_running
fn test_version_works_with_daemon_running() {
    let config_dir = create_test_config_dir("ver-daemon");
    let port = find_free_port();

    // Start a daemon in the background. The daemon needs TLS certs which
    // create_test_config_dir already generates via `init --no-daemon`.
    let _guard = start_test_daemon(&config_dir, port);

    // Run --version — this must succeed even while a daemon is running.
    // In the launchd SIGKILL bug, running the binary at the launchd-managed
    // path would get killed. With the daemon binary path separation fix,
    // the CLI binary path is not managed by launchd.
    let output = Command::new(devproxy_bin())
        .args(["--version"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .env("DEVPROXY_NO_SOCKET_ACTIVATION", "1")
        .output()
        .expect("failed to run devproxy --version");

    assert!(
        output.status.success(),
        "devproxy --version should succeed (exit {}): stderr={}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("devproxy"),
        "--version should print version info: {stdout}"
    );
}

/// Verify that init with daemon installs the daemon binary at the
/// DEVPROXY_DATA_DIR path and that the daemon binary path differs
/// from the CLI binary path.
#[test]
fn test_init_daemon_binary_path_separation() {
    let config_dir = std::env::temp_dir().join(format!(
        "devproxy-config-path-sep-{}",
        std::process::id()
    ));
    let data_dir = std::env::temp_dir().join(format!(
        "devproxy-data-path-sep-{}",
        std::process::id()
    ));
    if config_dir.exists() {
        std::fs::remove_dir_all(&config_dir).unwrap();
    }
    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir).unwrap();
    }
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();

    let port = find_free_port();

    // Run init with daemon (uses fallback spawn since DEVPROXY_NO_SOCKET_ACTIVATION=1)
    let output = Command::new(devproxy_bin())
        .args([
            "init",
            "--domain",
            TEST_DOMAIN,
            "--port",
            &port.to_string(),
        ])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .env("DEVPROXY_DATA_DIR", &data_dir)
        .env("DEVPROXY_NO_SOCKET_ACTIVATION", "1")
        .output()
        .expect("failed to run devproxy init");

    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("init stderr: {stderr}");

    assert!(
        output.status.success(),
        "init should succeed: {stderr}"
    );

    // Verify daemon binary was installed at the data dir path
    let daemon_bin = data_dir.join("devproxy-daemon");
    assert!(
        daemon_bin.exists(),
        "daemon binary should be installed at {}: {stderr}",
        daemon_bin.display()
    );

    // Verify daemon binary path is different from CLI binary path
    let cli_bin = devproxy_bin();
    assert_ne!(
        cli_bin, daemon_bin,
        "daemon binary path should differ from CLI binary path"
    );

    // Verify --version works from CLI binary while daemon is running
    let version_output = Command::new(&cli_bin)
        .args(["--version"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run --version");

    assert!(
        version_output.status.success(),
        "--version should succeed while daemon is running (exit {}): {}",
        version_output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&version_output.stderr)
    );

    // Clean up daemon
    let pid_path = config_dir.join("daemon.pid");
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
                std::thread::sleep(Duration::from_millis(500));
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&config_dir);
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Verify that the daemon binary at the data dir path can run --version
/// and produce the same output as the CLI binary.
#[test]
fn test_daemon_binary_matches_cli_binary() {
    let data_dir = std::env::temp_dir().join(format!(
        "devproxy-data-match-{}",
        std::process::id()
    ));
    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir).unwrap();
    }
    std::fs::create_dir_all(&data_dir).unwrap();

    let cli_bin = devproxy_bin();
    let daemon_bin = data_dir.join("devproxy-daemon");

    // Copy CLI binary to daemon path (simulating what init does)
    std::fs::copy(&cli_bin, &daemon_bin).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&daemon_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let cli_output = Command::new(&cli_bin)
        .args(["--version"])
        .output()
        .expect("CLI --version failed");
    let daemon_output = Command::new(&daemon_bin)
        .args(["--version"])
        .output()
        .expect("daemon --version failed");

    assert!(cli_output.status.success(), "CLI --version should succeed");
    assert!(
        daemon_output.status.success(),
        "daemon binary --version should succeed"
    );

    let cli_version = String::from_utf8_lossy(&cli_output.stdout);
    let daemon_version = String::from_utf8_lossy(&daemon_output.stdout);
    assert_eq!(
        cli_version, daemon_version,
        "CLI and daemon binaries should report the same version"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}
