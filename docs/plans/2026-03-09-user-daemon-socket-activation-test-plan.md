# User-Owned Daemon via Socket Activation — Test Plan

## Strategy reconciliation

The agreed Heavy fidelity strategy assumed:
1. Unit tests for plist/systemd unit generation, LISTEN_FDS parsing, kill logic
2. macOS integration tests via real launchd with ephemeral ports
3. Docker-based Linux e2e tests for systemd, LISTEN_FDS, and setcap paths
4. Existing tests continue passing (regression)

After reviewing the implementation plan, the strategy holds with these minor adjustments (no cost/scope change):

- **Config dir propagation tests added**: The plan introduced `config_dir: Option<&str>` parameters on generation functions and `DEVPROXY_CONFIG_DIR` embedding in plist/unit files (design decision #14). Unit tests must cover this path since it is critical for test isolation — without it, launchd/systemd-launched daemons would write to the default config dir while tests check a different dir.
- **`restart_daemon` lifecycle test added**: The update flow now uses `restart_daemon()` (kickstart/systemctl restart) instead of stop+start. This is a new interaction surface not in the original strategy. Covered by the macOS launchd scenario test and Docker systemd test.
- **`is_daemon_platform_managed` test added**: New function in update.rs determines whether to use platform restart vs. kill_stale_daemon. Needs boundary testing.

## Harness requirements

### 1. Docker Linux test container

**What it does**: Provides a Debian bookworm container with systemd as PID 1, dbus, a non-root `testuser` with lingering enabled, and the devproxy binary built from source. Enables running `systemctl --user` and `setcap` in a controlled environment.

**What it exposes**: Shell scripts that exercise specific Linux paths and report PASS/FAIL. A runner script (`run-tests.sh`) that builds the container, starts it with `--privileged`, waits for systemd initialization, then runs each test script via `docker exec`.

**Estimated complexity**: Medium. The Dockerfile and runner script are specified in the implementation plan (Task 9). The main complexity is systemd user session setup (XDG_RUNTIME_DIR, dbus, lingering).

**Which tests depend on it**: Tests 3, 4, 5.

### 2. Existing e2e harness (tests/e2e.rs)

Already exists. `create_test_config_dir`, `start_test_daemon`, `find_free_port`, `DaemonGuard`, `ComposeGuard` helpers. The macOS launchd test and existing regression tests use this harness.

**Which tests depend on it**: Tests 2, 6, 7, 8, 9, 10.

---

## Test plan

### Test 1: Full init-to-HTTPS scenario via launchd socket activation (macOS)

- **Name**: Init installs LaunchAgent, daemon starts via socket activation, responds to IPC and HTTPS, re-init is idempotent, update restarts daemon
- **Type**: scenario
- **Harness**: tests/e2e.rs (`#[ignore]`, `#[cfg(target_os = "macos")]`)
- **Preconditions**: No production LaunchAgent plist at `~/Library/LaunchAgents/com.devproxy.daemon.plist`. devproxy binary built. No running devproxy daemon.
- **Actions**:
  1. Call `devproxy init --domain test.devproxy.dev --port <ephemeral>` with `DEVPROXY_CONFIG_DIR` set to a temp dir (no `DEVPROXY_NO_SOCKET_ACTIVATION`).
  2. Assert init succeeds and stderr contains "socket activation" and "daemon started".
  3. Run `devproxy status` — assert "running".
  4. Verify plist exists at `~/Library/LaunchAgents/com.devproxy.daemon.plist` and contains the ephemeral port.
  5. Run init again with same params — assert idempotent success (no crash from double-bootstrap).
  6. Cleanup: `launchctl bootout`, remove plist, remove config dir.
- **Expected outcome**: Per spec (implementation plan architecture section): "devproxy init installs a LaunchAgent plist with a Sockets entry for port 443; launchd owns the socket and passes fds via launch_activate_socket." The daemon runs as the current user and responds to IPC. Source of truth: implementation plan paragraph 1, design decision #8 (KeepAlive=true, launchd manages restarts).
- **Interactions**: launchd service manager (global state — the plist path and label are not scoped by DEVPROXY_CONFIG_DIR). Skip-guard prevents destroying existing installations.

### Test 2: Full init-to-HTTPS scenario via systemd socket activation (Linux/Docker)

- **Name**: Init installs systemd units, daemon starts via socket activation, responds to IPC
- **Type**: scenario
- **Harness**: Docker Linux test container (test-systemd.sh)
- **Preconditions**: Docker container with systemd as PID 1, testuser with lingering, XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS set.
- **Actions**:
  1. Set `DEVPROXY_CONFIG_DIR` to a temp dir.
  2. Run `devproxy init --domain test.dev --no-daemon` to generate certs.
  3. Run `devproxy init --domain test.dev --port 8443` to install systemd units and start daemon.
  4. Assert stderr contains "daemon started".
  5. Run `devproxy status` — assert "running".
  6. Verify `~/.config/systemd/user/devproxy.socket` exists and contains `ListenStream=127.0.0.1:8443`.
  7. Verify `~/.config/systemd/user/devproxy.service` exists and contains `daemon --port 8443`.
  8. Cleanup: stop and disable units, remove config dir.
- **Expected outcome**: Per spec: "On Linux, init installs systemd user socket+service units; systemd passes fds via the LISTEN_FDS protocol." Per design decision #4: socket binds to `127.0.0.1` only. Source of truth: implementation plan paragraph 1, design decision #4, #10.
- **Interactions**: systemd user session manager, dbus. Container isolation prevents host interference.

### Test 3: LISTEN_FDS protocol direct test (Linux/Docker)

- **Name**: Daemon accepts a pre-bound socket via LISTEN_FDS environment variables and serves on it
- **Type**: integration
- **Harness**: Docker Linux test container (test-listen-fds.sh)
- **Preconditions**: Docker container, devproxy binary available, certs generated.
- **Actions**:
  1. Use Python to bind a TCP socket on an ephemeral port, dup2 it to fd 3.
  2. Launch devproxy daemon via a shell wrapper that sets `LISTEN_PID=$$` and `LISTEN_FDS=1`.
  3. Wait for IPC socket to appear (up to 5 seconds).
  4. Run `devproxy status` — assert "running".
  5. Terminate daemon and verify clean exit.
- **Expected outcome**: Per spec (`src/proxy/socket_activation.rs` in plan): When LISTEN_PID matches and LISTEN_FDS=1, the daemon consumes fd 3 as its TCP listener. Per design decision #12: DEVPROXY_NO_SOCKET_ACTIVATION is NOT set, so acquire_listener() reads LISTEN_FDS. Source of truth: implementation plan Task 1 (acquire_systemd_fds), sd_listen_fds(3) protocol.
- **Interactions**: Python subprocess for fd passing, Unix fd inheritance across exec.

### Test 4: setcap fallback allows non-root binding of port 443 (Linux/Docker)

- **Name**: Binary with cap_net_bind_service binds port 443 as non-root user
- **Type**: integration
- **Harness**: Docker Linux test container (test-setcap.sh)
- **Preconditions**: Docker container with libcap2-bin. devproxy binary copied to a test path.
- **Actions**:
  1. Copy binary, `setcap cap_net_bind_service=+ep` on the copy.
  2. Verify `getcap` shows the capability.
  3. As testuser, start daemon on port 443 with `DEVPROXY_NO_SOCKET_ACTIVATION=1` (direct bind path).
  4. Assert daemon starts (status shows "running").
  5. Kill daemon, cleanup.
- **Expected outcome**: Per spec: "On Linux without systemd, setcap cap_net_bind_service=+ep is applied as a fallback so the binary can bind port 443 directly as a user." Source of truth: implementation plan paragraph 1, design decision #5.
- **Interactions**: Linux capabilities subsystem, setcap/getcap tools.

### Test 5: setcap NOT applied — non-root user cannot bind port 443 (Linux/Docker)

- **Name**: Without setcap, binding port 443 as non-root fails with a clear error
- **Type**: boundary
- **Harness**: Docker Linux test container (extension of test-setcap.sh)
- **Preconditions**: Docker container, devproxy binary WITHOUT setcap, testuser.
- **Actions**:
  1. As testuser, start daemon on port 443 with `DEVPROXY_NO_SOCKET_ACTIVATION=1`.
  2. Assert daemon fails to start (process exits non-zero or status shows not running).
- **Expected outcome**: The daemon cannot bind port 443 without root or capabilities. This is the negative case proving setcap is necessary. Source of truth: Linux kernel — unprivileged processes cannot bind ports < 1024 without CAP_NET_BIND_SERVICE.
- **Interactions**: Linux kernel privilege checks.

### Test 6: Existing test_reinit_kills_stale_daemon passes with DEVPROXY_NO_SOCKET_ACTIVATION

- **Name**: Re-init kills a directly-spawned stale daemon when socket activation is disabled
- **Type**: regression
- **Harness**: tests/e2e.rs (existing test, modified)
- **Preconditions**: `DEVPROXY_NO_SOCKET_ACTIVATION=1` set on the init Command. First daemon started via `start_test_daemon` (direct spawn).
- **Actions**: Existing test actions unchanged — start daemon1, run init with new port, verify daemon1 killed and new daemon started.
- **Expected outcome**: Same as before: init reports "killing stale daemon" and "daemon started". The env var causes `install_daemon()` to bail (triggering `spawn_daemon_directly` fallback) and `stop_daemon()` to no-op. Source of truth: implementation plan Task 7, design decision #3.
- **Interactions**: PID-based process management (kill/SIGTERM/SIGKILL). No launchd/systemd interaction due to env var.

### Test 7: devproxy update restarts platform-managed daemon via kickstart/systemctl restart

- **Name**: After self-update, platform-managed daemon is restarted without bootout
- **Type**: scenario
- **Harness**: tests/e2e.rs (macOS, `#[ignore]`), Docker container (Linux)
- **Preconditions**: Platform-managed daemon running (launchd on macOS, systemd on Linux). Binary at known path.
- **Actions** (macOS variant, conceptual — actual test depends on whether a real update binary is available):
  1. Init with socket activation to start daemon via launchd.
  2. Simulate update by replacing binary on disk (copy current binary over itself or use a temp binary).
  3. Call `platform::restart_daemon()` (or trigger via update flow).
  4. Verify daemon is still running after restart (status shows "running").
  5. Cleanup.
- **Expected outcome**: Per design decision #1 and #11: "do_update must NOT call stop_daemon() (bootout) before restart_daemon() (kickstart). Instead, update replaces the binary then calls restart_daemon() directly." kickstart -k atomically kills and restarts. Source of truth: design decisions #1, #8, #11.
- **Interactions**: launchd kickstart / systemctl restart. Binary replacement while process is running (safe on Unix — running process holds inode reference).

### Test 8: DEVPROXY_NO_SOCKET_ACTIVATION skips all platform operations and fd acquisition

- **Name**: Environment variable completely isolates tests from platform service managers
- **Type**: invariant
- **Harness**: Rust unit tests in `src/platform.rs` and `src/proxy/socket_activation.rs`
- **Preconditions**: `DEVPROXY_NO_SOCKET_ACTIVATION=1` set.
- **Actions**:
  1. Call `acquire_listener()` — assert returns `Ok(None)`.
  2. Call `is_socket_activation_disabled()` — assert returns true.
  3. Call `stop_daemon()` — assert returns `Ok(())` (no-op).
  4. Call `restart_daemon()` — assert returns `Ok(false)`.
  5. Call `uninstall_daemon()` — assert returns `Ok(())`.
  6. Call `install_daemon()` — assert returns `Err` (bail).
- **Expected outcome**: Per design decision #3: "install_daemon returns Err (triggers fallback). stop_daemon, restart_daemon (returns Ok(false)), and uninstall_daemon return Ok (no-op). acquire_listener returns Ok(None)." Source of truth: design decision #3, #12.
- **Interactions**: None — that is the point.

### Test 9: Plist XML generation contains all required fields

- **Name**: Generated LaunchAgent plist has label, binary path, Sockets entry, port, localhost binding, and optional EnvironmentVariables
- **Type**: unit
- **Harness**: `#[cfg(test)]` in `src/platform.rs`
- **Preconditions**: None.
- **Actions**:
  1. `generate_launchagent_plist("/usr/local/bin/devproxy", 443, None)` — assert contains: `com.devproxy.daemon`, binary path, `<key>Sockets</key>`, `443`, `Listeners`, `127.0.0.1`, does NOT contain `EnvironmentVariables`.
  2. `generate_launchagent_plist("/usr/local/bin/devproxy", 443, Some("/tmp/test-config"))` — assert contains `EnvironmentVariables`, `DEVPROXY_CONFIG_DIR`, `/tmp/test-config`.
  3. `generate_launchagent_plist("/opt/devproxy", 8443, None)` — assert contains `8443` and `/opt/devproxy`.
- **Expected outcome**: Plist matches the structure required by launchd for socket activation. Per design decision #7: socket name is "Listeners". Per design decision #14: config_dir propagation. Source of truth: implementation plan Task 3 step 3, design decisions #7, #14.
- **Interactions**: None (pure function).

### Test 10: Systemd unit file generation contains all required fields

- **Name**: Generated .socket and .service units have correct sections, localhost binding, port, binary path, and optional Environment
- **Type**: unit
- **Harness**: `#[cfg(test)]` in `src/platform.rs`
- **Preconditions**: None.
- **Actions**:
  1. `generate_systemd_socket_unit(443)` — assert `ListenStream=127.0.0.1:443` and `[Socket]` section.
  2. `generate_systemd_socket_unit(8443)` — assert `ListenStream=127.0.0.1:8443`.
  3. `generate_systemd_service_unit("/usr/local/bin/devproxy", 443, None)` — assert binary path, `daemon --port 443`, `Type=simple`, `Requires=devproxy.socket`, no `Environment=`.
  4. `generate_systemd_service_unit("/usr/local/bin/devproxy", 8443, None)` — assert `daemon --port 8443`.
  5. `generate_systemd_service_unit("/usr/local/bin/devproxy", 443, Some("/tmp/test-config"))` — assert `Environment=DEVPROXY_CONFIG_DIR=/tmp/test-config`.
- **Expected outcome**: Per design decision #4: localhost-only. Per design decision #10: --port in ExecStart. Per design decision #14: config_dir propagation. Source of truth: implementation plan Task 3 step 3, design decisions #4, #10, #14.
- **Interactions**: None (pure function).

### Test 11: acquire_activated_fds returns None when not launched by launchd/systemd

- **Name**: Socket activation fd acquisition returns None in normal (non-activated) process
- **Type**: unit
- **Harness**: `#[cfg(test)]` in `src/proxy/socket_activation.rs`
- **Preconditions**: Process not launched by launchd or systemd (normal test run).
- **Actions**:
  1. Call `acquire_activated_fds()` — assert `Ok(None)`.
  2. Call `acquire_listener().await` — assert `Ok(None)`.
- **Expected outcome**: Per spec: "Returns Ok(None) if socket activation is not active (caller should fall back to TcpListener::bind())." On macOS, `launch_activate_socket` returns ESRCH. On Linux, LISTEN_PID is unset. Source of truth: implementation plan Task 1 step 3.
- **Interactions**: launchd API (returns ESRCH) or env var check (LISTEN_PID absent).

### Test 12: All existing non-Docker e2e tests pass unchanged

- **Name**: Existing tests continue passing with socket activation changes
- **Type**: regression
- **Harness**: `cargo test --test e2e`
- **Preconditions**: Binary built with all changes.
- **Actions**: Run `cargo test --test e2e` (non-ignored tests only).
- **Expected outcome**: All pass. Specifically: `test_init_generates_certs` (uses --no-daemon, unaffected), `test_init_output_includes_sudo_note` (sudo appears in CA trust output, unchanged), `test_cli_help`, `test_cli_version`, `test_status_without_daemon`, `test_up_without_label`, `test_up_without_compose_file`, `test_down_without_project_file`, `test_up_fails_fast_with_dead_daemon`, `test_init_output_includes_dns_instructions`, `test_init_output_includes_ca_trust_path`. Source of truth: existing test suite expectations.
- **Interactions**: These tests do not spawn daemons (except via `--no-daemon`), so socket activation code is never triggered.

### Test 13: All existing Docker-dependent e2e tests pass unchanged

- **Name**: Full e2e workflow, self-healing, daemon restart, 502, IPC, PID file tests still pass
- **Type**: regression
- **Harness**: `cargo test --test e2e -- --ignored`
- **Preconditions**: Docker available. Binary built. `start_test_daemon` spawns daemon directly (no socket activation — tests don't set up launchd/systemd).
- **Actions**: Run all ignored e2e tests. The daemon falls through to `TcpListener::bind` fallback since `launch_activate_socket` returns ESRCH (macOS) or LISTEN_PID is unset (Linux).
- **Expected outcome**: All pass. The socket activation acquire path returns None, fallback to bind works as before. Source of truth: existing test expectations + implementation plan Task 2 (fallback path preserved).
- **Interactions**: Docker, daemon process lifecycle. No launchd/systemd interaction.

### Test 14: cargo clippy and cargo fmt produce clean output

- **Name**: No clippy warnings or formatting issues in new code
- **Type**: invariant
- **Harness**: `cargo clippy -- -D warnings` and `cargo fmt --check`
- **Preconditions**: All source changes applied.
- **Actions**: Run clippy and fmt check.
- **Expected outcome**: Zero warnings, zero formatting differences. Source of truth: project CLAUDE.md ("cargo clippy + cargo test" in `just check`).
- **Interactions**: None.

---

## Coverage summary

### Covered

| Area | Tests |
|------|-------|
| macOS launchd socket activation (init → daemon → IPC) | 1 |
| Linux systemd socket activation (init → daemon → IPC) | 2 |
| Linux LISTEN_FDS protocol (direct fd passing) | 3 |
| Linux setcap fallback (bind port 443 as user) | 4, 5 |
| Test isolation via DEVPROXY_NO_SOCKET_ACTIVATION | 6, 8 |
| Update flow with platform restart (kickstart/systemctl restart) | 7 |
| Plist XML content correctness | 9 |
| Systemd unit file content correctness | 10 |
| fd acquisition fallback (non-activated process) | 11 |
| Existing test regression safety | 6, 12, 13 |
| Code quality (clippy, fmt) | 14 |
| Config dir propagation in plist/unit files | 9, 10 |

### Explicitly excluded (per agreed strategy)

| Area | Reason | Risk |
|------|--------|------|
| Actual port 443 binding via launchd on macOS | Requires modifying system-level networking on the dev machine; integration test uses ephemeral port instead | Low — the port number is a launchd config parameter, not a code path difference. Ephemeral port exercises the same `launch_activate_socket` → fd → TcpListener code path. |
| Real HTTPS traffic through socket-activated daemon | Would require DNS setup and CA trust within the test; curl-through-proxy is already covered by existing e2e tests using the bind fallback path | Low — the HTTPS proxy loop is unchanged; only the listener acquisition differs. |
| Root → user daemon transition migration path | Users with existing root-owned daemons need manual cleanup before adopting socket activation | Medium — documentation should cover this. No automated migration is in scope per the plan. |
| `devproxy update` with an actual GitHub release download | Requires a live GitHub release; the update flow is already tested in the prior phase | Low — the update command's download/replace/validate logic is unchanged; only the daemon restart path is new (covered by test 7). |
| Windows/unsupported platforms | Code returns `Ok(None)` / `bail!` on unsupported platforms | None — devproxy explicitly targets macOS and Linux. |
