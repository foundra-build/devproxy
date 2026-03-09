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
fn copy_fixtures(test_name: &str) -> PathBuf {
    let dest = std::env::temp_dir().join(format!("devproxy-fixtures-{test_name}-{}", std::process::id()));
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
    dest
}

/// Create an isolated test config directory and generate certs using `init --no-daemon`.
/// Returns the path to the config directory (to be set as DEVPROXY_CONFIG_DIR).
fn create_test_config_dir(test_name: &str) -> PathBuf {
    let config_dir = std::env::temp_dir().join(format!("devproxy-config-{test_name}-{}", std::process::id()));
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
    assert!(config_dir.join("ca-cert.pem").exists(), "CA cert should exist after init");
    assert!(config_dir.join("tls-cert.pem").exists(), "TLS cert should exist after init");
    assert!(config_dir.join("config.json").exists(), "config should exist after init");

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
        if socket_path.exists()
            && std::os::unix::net::UnixStream::connect(&socket_path).is_ok()
        {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    panic!("daemon did not start within 5 seconds (socket: {})", socket_path.display());
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
    // Daemon should be hidden as a top-level command (it may appear in descriptions)
    // Check that "daemon" does not appear as a command entry (lines starting with "  daemon")
    assert!(
        !stdout.lines().any(|l| l.trim_start().starts_with("daemon ")),
        "daemon command should be hidden from help"
    );
}

#[test]
fn test_init_generates_certs() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-init-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init");

    assert!(output.status.success(), "init should succeed");
    assert!(config_dir.join("ca-cert.pem").exists(), "CA cert should exist");
    assert!(config_dir.join("ca-key.pem").exists(), "CA key should exist");
    assert!(config_dir.join("tls-cert.pem").exists(), "TLS cert should exist");
    assert!(config_dir.join("tls-key.pem").exists(), "TLS key should exist");
    assert!(config_dir.join("config.json").exists(), "config should exist");

    // Verify idempotency: running init again should succeed and not error
    let output2 = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init a second time");

    assert!(output2.status.success(), "init should be idempotent");
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(stderr2.contains("already exists"), "should report certs already exist");

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
fn test_up_without_label() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-nolabel-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    // Create a compose file without devproxy.port
    let test_dir = std::env::temp_dir().join(format!("devproxy-nolabel-project-{}", std::process::id()));
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
    let config_dir = std::env::temp_dir().join(format!("devproxy-nocompose-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    let test_dir = std::env::temp_dir().join(format!("devproxy-nocompose-project-{}", std::process::id()));
    std::fs::create_dir_all(&test_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&test_dir)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run up");

    assert!(!output.status.success(), "up should fail without compose file");
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

    let config_dir = std::env::temp_dir().join(format!("devproxy-noproject-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["down"])
        .current_dir(&test_dir)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run down");

    assert!(!output.status.success(), "down should fail without .devproxy-project");
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
    assert!(up_output.status.success(), "devproxy up should succeed: {up_stderr}");

    // Extract slug from output (look for "-> https://<slug>.test.devproxy.dev")
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| {
            l.split("https://")
                .nth(1)
                .and_then(|s| s.split('.').next())
        })
        .expect("should find slug in up output");

    // Verify .devproxy-project was written with the correct slug
    let project_file = fixtures.join(".devproxy-project");
    assert!(project_file.exists(), ".devproxy-project should exist after up");
    let saved_slug = std::fs::read_to_string(&project_file).unwrap();
    assert_eq!(saved_slug.trim(), slug, ".devproxy-project should contain the slug");

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

    // Ls check
    let ls_output = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy ls");
    let ls_stdout = String::from_utf8_lossy(&ls_output.stdout);
    assert!(
        ls_stdout.contains(slug),
        "ls should show our slug '{slug}': {ls_stdout}"
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
    assert!(down_output.status.success(), "devproxy down should succeed: {down_stderr}");

    // Verify cleanup files are gone
    assert!(!fixtures.join(".devproxy-project").exists(), ".devproxy-project should be removed after down");
    assert!(!fixtures.join(".devproxy-override.yml").exists(), ".devproxy-override.yml should be removed after down");
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
    assert!(ls_before_stdout.contains(slug), "route should exist before kill: {ls_before_stdout}");

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

    // Kill the daemon (not the container) -- DaemonGuard::drop kills process and cleans config_dir,
    // but we need the config_dir to survive for the new daemon. So we kill manually.
    let mut daemon = daemon;
    let _ = daemon.child.kill();
    let _ = daemon.child.wait();
    // Prevent DaemonGuard::drop from deleting config_dir by forgetting it
    std::mem::forget(daemon);

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
            "-o", "/dev/null",
            "-w", "%{http_code}",
            "--max-time", "5",
            "--resolve", &format!("{host}:{daemon_port}:127.0.0.1"),
            "--cacert", &ca_cert_path.to_string_lossy(),
            &url,
        ])
        .output()
        .expect("failed to run curl");

    let status_code = String::from_utf8_lossy(&curl_output.stdout);
    assert_eq!(status_code.trim(), "502", "should get 502 for unknown host, got: {status_code}");
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
    assert!(stderr.contains("running"), "should report daemon running: {stderr}");
    assert!(stderr.contains("active routes: 0"), "should report 0 routes: {stderr}");
}
