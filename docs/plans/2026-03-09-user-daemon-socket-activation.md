# User-Owned Daemon via Socket Activation — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Eliminate the need for `sudo` when starting the devproxy daemon by using OS-level socket activation (launchd on macOS, systemd on Linux) to bind port 443, then pass the pre-bound fd to the daemon process running as the current user.

**Architecture:** The daemon gains a new code path to receive pre-bound TCP listeners from launchd/systemd instead of calling `TcpListener::bind()` directly. On macOS, `devproxy init` installs a LaunchAgent plist with a Sockets entry for port 443; launchd owns the socket and passes fds via `launch_activate_socket`. On Linux, init installs systemd user socket+service units; systemd passes fds via the `LISTEN_FDS` protocol. On Linux without systemd, `setcap cap_net_bind_service=+ep` is applied as a fallback so the binary can bind port 443 directly as a user. The existing `TcpListener::bind()` path remains as the final fallback for tests and `--no-daemon` scenarios.

The `platform` module separates "stop" from "uninstall": `stop_daemon()` uses `launchctl bootout`/`systemctl --user stop` to halt the process without removing plist/unit files, while `uninstall_daemon()` both stops and removes the files. `kill_stale_daemon` and `devproxy update` use `stop_daemon()`. Only `devproxy init` (which re-installs) uses the full `uninstall_daemon()` before re-creating files.

Both `stop_daemon()` and `install_daemon()` are gated on whether the platform-managed files actually exist (plist on macOS, unit files on Linux). This prevents cross-environment interference: `stop_daemon()` called in a test using `DEVPROXY_CONFIG_DIR` won't touch a real LaunchAgent unless the plist file is present. Additionally, `DEVPROXY_NO_SOCKET_ACTIVATION=1` skips all platform operations (install, stop, uninstall) for complete test isolation.

**Tech Stack:** Rust, tokio, libc (for `launch_activate_socket` FFI on macOS), std::env (for `LISTEN_FDS` on Linux), plist XML generation, systemd unit file generation, Docker (for Linux e2e tests from macOS).

---

### Task 1: Add `socket_activation` module with platform-specific fd acquisition

**Files:**
- Create: `src/proxy/socket_activation.rs`
- Modify: `src/proxy/mod.rs:1` (add `pub mod socket_activation;`)

**Step 1: Write the failing test**

```rust
// In src/proxy/socket_activation.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_activated_fds_returns_none_when_not_activated() {
        let result = acquire_activated_fds();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --lib proxy::socket_activation::tests -v`
Expected: FAIL — module does not exist yet

**Step 3: Write the implementation**

```rust
// src/proxy/socket_activation.rs

//! Platform-specific socket activation support.
//!
//! On macOS: uses `launch_activate_socket` to receive fds from launchd.
//! On Linux: reads `LISTEN_FDS`/`LISTEN_PID` env vars per sd_listen_fds(3).
//! Returns `None` when no activated sockets are available (fallback to bind).

use anyhow::{Context, Result, bail};
use std::os::unix::io::{FromRawFd, RawFd};
use tokio::net::TcpListener;

/// Attempt to acquire a pre-bound TCP listener from the OS socket activation
/// mechanism. Returns `Ok(None)` if socket activation is not active (caller
/// should fall back to `TcpListener::bind()`).
pub async fn acquire_listener() -> Result<Option<TcpListener>> {
    match acquire_activated_fds()? {
        Some(fds) if !fds.is_empty() => {
            let fd = fds[0];
            // Safety: the fd is passed to us by launchd/systemd and is a valid
            // bound TCP socket. We take ownership (no dup needed — the OS
            // expects us to consume it).
            let std_listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };
            std_listener
                .set_nonblocking(true)
                .context("failed to set activated socket to non-blocking")?;
            let listener = TcpListener::from_std(std_listener)
                .context("failed to convert activated socket to tokio TcpListener")?;
            Ok(Some(listener))
        }
        _ => Ok(None),
    }
}

/// Low-level: get raw fds from the platform's socket activation mechanism.
/// Returns Ok(None) if socket activation is not in use.
fn acquire_activated_fds() -> Result<Option<Vec<RawFd>>> {
    #[cfg(target_os = "macos")]
    {
        acquire_launchd_fds()
    }
    #[cfg(target_os = "linux")]
    {
        acquire_systemd_fds()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
fn acquire_launchd_fds() -> Result<Option<Vec<RawFd>>> {
    use std::ffi::CString;
    use std::os::raw::c_int;

    // launch_activate_socket is declared in <launch.h>:
    //   int launch_activate_socket(const char *name, int **fds, size_t *cnt);
    // Returns 0 on success, non-zero errno on failure.
    // ESRCH (3) means "not managed by launchd" — this is the normal fallback case.
    extern "C" {
        fn launch_activate_socket(
            name: *const std::os::raw::c_char,
            fds: *mut *mut c_int,
            cnt: *mut usize,
        ) -> c_int;
    }

    let name = CString::new("Listeners")
        .context("CString::new failed for socket name")?;
    let mut fds_ptr: *mut c_int = std::ptr::null_mut();
    let mut count: usize = 0;

    let ret = unsafe { launch_activate_socket(name.as_ptr(), &mut fds_ptr, &mut count) };

    if ret != 0 {
        let err = std::io::Error::from_raw_os_error(ret);
        // ESRCH means not launched by launchd — normal fallback
        if ret == libc::ESRCH {
            return Ok(None);
        }
        bail!("launch_activate_socket failed: {err}");
    }

    if fds_ptr.is_null() || count == 0 {
        return Ok(None);
    }

    let fds: Vec<RawFd> = unsafe {
        let slice = std::slice::from_raw_parts(fds_ptr, count);
        let v = slice.to_vec();
        // The fds array was malloc'd by launch_activate_socket; we must free it.
        libc::free(fds_ptr as *mut libc::c_void);
        v
    };

    Ok(Some(fds))
}

#[cfg(target_os = "linux")]
fn acquire_systemd_fds() -> Result<Option<Vec<RawFd>>> {
    // sd_listen_fds(3) protocol:
    // - LISTEN_PID must match our PID
    // - LISTEN_FDS is the count of fds starting at fd 3

    let listen_pid = match std::env::var("LISTEN_PID") {
        Ok(val) => val,
        Err(_) => return Ok(None),
    };

    let our_pid = std::process::id().to_string();
    if listen_pid != our_pid {
        return Ok(None);
    }

    let listen_fds: usize = std::env::var("LISTEN_FDS")
        .context("LISTEN_PID set but LISTEN_FDS missing")?
        .parse()
        .context("LISTEN_FDS is not a valid number")?;

    if listen_fds == 0 {
        return Ok(None);
    }

    // Unset the env vars so child processes don't inherit them
    // (matches sd_listen_fds(1) behavior)
    std::env::remove_var("LISTEN_PID");
    std::env::remove_var("LISTEN_FDS");

    const SD_LISTEN_FDS_START: RawFd = 3;
    let fds: Vec<RawFd> = (SD_LISTEN_FDS_START..SD_LISTEN_FDS_START + listen_fds as RawFd)
        .collect();

    // Set CLOEXEC on all fds so they don't leak to child processes
    for &fd in &fds {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }

    Ok(Some(fds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_activated_fds_returns_none_when_not_activated() {
        let result = acquire_activated_fds();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_acquire_listener_returns_none_when_not_activated() {
        let result = acquire_listener().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
```

**Step 4: Register the module in `src/proxy/mod.rs`**

Add `pub mod socket_activation;` after line 3 (`pub mod router;`).

**Step 5: Run test to verify it passes**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --lib proxy::socket_activation::tests -v`
Expected: PASS

**Step 6: Commit**

```bash
git add src/proxy/socket_activation.rs src/proxy/mod.rs
git commit -m "feat: add socket_activation module for launchd/systemd fd acquisition"
```

---

### Task 2: Modify `run_daemon` to use activated listener with fallback

**Files:**
- Modify: `src/proxy/mod.rs:103-107` (the `TcpListener::bind` block)

**Step 1: Modify `run_daemon` to try socket activation first**

In `src/proxy/mod.rs`, replace lines 103-107:

```rust
    // Set up HTTPS listener
    let tcp_listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .with_context(|| format!("could not bind to port {port}"))?;
    eprintln!("HTTPS proxy listening on 127.0.0.1:{port}");
```

With:

```rust
    // Set up HTTPS listener: prefer socket activation (launchd/systemd),
    // fall back to direct bind for tests and manual runs.
    let tcp_listener = match socket_activation::acquire_listener().await? {
        Some(listener) => {
            let addr = listener
                .local_addr()
                .context("could not determine activated socket address")?;
            eprintln!("HTTPS proxy listening on {addr} (socket activation)");
            listener
        }
        None => {
            let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
                .await
                .with_context(|| format!("could not bind to port {port}"))?;
            eprintln!("HTTPS proxy listening on 127.0.0.1:{port}");
            listener
        }
    };
```

**Step 2: Run existing tests to verify fallback path still works**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: all tests PASS (socket activation returns None, fallback to bind works)

**Step 3: Commit**

```bash
git add src/proxy/mod.rs
git commit -m "feat: use socket activation in run_daemon with bind fallback"
```

---

### Task 3: Add `platform` module with stop/uninstall separation, setcap fallback, and file-existence guards

This is the core platform module. Critical design points:

1. **stop vs uninstall**: `stop_daemon()` halts the process without deleting unit/plist files. `uninstall_daemon()` stops AND removes files.
2. **File-existence guard**: `stop_daemon()` checks whether the plist/unit file actually exists before calling `launchctl bootout` / `systemctl stop`. This prevents a test using `DEVPROXY_CONFIG_DIR` from accidentally booting out a real production LaunchAgent.
3. **`DEVPROXY_NO_SOCKET_ACTIVATION` guard**: All public functions (`install_daemon`, `stop_daemon`, `uninstall_daemon`) early-return `Ok(())` when this env var is set, giving tests complete isolation.
4. **Localhost-only binding**: The systemd socket unit uses `ListenStream=127.0.0.1:{port}` (not bare `{port}`) to match the macOS plist's `SockNodeName=127.0.0.1` and the existing `TcpListener::bind("127.0.0.1:...")`.

**Files:**
- Create: `src/platform.rs`
- Modify: `src/main.rs:1` (add `mod platform;`)

**Step 1: Write failing tests for generation functions**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launchagent_plist_contains_required_fields() {
        let plist = generate_launchagent_plist("/usr/local/bin/devproxy", 443);
        assert!(plist.contains("com.devproxy.daemon"), "should have label");
        assert!(plist.contains("/usr/local/bin/devproxy"), "should have binary path");
        assert!(plist.contains("<key>Sockets</key>"), "should have Sockets");
        assert!(plist.contains("443"), "should have port 443");
        assert!(plist.contains("Listeners"), "should have socket name matching code");
        assert!(plist.contains("127.0.0.1"), "should bind to localhost only");
    }

    #[test]
    fn test_systemd_socket_unit_binds_localhost() {
        let unit = generate_systemd_socket_unit(443);
        assert!(unit.contains("ListenStream=127.0.0.1:443"), "should listen on localhost:443");
        assert!(unit.contains("[Socket]"), "should have Socket section");
    }

    #[test]
    fn test_systemd_socket_unit_custom_port() {
        let unit = generate_systemd_socket_unit(8443);
        assert!(unit.contains("ListenStream=127.0.0.1:8443"), "should use custom port on localhost");
    }

    #[test]
    fn test_systemd_service_unit_contains_binary() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy");
        assert!(unit.contains("/usr/local/bin/devproxy"), "should have binary path");
        assert!(unit.contains("daemon"), "should run daemon subcommand");
        assert!(unit.contains("Type=simple"), "should have Type=simple");
    }

    #[test]
    fn test_systemd_service_references_socket() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy");
        assert!(unit.contains("Requires=devproxy.socket"), "should require socket unit");
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --lib platform::tests -v`
Expected: FAIL — module does not exist

**Step 3: Write the implementation**

```rust
// src/platform.rs

//! Platform-specific daemon installation and management.
//!
//! macOS: LaunchAgent plist with socket activation (like puma-dev).
//! Linux: systemd user socket + service units, with setcap fallback.
//!
//! Key distinction: `stop_daemon()` halts the running daemon without removing
//! unit/plist files (used by update and kill_stale_daemon). `uninstall_daemon()`
//! stops AND removes files (used by init before re-installing).
//!
//! All public functions respect `DEVPROXY_NO_SOCKET_ACTIVATION` for test isolation
//! and check for file existence before touching global system state.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The launchd label for the devproxy daemon.
pub const LAUNCHD_LABEL: &str = "com.devproxy.daemon";

/// The systemd unit name prefix.
const SYSTEMD_UNIT_NAME: &str = "devproxy";

/// Returns true if socket activation is disabled via env var.
/// Used for test isolation: prevents tests from touching real
/// LaunchAgents/systemd units on the host.
pub fn is_socket_activation_disabled() -> bool {
    std::env::var("DEVPROXY_NO_SOCKET_ACTIVATION").is_ok()
}

// ---- Plist / unit file generation ------------------------------------------

/// Generate the LaunchAgent plist XML for macOS.
/// The plist uses Sockets to have launchd bind the port and pass the fd.
pub fn generate_launchagent_plist(binary_path: &str, port: u16) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary_path}</string>
        <string>daemon</string>
        <string>--port</string>
        <string>{port}</string>
    </array>
    <key>Sockets</key>
    <dict>
        <key>Listeners</key>
        <dict>
            <key>SockNodeName</key>
            <string>127.0.0.1</string>
            <key>SockServiceName</key>
            <string>{port}</string>
            <key>SockType</key>
            <string>stream</string>
        </dict>
    </dict>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>/tmp/devproxy-daemon.log</string>
    <key>StandardOutPath</key>
    <string>/dev/null</string>
</dict>
</plist>
"#
    )
}

/// Generate a systemd .socket unit for Linux.
/// Binds to 127.0.0.1 only — never expose the dev proxy to the network.
pub fn generate_systemd_socket_unit(port: u16) -> String {
    format!(
        "[Unit]\n\
         Description=devproxy HTTPS socket\n\
         \n\
         [Socket]\n\
         ListenStream=127.0.0.1:{port}\n\
         \n\
         [Install]\n\
         WantedBy=sockets.target\n"
    )
}

/// Generate a systemd .service unit for Linux.
pub fn generate_systemd_service_unit(binary_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=devproxy HTTPS reverse proxy daemon\n\
         Requires={SYSTEMD_UNIT_NAME}.socket\n\
         After={SYSTEMD_UNIT_NAME}.socket\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={binary_path} daemon\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

// ---- Path helpers ----------------------------------------------------------

/// Path to the LaunchAgent plist file.
#[cfg(target_os = "macos")]
pub fn launchagent_plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist")))
}

/// Path to the systemd user unit directory.
#[cfg(target_os = "linux")]
pub fn systemd_user_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".config/systemd/user"))
}

// ---- Stop (preserves files) ------------------------------------------------

/// Stop the daemon process without removing plist/unit files.
/// Used by `devproxy update` and `kill_stale_daemon`.
///
/// Respects `DEVPROXY_NO_SOCKET_ACTIVATION` for test isolation.
/// Only acts if the platform management files (plist/unit) actually exist,
/// preventing cross-environment interference (e.g., a test booting out a
/// real LaunchAgent).
///
/// macOS: `launchctl bootout` (stops the process; plist remains on disk).
/// Linux: `systemctl --user stop` the socket and service.
pub fn stop_daemon() -> Result<()> {
    if is_socket_activation_disabled() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        // Only bootout if we know we installed a plist
        let plist_path = launchagent_plist_path()?;
        if plist_path.exists() {
            bootout_launchagent()?;
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Only stop if unit files exist
        let unit_dir = systemd_user_dir()?;
        if unit_dir.join(format!("{SYSTEMD_UNIT_NAME}.socket")).exists() {
            stop_systemd_units()?;
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // No-op on unsupported platforms (don't bail — caller handles fallback)
    }

    Ok(())
}

// ---- Install (writes files + starts) ---------------------------------------

/// Install the daemon for the current platform. Returns Ok(()) on success.
///
/// Respects `DEVPROXY_NO_SOCKET_ACTIVATION` — returns Err so caller
/// falls through to `spawn_daemon_directly`.
///
/// macOS: writes plist and runs `launchctl bootstrap`.
/// Linux: writes systemd units and runs `systemctl --user enable --now`.
///        Falls back to `setcap` if systemd is not available.
pub fn install_daemon(binary_path: &Path, port: u16) -> Result<()> {
    if is_socket_activation_disabled() {
        bail!("socket activation disabled via DEVPROXY_NO_SOCKET_ACTIVATION");
    }

    let binary_str = binary_path
        .to_str()
        .context("binary path is not valid UTF-8")?;

    #[cfg(target_os = "macos")]
    {
        install_launchagent(binary_str, port)?;
    }
    #[cfg(target_os = "linux")]
    {
        install_linux_daemon(binary_str, binary_path, port)?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (binary_str, port);
        bail!("daemon installation is not supported on this platform");
    }

    Ok(())
}

/// Uninstall: stop AND remove plist/unit files.
/// Used by `devproxy init` before re-installing.
///
/// Respects `DEVPROXY_NO_SOCKET_ACTIVATION` for test isolation.
pub fn uninstall_daemon() -> Result<()> {
    if is_socket_activation_disabled() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        uninstall_launchagent()?;
    }
    #[cfg(target_os = "linux")]
    {
        uninstall_systemd_units()?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // No-op on unsupported platforms
    }

    Ok(())
}

// ---- macOS launchd ---------------------------------------------------------

#[cfg(target_os = "macos")]
fn bootout_launchagent() -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let status = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
        .status()
        .context("failed to run launchctl bootout")?;

    if !status.success() {
        // Not fatal — agent may not be loaded
        eprintln!(
            "  launchctl bootout returned {} (agent may not be loaded)",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_launchagent(binary_path: &str, port: u16) -> Result<()> {
    use colored::Colorize;

    let plist_path = launchagent_plist_path()?;
    let plist_dir = plist_path
        .parent()
        .context("plist path has no parent")?;

    std::fs::create_dir_all(plist_dir)
        .with_context(|| format!("could not create {}", plist_dir.display()))?;

    // If already installed, bootout first
    if plist_path.exists() {
        eprintln!("{} removing existing LaunchAgent...", "info:".cyan());
        let _ = bootout_launchagent();
    }

    let plist_content = generate_launchagent_plist(binary_path, port);
    std::fs::write(&plist_path, &plist_content)
        .with_context(|| format!("could not write plist at {}", plist_path.display()))?;
    eprintln!("{} wrote {}", "ok:".green(), plist_path.display());

    // Bootstrap the agent (loads and starts it)
    let uid = unsafe { libc::getuid() };
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{uid}"), &plist_path.to_string_lossy()])
        .status()
        .context("failed to run launchctl bootstrap")?;

    if !status.success() {
        bail!(
            "launchctl bootstrap failed (exit {}). Check: launchctl print gui/{uid}/{LAUNCHD_LABEL}",
            status.code().unwrap_or(-1)
        );
    }

    eprintln!("{} LaunchAgent installed and started", "ok:".green());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchagent() -> Result<()> {
    use colored::Colorize;

    let plist_path = launchagent_plist_path()?;
    if plist_path.exists() {
        let _ = bootout_launchagent();
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("could not remove {}", plist_path.display()))?;
        eprintln!("{} removed {}", "ok:".green(), plist_path.display());
    }

    Ok(())
}

// ---- Linux: systemd preferred, setcap fallback -----------------------------

#[cfg(target_os = "linux")]
fn install_linux_daemon(binary_str: &str, binary_path: &Path, port: u16) -> Result<()> {
    // Try systemd first
    match install_systemd_units(binary_str, port) {
        Ok(()) => return Ok(()),
        Err(e) => {
            use colored::Colorize;
            eprintln!(
                "{} systemd setup failed: {e}",
                "info:".cyan()
            );
            eprintln!(
                "{} trying setcap fallback...",
                "info:".cyan()
            );
        }
    }

    // Fallback: setcap
    apply_setcap(binary_path)?;
    Ok(())
}

/// Apply `setcap cap_net_bind_service=+ep` to the binary so it can bind
/// privileged ports as a regular user. Requires sudo.
#[cfg(target_os = "linux")]
fn apply_setcap(binary_path: &Path) -> Result<()> {
    use colored::Colorize;

    eprintln!(
        "{} applying cap_net_bind_service to {} (requires sudo)...",
        "info:".cyan(),
        binary_path.display()
    );

    let status = std::process::Command::new("sudo")
        .args([
            "setcap",
            "cap_net_bind_service=+ep",
            &binary_path.to_string_lossy(),
        ])
        .status()
        .context("failed to run sudo setcap")?;

    if !status.success() {
        bail!(
            "setcap failed (exit {}). You can run manually:\n  \
             sudo setcap cap_net_bind_service=+ep {}",
            status.code().unwrap_or(-1),
            binary_path.display()
        );
    }

    eprintln!("{} cap_net_bind_service applied", "ok:".green());
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_systemd_units(binary_path: &str, port: u16) -> Result<()> {
    use colored::Colorize;

    // Check if systemctl is available before writing files
    let systemctl_check = std::process::Command::new("systemctl")
        .args(["--user", "--version"])
        .output();
    match systemctl_check {
        Ok(output) if output.status.success() => {}
        _ => bail!("systemctl --user not available"),
    }

    let unit_dir = systemd_user_dir()?;
    std::fs::create_dir_all(&unit_dir)
        .with_context(|| format!("could not create {}", unit_dir.display()))?;

    let socket_path = unit_dir.join(format!("{SYSTEMD_UNIT_NAME}.socket"));
    let service_path = unit_dir.join(format!("{SYSTEMD_UNIT_NAME}.service"));

    std::fs::write(&socket_path, generate_systemd_socket_unit(port))
        .with_context(|| format!("could not write {}", socket_path.display()))?;
    eprintln!("{} wrote {}", "ok:".green(), socket_path.display());

    std::fs::write(&service_path, generate_systemd_service_unit(binary_path))
        .with_context(|| format!("could not write {}", service_path.display()))?;
    eprintln!("{} wrote {}", "ok:".green(), service_path.display());

    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .context("failed to run systemctl --user daemon-reload")?;

    if !reload.success() {
        bail!("systemctl --user daemon-reload failed");
    }

    let enable = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", &format!("{SYSTEMD_UNIT_NAME}.socket")])
        .status()
        .context("failed to run systemctl --user enable")?;

    if !enable.success() {
        bail!("systemctl --user enable --now {SYSTEMD_UNIT_NAME}.socket failed");
    }

    eprintln!("{} systemd socket unit installed and enabled", "ok:".green());
    Ok(())
}

#[cfg(target_os = "linux")]
fn stop_systemd_units() -> Result<()> {
    // Stop without disabling or removing files
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", &format!("{SYSTEMD_UNIT_NAME}.service")])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", &format!("{SYSTEMD_UNIT_NAME}.socket")])
        .status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd_units() -> Result<()> {
    use colored::Colorize;

    let unit_dir = systemd_user_dir()?;
    let socket_file = unit_dir.join(format!("{SYSTEMD_UNIT_NAME}.socket"));

    // Only act if unit files exist
    if !socket_file.exists() {
        return Ok(());
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", &format!("{SYSTEMD_UNIT_NAME}.socket")])
        .status();

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", &format!("{SYSTEMD_UNIT_NAME}.service")])
        .status();

    for name in [
        format!("{SYSTEMD_UNIT_NAME}.socket"),
        format!("{SYSTEMD_UNIT_NAME}.service"),
    ] {
        let path = unit_dir.join(&name);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("could not remove {}", path.display()))?;
            eprintln!("{} removed {}", "ok:".green(), path.display());
        }
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launchagent_plist_contains_required_fields() {
        let plist = generate_launchagent_plist("/usr/local/bin/devproxy", 443);
        assert!(plist.contains("com.devproxy.daemon"), "should have label");
        assert!(plist.contains("/usr/local/bin/devproxy"), "should have binary path");
        assert!(plist.contains("<key>Sockets</key>"), "should have Sockets");
        assert!(plist.contains("443"), "should have port 443");
        assert!(plist.contains("Listeners"), "should have socket name matching code");
        assert!(plist.contains("127.0.0.1"), "should bind to localhost only");
    }

    #[test]
    fn test_launchagent_plist_custom_port() {
        let plist = generate_launchagent_plist("/opt/devproxy", 8443);
        assert!(plist.contains("8443"), "should use custom port");
        assert!(plist.contains("/opt/devproxy"), "should use custom binary path");
    }

    #[test]
    fn test_systemd_socket_unit_binds_localhost() {
        let unit = generate_systemd_socket_unit(443);
        assert!(unit.contains("ListenStream=127.0.0.1:443"), "should listen on localhost:443");
        assert!(unit.contains("[Socket]"), "should have Socket section");
    }

    #[test]
    fn test_systemd_socket_unit_custom_port() {
        let unit = generate_systemd_socket_unit(8443);
        assert!(unit.contains("ListenStream=127.0.0.1:8443"), "should use custom port on localhost");
    }

    #[test]
    fn test_systemd_service_unit_contains_binary() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy");
        assert!(unit.contains("/usr/local/bin/devproxy"), "should have binary path");
        assert!(unit.contains("daemon"), "should run daemon subcommand");
        assert!(unit.contains("Type=simple"), "should have Type=simple");
    }

    #[test]
    fn test_systemd_service_references_socket() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy");
        assert!(unit.contains("Requires=devproxy.socket"), "should require socket unit");
    }

    #[test]
    fn test_is_socket_activation_disabled_default() {
        // In normal test runs, env var should not be set
        // (unless the runner explicitly sets it, which is fine)
        // This test just verifies the function doesn't panic
        let _ = is_socket_activation_disabled();
    }
}
```

**Step 4: Register the module in `src/main.rs`**

Add `mod platform;` after line 5 (`mod proxy;`).

**Step 5: Run tests to verify they pass**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --lib platform::tests -v`
Expected: PASS

**Step 6: Commit**

```bash
git add src/platform.rs src/main.rs
git commit -m "feat: add platform module with stop/uninstall separation, localhost binding, and setcap fallback"
```

---

### Task 4: Rewrite `init` daemon startup to use socket activation

**Files:**
- Modify: `src/commands/init.rs:297-387` (the daemon startup block)

**Step 1: Understand the change**

The init command currently spawns the daemon directly with `setsid()`. The new flow:
1. Kill any stale daemon via `kill_stale_daemon()` (updated in Task 5)
2. Try `platform::install_daemon()` for socket activation (will bail immediately if `DEVPROXY_NO_SOCKET_ACTIVATION` is set)
3. On failure (including from the env var bail), fall back to `spawn_daemon_directly()`

**Step 2: Modify the daemon startup section**

Replace the `else` branch of `if no_daemon` in `src/commands/init.rs` (lines 300-387) with:

```rust
    } else {
        // Kill any stale daemon from a previous init
        kill_stale_daemon()?;

        eprintln!("starting daemon on port {port}...");
        let exe = std::env::current_exe().context("could not determine binary path")?;

        // Try platform-specific socket activation (launchd on macOS,
        // systemd on Linux). The daemon receives pre-bound sockets from
        // the OS, so it runs as the current user — no sudo needed for
        // privileged ports.
        // install_daemon returns Err when DEVPROXY_NO_SOCKET_ACTIVATION is
        // set, triggering the fallback path.
        let mut activated = false;

        match crate::platform::install_daemon(&exe, port) {
            Ok(()) => {
                // Wait for daemon to become responsive (socket activation
                // startup may be slower than direct spawn)
                match wait_for_daemon(Duration::from_secs(10)) {
                    Ok(()) => {
                        eprintln!("{} daemon started via socket activation", "ok:".green());
                        activated = true;
                    }
                    Err(e) => {
                        eprintln!("{} daemon failed to start: {e}", "error:".red());
                        #[cfg(target_os = "macos")]
                        {
                            eprintln!(
                                "  {} check: launchctl print gui/$(id -u)/{}",
                                "hint:".yellow(),
                                crate::platform::LAUNCHD_LABEL
                            );
                            eprintln!(
                                "  {} log: /tmp/devproxy-daemon.log",
                                "hint:".yellow()
                            );
                        }
                        #[cfg(target_os = "linux")]
                        {
                            eprintln!(
                                "  {} check: systemctl --user status devproxy.socket",
                                "hint:".yellow()
                            );
                            eprintln!(
                                "  {} check: journalctl --user -u devproxy -n 20",
                                "hint:".yellow()
                            );
                        }
                        bail!("daemon failed to start. See hints above.");
                    }
                }
            }
            Err(e) => {
                // Log only if not the expected DEVPROXY_NO_SOCKET_ACTIVATION bail
                if !crate::platform::is_socket_activation_disabled() {
                    eprintln!(
                        "{} socket activation setup failed: {e}",
                        "warn:".yellow()
                    );
                    eprintln!("{} falling back to direct daemon spawn...", "info:".cyan());
                }
            }
        }

        if !activated {
            spawn_daemon_directly(&exe, port, domain)?;
        }
    }
```

**Step 3: Extract the direct spawn logic into a helper function**

Add after `wait_for_daemon`:

```rust
/// Fallback: spawn the daemon directly as a detached process.
/// Used when socket activation is not available.
fn spawn_daemon_directly(exe: &std::path::Path, port: u16, domain: &str) -> Result<()> {
    if port < 1024 {
        eprintln!(
            "{} port {port} requires root privileges (sudo) in fallback mode",
            "info:".cyan()
        );
    }

    let mut cmd = std::process::Command::new(exe);
    cmd.args(["daemon", "--port", &port.to_string()]);

    if let Ok(dir) = std::env::var("DEVPROXY_CONFIG_DIR") {
        cmd.env("DEVPROXY_CONFIG_DIR", dir);
    }

    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let daemon_log_path = Config::daemon_log_path()?;
    let daemon_log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&daemon_log_path)
        .with_context(|| {
            format!("could not open daemon log at {}", daemon_log_path.display())
        })?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(daemon_log_file));

    let mut child = cmd.spawn().context("could not spawn daemon")?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    match wait_for_daemon(Duration::from_secs(5)) {
        Ok(()) => {
            eprintln!("{} daemon started (pid: {pid})", "ok:".green());
        }
        Err(e) => {
            eprintln!("{} daemon failed to start: {e}", "error:".red());
            if let Ok(log_contents) = std::fs::read_to_string(&daemon_log_path) {
                let last_lines: Vec<&str> = log_contents.lines().rev().take(10).collect();
                if !last_lines.is_empty() {
                    eprintln!(
                        "  {} daemon log ({}):",
                        "log:".cyan(),
                        daemon_log_path.display()
                    );
                    for line in last_lines.into_iter().rev() {
                        eprintln!("    {line}");
                    }
                }
            }
            if port < 1024 {
                eprintln!(
                    "  {} port {port} requires root. Try: sudo devproxy init --domain {domain}",
                    "hint:".yellow()
                );
            }
            bail!("daemon failed to start. See error above.");
        }
    }

    Ok(())
}
```

**Step 4: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: all tests PASS.

**Step 5: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat: init uses socket activation with direct-spawn fallback"
```

---

### Task 5: Update `kill_stale_daemon` to use `stop_daemon` (not uninstall)

**Files:**
- Modify: `src/commands/init.rs:82-189` (the `kill_stale_daemon` function)

**Step 1: Understand the change**

Currently `kill_stale_daemon` sends SIGTERM/SIGKILL. When the daemon is managed by launchd (with KeepAlive=true), SIGTERM causes launchd to immediately restart it. We need `launchctl bootout` / `systemctl --user stop` first.

Critical: we use `stop_daemon()` here, NOT `uninstall_daemon()`. `stop_daemon()` already respects `DEVPROXY_NO_SOCKET_ACTIVATION` and checks for file existence before acting, so tests using `DEVPROXY_CONFIG_DIR` + `DEVPROXY_NO_SOCKET_ACTIVATION` will not touch real LaunchAgents.

**Step 2: Add platform-aware stop at the beginning of `kill_stale_daemon`**

Insert at the top of `kill_stale_daemon`, before the PID file check (after line 82):

```rust
pub fn kill_stale_daemon() -> Result<()> {
    // Try platform-specific stop first (handles launchd/systemd managed daemons).
    // Uses stop_daemon() (not uninstall) to preserve plist/unit files.
    // If the daemon is managed by launchd (KeepAlive=true), sending SIGTERM
    // would just cause launchd to restart it. bootout is the correct way.
    //
    // stop_daemon() respects DEVPROXY_NO_SOCKET_ACTIVATION and checks for
    // plist/unit file existence, so it won't interfere with real installed
    // daemons when called from tests.
    if let Err(e) = crate::platform::stop_daemon() {
        // Not fatal — daemon may not be platform-managed
        eprintln!(
            "{} platform stop: {e} (falling back to signal)",
            "info:".cyan()
        );
    } else {
        // Give the platform manager a moment to stop the daemon
        std::thread::sleep(Duration::from_millis(500));
    }

    let pid_path = Config::pid_path()?;
    let socket_path = Config::socket_path()?;
    // ... rest of existing code unchanged from here
```

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: PASS

**Step 4: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat: kill_stale_daemon uses platform stop (not uninstall) before signal fallback"
```

---

### Task 6: Update `devproxy update` daemon stop — minor message update

**Files:**
- Modify: `src/commands/update.rs:275-279` (minor message update only)

**Step 1: Verify the update flow**

`do_update` calls `super::init::kill_stale_daemon()`. After Task 5, that now calls `platform::stop_daemon()` which uses `launchctl bootout` / `systemctl --user stop` WITHOUT removing files. After the binary is replaced in-place, the plist/unit files still point to the correct path.

**Step 2: Update the hint message slightly**

Replace in `src/commands/update.rs`:
```rust
    eprintln!(
        "{} run {} to restart the daemon",
        "info:".cyan(),
        "devproxy init".bold()
    );
```

With:
```rust
    eprintln!(
        "{} run {} to restart the daemon with the new version",
        "info:".cyan(),
        "devproxy init".bold()
    );
```

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: PASS

**Step 4: Commit**

```bash
git add src/commands/update.rs
git commit -m "chore: update restart hint message after self-update"
```

---

### Task 7: Fix existing e2e tests for socket activation compatibility

**Files:**
- Modify: `tests/e2e.rs:994-1091` (`test_reinit_kills_stale_daemon`)

**Step 1: Understand the breakage**

The `test_reinit_kills_stale_daemon` test runs `devproxy init --domain ... --port <ephemeral>`. After our changes, init calls `platform::install_daemon()` which on macOS calls `launchctl bootstrap` — installing a real LaunchAgent on the developer's machine.

Also, the first daemon in this test is started via `start_test_daemon` (direct spawn), then the second `init` call tries to kill it. `kill_stale_daemon` now calls `stop_daemon()` which would try to bootout a real LaunchAgent.

**Fix:** Set `DEVPROXY_NO_SOCKET_ACTIVATION=1` on the init Command. This causes:
- `install_daemon()` to bail (triggering `spawn_daemon_directly` fallback)
- `stop_daemon()` to return `Ok(())` immediately (no bootout)
- `uninstall_daemon()` to return `Ok(())` (no file deletion)

The test's assertions `stderr.contains("killing stale daemon")` and `stderr.contains("daemon started")` will match the PID-based kill path and `spawn_daemon_directly` output, same as before.

**Step 2: Update `test_reinit_kills_stale_daemon` in `tests/e2e.rs`**

At line 1032, add `DEVPROXY_NO_SOCKET_ACTIVATION` to the init Command:

```rust
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
```

**Step 3: Verify `test_init_output_includes_sudo_note` still passes**

This test uses `--no-daemon`, so the daemon startup path is not entered. The "sudo" string appears in the CA trust failure message (since tests run without sudo). No change needed.

**Step 4: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e`
Expected: all non-ignored tests PASS

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e -- --ignored test_reinit_kills_stale_daemon`
Expected: PASS

**Step 5: Commit**

```bash
git add tests/e2e.rs
git commit -m "fix: add DEVPROXY_NO_SOCKET_ACTIVATION to test_reinit for test isolation"
```

---

### Task 8: Add macOS launchd integration test using ephemeral port

**Files:**
- Modify: `tests/e2e.rs` (add new test)

**Step 1: Write the test**

This test installs a real LaunchAgent with an ephemeral port, verifies the daemon starts and responds to IPC, then tears it down. It is `#[ignore]` and `#[cfg(target_os = "macos")]` so it only runs explicitly.

```rust
/// macOS-only: verify socket activation via launchd with an ephemeral port.
/// Installs a real LaunchAgent plist, waits for the daemon to respond,
/// then uninstalls. Run with: cargo test --test e2e -- --ignored test_launchd_socket_activation
#[test]
#[ignore]
#[cfg(target_os = "macos")]
fn test_launchd_socket_activation() {
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

    assert!(
        output.status.success(),
        "init should succeed: {stderr}"
    );
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
        let home = std::env::var("HOME").unwrap();
        let plist_path = format!(
            "{home}/Library/LaunchAgents/com.devproxy.daemon.plist"
        );
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
    }

    // Cleanup: bootout the agent and remove plist
    if used_socket_activation {
        let uid_output = Command::new("id").arg("-u").output().unwrap();
        let uid = String::from_utf8_lossy(&uid_output.stdout).trim().to_string();
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/com.devproxy.daemon")])
            .status();
        let home = std::env::var("HOME").unwrap();
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
```

**Step 2: Run the test**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e -- --ignored test_launchd_socket_activation`
Expected: PASS on macOS

**Step 3: Commit**

```bash
git add tests/e2e.rs
git commit -m "test: add macOS launchd socket activation integration test"
```

---

### Task 9: Add Docker-based Linux e2e test infrastructure

**Files:**
- Create: `tests/linux-docker/Dockerfile`
- Create: `tests/linux-docker/run-tests.sh`
- Create: `tests/linux-docker/test-systemd.sh`
- Create: `tests/linux-docker/test-listen-fds.sh`
- Create: `tests/linux-docker/test-setcap.sh`

**Step 1: Understand the approach**

The user requires Docker-based tests that prove the Linux systemd and setcap paths work, runnable from any dev machine (including macOS). This means:

1. A Dockerfile that builds devproxy inside a Linux container with systemd support
2. Shell scripts that exercise each path inside the container
3. A runner script that builds the container and runs all tests, reporting results

We use a Debian-based image with systemd. The container runs with `--privileged` to allow systemd to work (needed for `systemctl --user`).

**Step 2: Create the Dockerfile**

```dockerfile
# tests/linux-docker/Dockerfile
# Builds devproxy and runs Linux-specific integration tests.
# Requires: docker build --tag devproxy-linux-test .
# Run:      docker run --rm --privileged devproxy-linux-test

FROM rust:1.84-bookworm AS builder

# Install libcap2-bin for setcap
RUN apt-get update && apt-get install -y libcap2-bin && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release
RUN cp target/release/devproxy /usr/local/bin/devproxy

FROM debian:bookworm

# Install systemd + dbus + libcap2-bin (for setcap/getcap)
RUN apt-get update && \
    apt-get install -y systemd dbus libcap2-bin curl procps && \
    rm -rf /var/lib/apt/lists/*

# Copy the built binary and test scripts
COPY --from=builder /usr/local/bin/devproxy /usr/local/bin/devproxy
COPY tests/linux-docker/test-systemd.sh /tests/test-systemd.sh
COPY tests/linux-docker/test-listen-fds.sh /tests/test-listen-fds.sh
COPY tests/linux-docker/test-setcap.sh /tests/test-setcap.sh
RUN chmod +x /tests/*.sh

# Create a non-root user for testing user-level systemd/setcap
RUN useradd -m -s /bin/bash testuser

# Enable lingering so systemd user session works without login
RUN loginctl enable-linger testuser 2>/dev/null || mkdir -p /var/lib/systemd/linger && touch /var/lib/systemd/linger/testuser

# Use systemd as init (PID 1) so systemctl --user works
STOPSIGNAL SIGRTMIN+3
ENTRYPOINT ["/sbin/init"]
```

**Step 3: Create the systemd test script**

```bash
#!/bin/bash
# tests/linux-docker/test-systemd.sh
# Tests the systemd socket activation path.
# Run inside the Docker container as testuser.

set -euo pipefail

echo "=== Test: systemd socket activation ==="

export DEVPROXY_CONFIG_DIR="/tmp/devproxy-test-systemd"
mkdir -p "$DEVPROXY_CONFIG_DIR"

# Init with certs only (--no-daemon)
devproxy init --domain test.dev --no-daemon 2>&1 || true

# Install daemon via init (should use systemd path)
devproxy init --domain test.dev --port 8443 2>&1
INIT_OUTPUT=$(devproxy init --domain test.dev --port 8443 2>&1 || true)
echo "$INIT_OUTPUT"

# Wait for daemon
sleep 3

# Check status
STATUS=$(devproxy status 2>&1 || true)
echo "Status: $STATUS"

if echo "$STATUS" | grep -q "running"; then
    echo "PASS: daemon is running via systemd"
else
    echo "FAIL: daemon not running"
    systemctl --user status devproxy.socket 2>&1 || true
    systemctl --user status devproxy.service 2>&1 || true
    journalctl --user -u devproxy --no-pager -n 20 2>&1 || true
    exit 1
fi

# Verify systemd unit files were created
UNIT_DIR="$HOME/.config/systemd/user"
if [ -f "$UNIT_DIR/devproxy.socket" ]; then
    echo "PASS: socket unit exists"
    # Verify localhost binding
    if grep -q "ListenStream=127.0.0.1:8443" "$UNIT_DIR/devproxy.socket"; then
        echo "PASS: socket binds to localhost only"
    else
        echo "FAIL: socket unit does not bind to localhost"
        cat "$UNIT_DIR/devproxy.socket"
        exit 1
    fi
else
    echo "FAIL: socket unit not created"
    exit 1
fi

# Cleanup
devproxy init --domain test.dev --no-daemon 2>&1 || true
systemctl --user stop devproxy.socket devproxy.service 2>/dev/null || true
systemctl --user disable devproxy.socket 2>/dev/null || true
rm -rf "$DEVPROXY_CONFIG_DIR"
echo "=== PASS: systemd test complete ==="
```

**Step 4: Create the LISTEN_FDS test script**

```bash
#!/bin/bash
# tests/linux-docker/test-listen-fds.sh
# Tests the LISTEN_FDS protocol directly (simulating systemd socket activation).
# Run inside the Docker container as testuser.

set -euo pipefail

echo "=== Test: LISTEN_FDS protocol ==="

export DEVPROXY_CONFIG_DIR="/tmp/devproxy-test-listen-fds"
mkdir -p "$DEVPROXY_CONFIG_DIR"

# Generate certs
devproxy init --domain test.dev --no-daemon 2>&1 || true

# Use Python to pre-bind a socket and pass fd 3 to the daemon.
# Python's socket module makes this straightforward.
python3 -c "
import socket, os, subprocess, sys, time

# Bind a TCP socket on an ephemeral port
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
sock.bind(('127.0.0.1', 0))
sock.listen(128)
port = sock.getsockname()[1]
print(f'Pre-bound port: {port}')

# dup2 the socket fd to fd 3 (SD_LISTEN_FDS_START)
fd = sock.fileno()
if fd != 3:
    os.dup2(fd, 3)
    sock.close()  # close the original fd; fd 3 is now the socket

# Clear CLOEXEC on fd 3 so it survives exec
import fcntl
flags = fcntl.fcntl(3, fcntl.F_GETFD)
fcntl.fcntl(3, fcntl.F_SETFD, flags & ~fcntl.FD_CLOEXEC)

# Spawn daemon with LISTEN_FDS
env = os.environ.copy()
env['LISTEN_FDS'] = '1'
env['DEVPROXY_CONFIG_DIR'] = os.environ['DEVPROXY_CONFIG_DIR']

proc = subprocess.Popen(
    ['devproxy', 'daemon', '--port', str(port)],
    env=env,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.PIPE,
    preexec_fn=lambda: os.environ.update({'LISTEN_PID': str(os.getpid())}) or None,
    close_fds=False,
)

# Set LISTEN_PID after fork
# Actually, LISTEN_PID needs to be the child's PID. We set it in the env
# but the child's PID is different. Re-approach: pass LISTEN_PID via env
# after we know the child PID... but Popen has already exec'd.
# Better: don't set LISTEN_PID in env; let the daemon check getpid().
# Wait, the protocol requires LISTEN_PID to match getpid() in the daemon.
# Since we set LISTEN_FDS but not LISTEN_PID, the daemon will see
# LISTEN_PID is missing and fall back. We need to set it.
#
# Workaround: use a wrapper that sets LISTEN_PID to its own PID.
proc.terminate()
proc.wait()

# Use a shell wrapper instead
import tempfile
wrapper = tempfile.NamedTemporaryFile(mode='w', suffix='.sh', delete=False)
wrapper.write(f'''#!/bin/bash
export LISTEN_PID=\$\$
exec devproxy daemon --port {port}
''')
wrapper.close()
os.chmod(wrapper.name, 0o755)

proc = subprocess.Popen(
    [wrapper.name],
    env=env,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.PIPE,
    close_fds=False,
)

# Wait for IPC socket
socket_path = os.path.join(os.environ['DEVPROXY_CONFIG_DIR'], 'devproxy.sock')
for i in range(50):
    if os.path.exists(socket_path):
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(socket_path)
            s.close()
            print('Daemon started successfully with LISTEN_FDS')
            break
        except:
            pass
    time.sleep(0.1)
else:
    stderr = proc.stderr.read().decode() if proc.stderr else ''
    print(f'FAIL: daemon did not start. stderr: {stderr}')
    proc.kill()
    os.unlink(wrapper.name)
    sys.exit(1)

# Verify via status command
result = subprocess.run(['devproxy', 'status'], capture_output=True, text=True,
                       env=env)
if 'running' in result.stderr:
    print('PASS: daemon running with activated socket')
else:
    print(f'FAIL: status output: {result.stderr}')
    proc.kill()
    os.unlink(wrapper.name)
    sys.exit(1)

proc.terminate()
proc.wait()
os.unlink(wrapper.name)
print('PASS: LISTEN_FDS test complete')
"

rm -rf "$DEVPROXY_CONFIG_DIR"
echo "=== PASS: LISTEN_FDS test complete ==="
```

**Step 5: Create the setcap test script**

```bash
#!/bin/bash
# tests/linux-docker/test-setcap.sh
# Tests the setcap fallback path (when systemd is not available).
# Run inside the Docker container as root (setcap requires root).

set -euo pipefail

echo "=== Test: setcap fallback ==="

export DEVPROXY_CONFIG_DIR="/tmp/devproxy-test-setcap"
mkdir -p "$DEVPROXY_CONFIG_DIR"

# Generate certs
devproxy init --domain test.dev --no-daemon 2>&1 || true

# Copy binary so we can setcap it without affecting other tests
cp /usr/local/bin/devproxy /tmp/devproxy-setcap-test
chmod 755 /tmp/devproxy-setcap-test

# Apply setcap
setcap cap_net_bind_service=+ep /tmp/devproxy-setcap-test

# Verify capability is set
CAPS=$(getcap /tmp/devproxy-setcap-test)
echo "Capabilities: $CAPS"
if echo "$CAPS" | grep -q "cap_net_bind_service"; then
    echo "PASS: capability applied"
else
    echo "FAIL: capability not applied"
    exit 1
fi

# Start daemon on port 443 as non-root user with the setcap binary
# (This proves setcap allows binding port 443 without root)
su testuser -c "
    export DEVPROXY_CONFIG_DIR='$DEVPROXY_CONFIG_DIR'
    export DEVPROXY_NO_SOCKET_ACTIVATION=1
    /tmp/devproxy-setcap-test daemon --port 443 &
    DAEMON_PID=\$!
    sleep 2

    STATUS=\$(/tmp/devproxy-setcap-test status 2>&1 || true)
    echo \"Status: \$STATUS\"
    if echo \"\$STATUS\" | grep -q 'running'; then
        echo 'PASS: daemon running on port 443 as non-root (setcap)'
    else
        echo 'FAIL: daemon not running on port 443'
        kill \$DAEMON_PID 2>/dev/null || true
        exit 1
    fi

    kill \$DAEMON_PID 2>/dev/null || true
    wait \$DAEMON_PID 2>/dev/null || true
"

rm -f /tmp/devproxy-setcap-test
rm -rf "$DEVPROXY_CONFIG_DIR"
echo "=== PASS: setcap test complete ==="
```

**Step 6: Create the runner script**

```bash
#!/bin/bash
# tests/linux-docker/run-tests.sh
# Builds and runs the Linux integration tests in Docker.
# Run from the repo root: bash tests/linux-docker/run-tests.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "Building Linux test container..."
docker build -t devproxy-linux-test -f "$SCRIPT_DIR/Dockerfile" "$REPO_ROOT"

echo "Starting container with systemd..."
CONTAINER_ID=$(docker run -d --privileged \
    --tmpfs /run --tmpfs /run/lock \
    -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
    devproxy-linux-test)

# Wait for systemd to fully initialize
echo "Waiting for systemd to initialize..."
for i in $(seq 1 30); do
    if docker exec "$CONTAINER_ID" systemctl is-system-running --quiet 2>/dev/null; then
        break
    fi
    if docker exec "$CONTAINER_ID" systemctl is-system-running 2>/dev/null | grep -q "running\|degraded"; then
        break
    fi
    sleep 1
done

FAILED=0

echo ""
echo "=========================================="
echo "Running LISTEN_FDS test..."
echo "=========================================="
if docker exec -u testuser "$CONTAINER_ID" bash /tests/test-listen-fds.sh; then
    echo ">>> LISTEN_FDS: PASS"
else
    echo ">>> LISTEN_FDS: FAIL"
    FAILED=1
fi

echo ""
echo "=========================================="
echo "Running systemd test..."
echo "=========================================="
if docker exec -u testuser "$CONTAINER_ID" bash -c "export XDG_RUNTIME_DIR=/run/user/\$(id -u) && /tests/test-systemd.sh"; then
    echo ">>> systemd: PASS"
else
    echo ">>> systemd: FAIL"
    FAILED=1
fi

echo ""
echo "=========================================="
echo "Running setcap test..."
echo "=========================================="
if docker exec "$CONTAINER_ID" bash /tests/test-setcap.sh; then
    echo ">>> setcap: PASS"
else
    echo ">>> setcap: FAIL"
    FAILED=1
fi

# Cleanup
docker stop "$CONTAINER_ID" >/dev/null 2>&1
docker rm "$CONTAINER_ID" >/dev/null 2>&1

echo ""
if [ $FAILED -eq 0 ]; then
    echo "All Linux integration tests PASSED"
else
    echo "Some Linux integration tests FAILED"
    exit 1
fi
```

**Step 7: Run the Docker tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && bash tests/linux-docker/run-tests.sh`
Expected: All three tests PASS

**Step 8: Commit**

```bash
git add tests/linux-docker/
git commit -m "test: add Docker-based Linux integration tests for systemd, LISTEN_FDS, and setcap"
```

---

### Task 10: Run full test suite and verify

**Files:** none (verification only)

**Step 1: Build the project**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo build`
Expected: compiles cleanly

**Step 2: Run clippy**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo clippy -- -D warnings`
Expected: no warnings

**Step 3: Run all unit tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: all PASS

**Step 4: Run e2e tests (non-Docker, non-ignored subset)**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e`
Expected: all non-ignored tests PASS. Specifically verify:
- `test_init_output_includes_sudo_note` still passes (sudo is mentioned in CA trust output)
- `test_init_generates_certs` still passes (uses `--no-daemon`)

**Step 5: Run ignored macOS e2e test**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e -- --ignored test_launchd_socket_activation`
Expected: PASS on macOS

**Step 6: Run Docker-based Linux tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && bash tests/linux-docker/run-tests.sh`
Expected: All PASS

**Step 7: Commit** (if any fixups were needed)

---

### Task 11: Final commit and cleanup

**Step 1: Run cargo fmt**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo fmt`

**Step 2: Final commit if formatting changed**

```bash
git add -A
git commit -m "style: cargo fmt"
```

---

## Summary of changed files

| File | Action | Description |
|------|--------|-------------|
| `src/proxy/socket_activation.rs` | Create | Platform-specific fd acquisition from launchd/systemd |
| `src/proxy/mod.rs` | Modify | Add socket_activation module, use it in run_daemon |
| `src/platform.rs` | Create | LaunchAgent/systemd/setcap management with stop vs uninstall separation, file-existence guards, DEVPROXY_NO_SOCKET_ACTIVATION support, localhost-only binding |
| `src/main.rs` | Modify | Add `mod platform;` |
| `src/commands/init.rs` | Modify | Socket activation install with fallback, `spawn_daemon_directly` helper, `kill_stale_daemon` uses `stop_daemon()` |
| `src/commands/update.rs` | Modify | Minor message update |
| `tests/e2e.rs` | Modify | Fix `test_reinit_kills_stale_daemon` with `DEVPROXY_NO_SOCKET_ACTIVATION`, add macOS launchd integration test |
| `tests/linux-docker/Dockerfile` | Create | Linux test container with systemd |
| `tests/linux-docker/run-tests.sh` | Create | Docker test runner |
| `tests/linux-docker/test-systemd.sh` | Create | Systemd socket activation test |
| `tests/linux-docker/test-listen-fds.sh` | Create | LISTEN_FDS protocol test |
| `tests/linux-docker/test-setcap.sh` | Create | setcap fallback test |

## Design decisions made

1. **`stop_daemon()` vs `uninstall_daemon()`**: `stop_daemon()` halts the process (bootout/systemctl stop) without removing files. `uninstall_daemon()` stops and deletes files. `kill_stale_daemon` uses stop (preserves files for update), while init's internal flow calls uninstall only when needed.

2. **File-existence guard on `stop_daemon()`**: On macOS, `stop_daemon()` checks if `~/Library/LaunchAgents/com.devproxy.daemon.plist` exists before calling `launchctl bootout`. On Linux, it checks if `~/.config/systemd/user/devproxy.socket` exists before calling `systemctl stop`. This prevents tests using `DEVPROXY_CONFIG_DIR` from accidentally booting out a real production LaunchAgent.

3. **`DEVPROXY_NO_SOCKET_ACTIVATION` env var**: All three public functions (`install_daemon`, `stop_daemon`, `uninstall_daemon`) respect this. `install_daemon` returns `Err` (triggers fallback). `stop_daemon` and `uninstall_daemon` return `Ok(())` (no-op). This gives tests complete isolation from the host's service management.

4. **Systemd `ListenStream=127.0.0.1:{port}`**: The socket unit binds to localhost only, matching the macOS plist `SockNodeName=127.0.0.1` and the existing `TcpListener::bind("127.0.0.1:...")`. Using a bare port number would bind to `[::]` (all interfaces), exposing the dev proxy to the network.

5. **Linux setcap fallback**: On Linux, `install_daemon` first tries systemd. If `systemctl --user` is unavailable, it falls back to `sudo setcap cap_net_bind_service=+ep` on the binary.

6. **Docker-based Linux tests**: Instead of `#[cfg(target_os = "linux")]` Rust tests that can't run on macOS, we use a Docker container with systemd to prove all three Linux paths work. The container runs `debian:bookworm` with systemd as PID 1, a non-root `testuser`, and test scripts for each path. The runner script can be invoked from any OS with Docker.

7. **Socket name "Listeners"**: Matches the plist Sockets key name. `launch_activate_socket("Listeners", ...)` retrieves fds for the socket named "Listeners" in the plist.

8. **KeepAlive=true in plist**: Ensures launchd restarts the daemon if it crashes. `kill_stale_daemon` uses `launchctl bootout` (via `stop_daemon`) instead of SIGTERM to avoid launchd immediately restarting the process.

9. **No new Cargo dependencies**: Uses `libc` (already a dependency) for `launch_activate_socket` FFI and standard library for systemd env var parsing.
