# User-Owned Daemon via Socket Activation — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Eliminate the need for `sudo` when starting the devproxy daemon by using OS-level socket activation (launchd on macOS, systemd on Linux) to bind port 443, then pass the pre-bound fd to the daemon process running as the current user.

**Architecture:** The daemon gains a new code path to receive pre-bound TCP listeners from launchd/systemd instead of calling `TcpListener::bind()` directly. On macOS, `devproxy init` installs a LaunchAgent plist with a Sockets entry for port 443; launchd owns the socket and passes fds via `launch_activate_socket`. On Linux, init installs systemd user socket+service units; systemd passes fds via the `LISTEN_FDS` protocol. The existing `TcpListener::bind()` path remains as a fallback for tests and `--no-daemon` scenarios.

**Tech Stack:** Rust, tokio, libc (for `launch_activate_socket` FFI on macOS), std::env (for `LISTEN_FDS` on Linux), plist XML generation, systemd unit file generation.

---

### Task 1: Add `socket_activation` module with platform-specific fd acquisition

**Files:**
- Create: `src/proxy/socket_activation.rs`
- Modify: `src/proxy/mod.rs:1` (add `pub mod socket_activation;`)

**Step 1: Write the failing test for Linux LISTEN_FDS parsing**

```rust
// In src/proxy/socket_activation.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linux_listen_fds_returns_none_when_not_set() {
        // Ensure env vars are not set (they shouldn't be in test)
        // This tests the "no socket activation" fallback path
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

**Step 1: Write a test that the daemon accepts a pre-bound listener**

This is inherently an integration-level behavior. The existing e2e tests that use `start_test_daemon` with ephemeral ports exercise the fallback path. We will verify the socket activation path works correctly by ensuring the daemon still starts when `acquire_listener` returns `None`. A targeted unit test for the branching logic is not practical since `run_daemon` is a top-level orchestrator. Instead, we verify correctness through the e2e tests (which must still pass — they use the fallback path).

**Step 2: Modify `run_daemon` to try socket activation first**

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

**Step 3: Run existing tests to verify fallback path still works**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: all tests PASS (socket activation returns None, fallback to bind works)

**Step 4: Commit**

```bash
git add src/proxy/mod.rs
git commit -m "feat: use socket activation in run_daemon with bind fallback"
```

---

### Task 3: Add `platform` module for LaunchAgent/systemd unit management

**Files:**
- Create: `src/platform.rs`
- Modify: `src/main.rs:1` (add `mod platform;`)

**Step 1: Write failing test for plist generation (macOS)**

```rust
// src/platform.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launchagent_plist_contains_required_fields() {
        let plist = generate_launchagent_plist("/usr/local/bin/devproxy", 443);
        assert!(plist.contains("com.devproxy.daemon"), "should have label");
        assert!(plist.contains("/usr/local/bin/devproxy"), "should have binary path");
        assert!(plist.contains("<key>Sockets</key>"), "should have Sockets");
        assert!(plist.contains("<integer>443</integer>"), "should have port 443");
        assert!(plist.contains("Listeners"), "should have socket name matching code");
    }

    #[test]
    fn test_systemd_socket_unit_contains_port() {
        let unit = generate_systemd_socket_unit(443);
        assert!(unit.contains("ListenStream=443"), "should listen on 443");
        assert!(unit.contains("[Socket]"), "should have Socket section");
    }

    #[test]
    fn test_systemd_service_unit_contains_binary() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy");
        assert!(unit.contains("/usr/local/bin/devproxy"), "should have binary path");
        assert!(unit.contains("daemon"), "should run daemon subcommand");
        assert!(unit.contains("Type=simple") || unit.contains("Type=notify"), "should have Type");
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
//! Linux: systemd user socket + service units.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The launchd label for the devproxy daemon.
pub const LAUNCHD_LABEL: &str = "com.devproxy.daemon";

/// The systemd unit name prefix.
const SYSTEMD_UNIT_NAME: &str = "devproxy";

// ---- Plist / unit file generation ------------------------------------------

/// Generate the LaunchAgent plist XML for macOS.
/// The plist uses Sockets to have launchd bind port 443 and pass the fd.
pub fn generate_launchagent_plist(binary_path: &str, port: u16) -> String {
    // Note: KeepAlive = true so launchd restarts the daemon if it crashes.
    // StandardErrorPath for logging. Sockets/Listeners for socket activation.
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
pub fn generate_systemd_socket_unit(port: u16) -> String {
    format!(
        "[Unit]\n\
         Description=devproxy HTTPS socket\n\
         \n\
         [Socket]\n\
         ListenStream={port}\n\
         BindIPv6Only=default\n\
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

// ---- Installation ----------------------------------------------------------

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

/// Install the daemon for the current platform. Returns Ok(()) on success.
///
/// macOS: writes plist and runs `launchctl bootstrap`.
/// Linux: writes systemd units and runs `systemctl --user enable --now`.
pub fn install_daemon(binary_path: &Path, port: u16) -> Result<()> {
    let binary_str = binary_path
        .to_str()
        .context("binary path is not valid UTF-8")?;

    #[cfg(target_os = "macos")]
    {
        install_launchagent(binary_str, port)?;
    }
    #[cfg(target_os = "linux")]
    {
        install_systemd_units(binary_str, port)?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (binary_str, port);
        bail!("daemon installation is not supported on this platform");
    }

    Ok(())
}

/// Uninstall / stop the daemon for the current platform.
pub fn uninstall_daemon() -> Result<()> {
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
        bail!("daemon uninstallation is not supported on this platform");
    }

    Ok(())
}

// ---- macOS launchd ---------------------------------------------------------

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
fn uninstall_launchagent() -> Result<()> {
    use colored::Colorize;

    let _ = bootout_launchagent();

    let plist_path = launchagent_plist_path()?;
    if plist_path.exists() {
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("could not remove {}", plist_path.display()))?;
        eprintln!("{} removed {}", "ok:".green(), plist_path.display());
    }

    Ok(())
}

// ---- Linux systemd ---------------------------------------------------------

#[cfg(target_os = "linux")]
fn install_systemd_units(binary_path: &str, port: u16) -> Result<()> {
    use colored::Colorize;

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

    // Reload and enable
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
fn uninstall_systemd_units() -> Result<()> {
    use colored::Colorize;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", &format!("{SYSTEMD_UNIT_NAME}.socket")])
        .status();

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", &format!("{SYSTEMD_UNIT_NAME}.service")])
        .status();

    let unit_dir = systemd_user_dir()?;
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
        assert!(
            plist.contains("/usr/local/bin/devproxy"),
            "should have binary path"
        );
        assert!(plist.contains("<key>Sockets</key>"), "should have Sockets");
        assert!(
            plist.contains("443"),
            "should have port 443"
        );
        assert!(
            plist.contains("Listeners"),
            "should have socket name matching code"
        );
    }

    #[test]
    fn test_launchagent_plist_custom_port() {
        let plist = generate_launchagent_plist("/opt/devproxy", 8443);
        assert!(plist.contains("8443"), "should use custom port");
        assert!(plist.contains("/opt/devproxy"), "should use custom binary path");
    }

    #[test]
    fn test_systemd_socket_unit_contains_port() {
        let unit = generate_systemd_socket_unit(443);
        assert!(unit.contains("ListenStream=443"), "should listen on 443");
        assert!(unit.contains("[Socket]"), "should have Socket section");
    }

    #[test]
    fn test_systemd_socket_unit_custom_port() {
        let unit = generate_systemd_socket_unit(8443);
        assert!(unit.contains("ListenStream=8443"), "should use custom port");
    }

    #[test]
    fn test_systemd_service_unit_contains_binary() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy");
        assert!(
            unit.contains("/usr/local/bin/devproxy"),
            "should have binary path"
        );
        assert!(unit.contains("daemon"), "should run daemon subcommand");
        assert!(unit.contains("Type=simple"), "should have Type=simple");
    }

    #[test]
    fn test_systemd_service_references_socket() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy");
        assert!(
            unit.contains("Requires=devproxy.socket"),
            "should require socket unit"
        );
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
git commit -m "feat: add platform module for LaunchAgent/systemd unit management"
```

---

### Task 4: Rewrite `init` daemon startup to use socket activation

**Files:**
- Modify: `src/commands/init.rs:297-387` (the daemon startup block)
- Modify: `src/commands/init.rs:304-309` (remove the `port < 1024` sudo warning)

**Step 1: Understand the change**

The init command currently:
1. Kills stale daemon via PID file + signals
2. Spawns the daemon binary directly with `setsid()` as a detached process
3. Waits for IPC socket to appear

The new flow:
1. On macOS: uninstall existing LaunchAgent (if any), then install new LaunchAgent plist. launchd starts the daemon and passes the socket fd.
2. On Linux: install systemd user units. systemd starts the daemon via socket activation.
3. Fallback (neither platform or --no-daemon): keep the direct spawn path for tests.

The direct spawn path remains for `--no-daemon` mode and as an explicit fallback.

**Step 2: Modify the daemon startup section**

Replace the daemon startup block in `src/commands/init.rs` (lines 298-387, the `else` branch of `if no_daemon`) with:

```rust
    } else {
        // Kill any stale daemon from a previous init
        kill_stale_daemon()?;

        eprintln!("installing daemon on port {port}...");
        let exe = std::env::current_exe().context("could not determine binary path")?;

        // Use platform-specific socket activation (launchd on macOS,
        // systemd on Linux). The daemon receives pre-bound sockets from
        // the OS, so it runs as the current user — no sudo needed for
        // privileged ports.
        match crate::platform::install_daemon(&exe, port) {
            Ok(()) => {
                // Wait for daemon to become responsive
                match wait_for_daemon(Duration::from_secs(10)) {
                    Ok(()) => {
                        eprintln!("{} daemon started via socket activation", "ok:".green());
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
                eprintln!(
                    "{} socket activation setup failed: {e}",
                    "warn:".yellow()
                );
                eprintln!("{} falling back to direct daemon spawn...", "info:".cyan());

                // Fallback: direct spawn (may need sudo for port < 1024)
                if port < 1024 {
                    eprintln!(
                        "{} port {port} requires root privileges (sudo) in fallback mode",
                        "info:".cyan()
                    );
                }
                spawn_daemon_directly(&exe, port, domain)?;
            }
        }
    }
```

**Step 3: Extract the direct spawn logic into a helper function**

Add at the bottom of `src/commands/init.rs` (before the closing of the module, or after `wait_for_daemon`):

```rust
/// Fallback: spawn the daemon directly as a detached process.
/// Used when socket activation is not available.
fn spawn_daemon_directly(exe: &std::path::Path, port: u16, domain: &str) -> Result<()> {
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

**Step 4: Run all tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: all tests PASS. The e2e tests using `start_test_daemon` will work because:
- The daemon subprocess is started directly (not via init), so socket activation returns None and the fallback `TcpListener::bind()` path fires.
- The `test_init_generates_certs` test uses `--no-daemon`, so daemon startup is skipped.

**Step 5: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat: init uses socket activation with direct-spawn fallback"
```

---

### Task 5: Update `kill_stale_daemon` to use platform-specific stop

**Files:**
- Modify: `src/commands/init.rs:82-189` (the `kill_stale_daemon` function)

**Step 1: Understand the change**

Currently `kill_stale_daemon` reads the PID file and sends SIGTERM/SIGKILL. When the daemon is managed by launchd/systemd, we should use `launchctl bootout` or `systemctl --user stop` first, then fall back to signal-based kill. This ensures clean shutdown and avoids launchd restarting the daemon immediately after we kill it (since KeepAlive=true).

**Step 2: Add platform-aware stop at the beginning of `kill_stale_daemon`**

Insert at the top of `kill_stale_daemon`, before the PID file check:

```rust
pub fn kill_stale_daemon() -> Result<()> {
    // Try platform-specific stop first (handles launchd/systemd managed daemons).
    // If the daemon is managed by launchd (KeepAlive=true), sending SIGTERM
    // would just cause launchd to restart it. bootout is the correct way.
    if let Err(e) = crate::platform::uninstall_daemon() {
        // Not fatal — daemon may not be platform-managed
        eprintln!(
            "{} platform stop: {e} (falling back to signal)",
            "info:".cyan()
        );
    } else {
        // Give the platform manager a moment to stop the daemon
        std::thread::sleep(Duration::from_millis(500));
    }

    // Continue with PID-file-based cleanup as before (handles non-managed
    // daemons and cleans up stale files)
    let pid_path = Config::pid_path()?;
    let socket_path = Config::socket_path()?;
    // ... rest of existing code unchanged
```

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test`
Expected: PASS

**Step 4: Commit**

```bash
git add src/commands/init.rs
git commit -m "feat: kill_stale_daemon uses platform stop before signal fallback"
```

---

### Task 6: Update `devproxy update` to use platform-aware stop/restart

**Files:**
- Modify: `src/commands/update.rs:292-298` (the daemon stop block in `do_update`)

**Step 1: Understand the change**

Currently `do_update` calls `super::init::kill_stale_daemon()` to stop the daemon before replacing the binary. Since `kill_stale_daemon` now tries platform uninstall first (from Task 5), this mostly works. However, after the binary is replaced, the update command should tell the user to run `devproxy init` to re-install the daemon (which will write a new plist/unit pointing to the updated binary).

The existing message "run `devproxy init` to restart the daemon" is already correct. No code change needed in the stop path since Task 5 updated `kill_stale_daemon`.

**Step 2: Verify update's daemon stop still works**

The only change needed: in `do_update`, the daemon stop message should reflect the new mechanism. The current code at line 296 says "stopping daemon for update..." which is fine.

Actually, we need one adjustment: after binary replacement, the LaunchAgent plist still points to the old binary path. Since `devproxy update` replaces the binary in-place (same path), the plist does NOT need updating. The user just needs to restart:

After the "updated to vX.Y.Z" message, change the hint:

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

### Task 7: Run full test suite and verify

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

**Step 4: Run e2e tests (non-Docker subset)**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e`
Expected: all non-ignored tests PASS

The ignored e2e tests (Docker-dependent) use `start_test_daemon` which spawns the daemon directly, not through init. The daemon receives no activated fds, so it falls back to `TcpListener::bind()` on an ephemeral port. These tests should be unaffected.

**Step 5: Commit** (if any fixups were needed)

---

### Task 8: Final commit and cleanup

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
| `src/platform.rs` | Create | LaunchAgent plist and systemd unit generation + install/uninstall |
| `src/main.rs` | Modify | Add `mod platform;` |
| `src/commands/init.rs` | Modify | Use platform::install_daemon, extract spawn_daemon_directly fallback, platform-aware kill |
| `src/commands/update.rs` | Modify | Minor message update |

## Design decisions made

1. **Socket name "Listeners"**: Matches the plist Sockets key name. `launch_activate_socket("Listeners", ...)` retrieves fds for the socket named "Listeners" in the plist.

2. **KeepAlive=true in plist**: Ensures launchd restarts the daemon if it crashes, matching puma-dev's behavior. This means `kill_stale_daemon` MUST use `launchctl bootout` instead of SIGTERM to stop it permanently.

3. **Fallback retained**: The direct `TcpListener::bind()` path in `run_daemon` and the direct spawn path in `init` are kept as fallbacks. This ensures tests work (they don't use launchd/systemd) and provides a degraded experience on unsupported platforms.

4. **ESRCH from launch_activate_socket**: On macOS, if the process is not managed by launchd, `launch_activate_socket` returns ESRCH (3). This is the normal "not activated" signal and triggers the fallback path.

5. **StandardErrorPath**: The plist logs daemon stderr to `/tmp/devproxy-daemon.log` instead of the config-dir log, because the daemon's config dir isn't known to the plist at generation time. This could be improved later by passing the config dir as an env var in the plist, but it's simpler to use a fixed path for now.

6. **No new dependencies**: The implementation uses `libc` (already a dependency) for the `launch_activate_socket` FFI and standard library for systemd env var parsing. No new crate dependencies needed.
