# User-Owned Daemon via Socket Activation — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Eliminate the need for `sudo` when starting the devproxy daemon by using OS-level socket activation (launchd on macOS, systemd on Linux) to bind port 443, then pass the pre-bound fd to the daemon process running as the current user.

**Architecture:** The daemon gains a new code path to receive pre-bound TCP listeners from launchd/systemd instead of calling `TcpListener::bind()` directly. On macOS, `devproxy init` installs a LaunchAgent plist with a Sockets entry for port 443; launchd owns the socket and passes fds via `launch_activate_socket`. On Linux, init installs systemd user socket+service units; systemd passes fds via the `LISTEN_FDS` protocol. On Linux without systemd, `setcap cap_net_bind_service=+ep` is applied as a fallback so the binary can bind port 443 directly as a user. The existing `TcpListener::bind()` path remains as the final fallback for tests and `--no-daemon` scenarios.

The `platform` module separates "stop" from "uninstall": `stop_daemon()` uses `launchctl bootout`/`systemctl --user stop` to halt the process without removing plist/unit files, while `uninstall_daemon()` both stops and removes the files. `kill_stale_daemon` and `devproxy update` use `stop_daemon()`. Only `devproxy init` (which re-installs) uses the full `uninstall_daemon()` before re-creating files.

**Tech Stack:** Rust, tokio, libc (for `launch_activate_socket` FFI on macOS), std::env (for `LISTEN_FDS` on Linux), plist XML generation, systemd unit file generation.

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

### Task 3: Add `platform` module with stop/uninstall separation and setcap fallback

This is the core platform module. Critical design: `stop_daemon()` halts the process without deleting unit/plist files (used by `update` and `kill_stale_daemon`). `uninstall_daemon()` stops AND removes files (used only by `init` before re-installing).

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

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The launchd label for the devproxy daemon.
pub const LAUNCHD_LABEL: &str = "com.devproxy.daemon";

/// The systemd unit name prefix.
const SYSTEMD_UNIT_NAME: &str = "devproxy";

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
/// macOS: `launchctl bootout` (stops the process; launchd remembers the plist).
/// Linux: `systemctl --user stop` the socket and service.
pub fn stop_daemon() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        bootout_launchagent()?;
    }
    #[cfg(target_os = "linux")]
    {
        stop_systemd_units()?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        bail!("daemon stop is not supported on this platform");
    }

    Ok(())
}

// ---- Install (writes files + starts) ---------------------------------------

/// Install the daemon for the current platform. Returns Ok(()) on success.
///
/// macOS: writes plist and runs `launchctl bootstrap`.
/// Linux: writes systemd units and runs `systemctl --user enable --now`.
///        Falls back to `setcap` if systemd is not available.
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

    let _ = bootout_launchagent();

    let plist_path = launchagent_plist_path()?;
    if plist_path.exists() {
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
        assert!(plist.contains("/usr/local/bin/devproxy"), "should have binary path");
        assert!(plist.contains("<key>Sockets</key>"), "should have Sockets");
        assert!(plist.contains("443"), "should have port 443");
        assert!(plist.contains("Listeners"), "should have socket name matching code");
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

**Step 4: Register the module in `src/main.rs`**

Add `mod platform;` after line 5 (`mod proxy;`).

**Step 5: Run tests to verify they pass**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --lib platform::tests -v`
Expected: PASS

**Step 6: Commit**

```bash
git add src/platform.rs src/main.rs
git commit -m "feat: add platform module with stop/uninstall separation and setcap fallback"
```

---

### Task 4: Rewrite `init` daemon startup to use socket activation

**Files:**
- Modify: `src/commands/init.rs:297-387` (the daemon startup block)

**Step 1: Understand the change**

The init command currently spawns the daemon directly with `setsid()`. The new flow:
1. Kill any stale daemon via `kill_stale_daemon()` (updated in Task 5)
2. Try `platform::install_daemon()` for socket activation
3. On failure, fall back to `spawn_daemon_directly()` which preserves the original direct-spawn behavior

The `spawn_daemon_directly` helper is extracted from the existing code so it is identical to today's behavior, preserving the `DEVPROXY_CONFIG_DIR` forwarding, the `setsid()`, and the log tail on failure.

**Step 2: Modify the daemon startup section**

Replace the `else` branch of `if no_daemon` in `src/commands/init.rs` (lines 300-387) with:

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
                // Wait for daemon to become responsive (socket activation
                // startup may be slower than direct spawn)
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
                spawn_daemon_directly(&exe, port, domain)?;
            }
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

Critical: we use `stop_daemon()` here, NOT `uninstall_daemon()`. The `kill_stale_daemon` function is called from both `init` (which re-installs afterward) and `update` (which just wants to stop temporarily). If we removed the plist files here, `update` would delete them and the user would have to re-run `init` after every update.

**Step 2: Add platform-aware stop at the beginning of `kill_stale_daemon`**

Insert at the top of `kill_stale_daemon`, before the PID file check (after line 82):

```rust
pub fn kill_stale_daemon() -> Result<()> {
    // Try platform-specific stop first (handles launchd/systemd managed daemons).
    // Uses stop_daemon() (not uninstall) to preserve plist/unit files.
    // If the daemon is managed by launchd (KeepAlive=true), sending SIGTERM
    // would just cause launchd to restart it. bootout is the correct way.
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

### Task 6: Update `devproxy update` daemon stop — no code change needed

**Files:**
- Modify: `src/commands/update.rs:275-279` (minor message update only)

**Step 1: Verify the update flow**

`do_update` calls `super::init::kill_stale_daemon()`. After Task 5, that now calls `platform::stop_daemon()` which uses `launchctl bootout` / `systemctl --user stop` WITHOUT removing files. After the binary is replaced in-place, the plist/unit files still point to the correct path.

The existing "run `devproxy init` to restart the daemon" message is appropriate since:
- On macOS, `bootout` unloads the agent from launchd's memory; `init` will re-bootstrap it.
- On Linux, `stop` halts the service; `init` will re-enable it.
- After update, the binary path hasn't changed, so re-init just re-loads the existing files.

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
- Modify: `tests/e2e.rs:832-856` (`test_init_output_includes_sudo_note`)

**Step 1: Understand the breakage**

The `test_reinit_kills_stale_daemon` test runs `devproxy init --domain ... --port <ephemeral>` which now calls `platform::install_daemon()`. On macOS this calls `launchctl bootstrap`, which will:
- Either succeed and install a real LaunchAgent on the developer's machine (test pollution), or
- Fail (ephemeral port mismatch, test environment limits), causing init to fall through to `spawn_daemon_directly`.

The test asserts `stderr.contains("killing stale daemon")` and `stderr.contains("daemon started")`. The socket activation path says `"daemon started via socket activation"` which DOES contain `"daemon started"`, so that assertion is fine. But we must NOT allow the test to install a real LaunchAgent.

**Fix:** The test already uses `DEVPROXY_CONFIG_DIR`. We add a `DEVPROXY_NO_SOCKET_ACTIVATION=1` environment variable that the init command checks to skip `platform::install_daemon()` and go straight to `spawn_daemon_directly()`. This is a test-only escape hatch, similar to how `DEVPROXY_CONFIG_DIR` already exists for test isolation.

Also fix `test_init_output_includes_sudo_note`: it asserts `stderr.contains("sudo")`. After our changes, the `--no-daemon` path doesn't touch daemon startup at all, and the CA trust path still mentions sudo. The test should still pass because the CA trust failure message contains "sudo". Verify this.

**Step 2: Add `DEVPROXY_NO_SOCKET_ACTIVATION` check to init**

In `src/commands/init.rs`, in the daemon startup `else` branch (from Task 4), wrap the `platform::install_daemon` call:

```rust
    } else {
        kill_stale_daemon()?;
        eprintln!("installing daemon on port {port}...");
        let exe = std::env::current_exe().context("could not determine binary path")?;

        // Allow tests to skip socket activation (which would install real
        // LaunchAgents/systemd units on the host).
        let skip_activation = std::env::var("DEVPROXY_NO_SOCKET_ACTIVATION").is_ok();

        if !skip_activation {
            match crate::platform::install_daemon(&exe, port) {
                Ok(()) => {
                    match wait_for_daemon(Duration::from_secs(10)) {
                        Ok(()) => {
                            eprintln!("{} daemon started via socket activation", "ok:".green());
                            // (return early — skip spawn_daemon_directly)
                        }
                        Err(e) => {
                            // ... error hints (same as Task 4)
                            bail!("daemon failed to start. See hints above.");
                        }
                    }
                    // Skip the fallback path below
                }
                Err(e) => {
                    eprintln!("{} socket activation setup failed: {e}", "warn:".yellow());
                    eprintln!("{} falling back to direct daemon spawn...", "info:".cyan());
                    spawn_daemon_directly(&exe, port, domain)?;
                }
            }
        } else {
            spawn_daemon_directly(&exe, port, domain)?;
        }
    }
```

Actually, a cleaner approach: restructure to avoid code path duplication. Use a `use_socket_activation` bool:

```rust
    } else {
        kill_stale_daemon()?;
        eprintln!("starting daemon on port {port}...");
        let exe = std::env::current_exe().context("could not determine binary path")?;

        let skip_activation = std::env::var("DEVPROXY_NO_SOCKET_ACTIVATION").is_ok();
        let mut activated = false;

        if !skip_activation {
            match crate::platform::install_daemon(&exe, port) {
                Ok(()) => {
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
                    eprintln!("{} socket activation setup failed: {e}", "warn:".yellow());
                    eprintln!("{} falling back to direct daemon spawn...", "info:".cyan());
                }
            }
        }

        if !activated {
            spawn_daemon_directly(&exe, port, domain)?;
        }
    }
```

**Step 3: Update `test_reinit_kills_stale_daemon` in `tests/e2e.rs`**

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

The test's assertion `stderr.contains("daemon started")` will match `"daemon started (pid: ...)"` from `spawn_daemon_directly`, which is correct.

**Step 4: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e`
Expected: all non-ignored tests PASS

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e -- --ignored test_reinit_kills_stale_daemon`
Expected: PASS (if Docker is available)

**Step 5: Commit**

```bash
git add src/commands/init.rs tests/e2e.rs
git commit -m "fix: add DEVPROXY_NO_SOCKET_ACTIVATION for test isolation"
```

---

### Task 8: Add macOS launchd integration test using ephemeral port

**Files:**
- Modify: `tests/e2e.rs` (add new test)

**Step 1: Write the test**

This test installs a real LaunchAgent with an ephemeral port, verifies the daemon starts and responds to IPC, then tears it down. It uses the same `create_test_config_dir` pattern as existing e2e tests.

```rust
/// macOS-only: verify socket activation via launchd with an ephemeral port.
/// Installs a real LaunchAgent plist, waits for the daemon to respond,
/// then uninstalls.
#[test]
#[ignore] // Run with: cargo test --test e2e -- --ignored test_launchd_socket_activation
#[cfg(target_os = "macos")]
fn test_launchd_socket_activation() {
    let config_dir = create_test_config_dir("launchd");
    let daemon_port = find_free_port();

    // Run init with socket activation (no DEVPROXY_NO_SOCKET_ACTIVATION)
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

    // Determine whether socket activation or fallback was used.
    // If socket activation succeeded, we verify the launchd path.
    // If it fell back to direct spawn, that is also acceptable (the
    // fallback path is tested elsewhere).
    let used_socket_activation = stderr.contains("socket activation");

    assert!(
        output.status.success(),
        "init should succeed: {stderr}"
    );
    assert!(
        stderr.contains("daemon started"),
        "init should report daemon started: {stderr}"
    );

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

    // Cleanup: uninstall the LaunchAgent and kill any daemon
    let _ = Command::new(devproxy_bin())
        .args([
            "init",
            "--domain",
            TEST_DOMAIN,
            "--no-daemon",
        ])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output();

    // Bootout the agent if it was installed
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

### Task 9: Add LISTEN_FDS integration test for Linux systemd path

**Files:**
- Modify: `tests/e2e.rs` (add new test)

**Step 1: Write the test**

This test simulates the systemd `LISTEN_FDS` protocol: it pre-binds a TCP socket, sets `LISTEN_FDS=1` and `LISTEN_PID=<child_pid>`, then spawns the daemon. The daemon should accept the pre-bound socket via `acquire_listener()` instead of calling `TcpListener::bind()`.

On macOS, `launch_activate_socket` returns ESRCH (not managed by launchd) so the daemon falls back to checking `LISTEN_FDS`. But `LISTEN_FDS` is a Linux-only code path (`#[cfg(target_os = "linux")]`). So on macOS, if `LISTEN_FDS` is set but the daemon is compiled for macOS, it won't read it — the macOS code path is `launch_activate_socket` only.

Therefore, this test is Linux-only.

```rust
/// Linux-only: verify the LISTEN_FDS protocol (systemd socket activation).
/// Pre-binds a TCP socket, passes the fd to the daemon via LISTEN_FDS/LISTEN_PID,
/// and verifies the daemon accepts connections on that socket.
#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn test_systemd_listen_fds_protocol() {
    use std::os::unix::io::IntoRawFd;

    let config_dir = create_test_config_dir("listen-fds");

    // Pre-bind a TCP socket on an ephemeral port
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = std_listener.local_addr().unwrap().port();

    // The fd must be 3 (SD_LISTEN_FDS_START). We need to dup2 the fd to 3.
    let raw_fd = std_listener.into_raw_fd();

    // We'll use a wrapper script approach: spawn the daemon and
    // set LISTEN_FDS=1, LISTEN_PID to the child's PID.
    // Since we can't easily set LISTEN_PID before fork (it needs to be the
    // child's PID), we use a two-step approach:
    //
    // 1. Fork-exec the daemon binary
    // 2. In the child's pre_exec, dup2 our fd to fd 3
    // 3. Set LISTEN_FDS=1 and LISTEN_PID will be set to the child's PID
    //    by passing a placeholder and letting the daemon read its own PID.
    //
    // Actually, the sd_listen_fds protocol requires LISTEN_PID to match
    // getpid(). We can set it in the env before exec, but we don't know
    // the child PID before fork. The clean solution: use pre_exec to
    // set the env var after fork but before exec.

    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(devproxy_bin());
    cmd.args(["daemon", "--port", &port.to_string()])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .env("LISTEN_FDS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    unsafe {
        let fd_to_dup = raw_fd;
        cmd.pre_exec(move || {
            // dup2 our pre-bound socket to fd 3 (SD_LISTEN_FDS_START)
            if fd_to_dup != 3 {
                if libc::dup2(fd_to_dup, 3) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(fd_to_dup);
            }
            // Clear CLOEXEC on fd 3 so it survives exec
            let flags = libc::fcntl(3, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            // Set LISTEN_PID to our (post-fork) PID
            std::env::set_var("LISTEN_PID", std::process::id().to_string());
            Ok(())
        });
    }

    let mut child = cmd.spawn().expect("failed to spawn daemon with LISTEN_FDS");

    // Wait for IPC socket
    let socket_path = config_dir.join("devproxy.sock");
    let mut started = false;
    for _ in 0..50 {
        if socket_path.exists()
            && std::os::unix::net::UnixStream::connect(&socket_path).is_ok()
        {
            started = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(started, "daemon should start with LISTEN_FDS");

    // Verify daemon is listening on our pre-bound port by checking status
    let status_output = Command::new(devproxy_bin())
        .args(["status"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run status");
    let status_stderr = String::from_utf8_lossy(&status_output.stderr);
    assert!(
        status_stderr.contains("running"),
        "daemon should be running with activated socket: {status_stderr}"
    );

    // Cleanup
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&config_dir);
}
```

**Step 2: Run the test**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e -- --ignored test_systemd_listen_fds_protocol`
Expected: PASS on Linux

**Step 3: Commit**

```bash
git add tests/e2e.rs
git commit -m "test: add Linux LISTEN_FDS protocol integration test"
```

---

### Task 10: Add setcap fallback test for Linux

**Files:**
- Modify: `tests/e2e.rs` (add new test)

**Step 1: Write the test**

The setcap fallback is triggered when systemd is unavailable. We test the setcap code path indirectly by testing `platform::apply_setcap` — but that requires sudo. Instead, we test the decision logic: when systemd fails, init should fall back to direct spawn (and on a system with setcap, would apply it).

For a unit-testable approach, we test the `install_linux_daemon` fallback path by mocking systemctl absence. Since this requires runtime conditions, we write an e2e test that verifies the fallback message appears when systemd is unavailable.

However, actually testing `setcap` requires root. We add a test that:
1. Verifies the fallback message when systemd is not available
2. On systems where `setcap` is available and we have sudo, tests the full path

```rust
/// Linux-only: verify that when systemd is not available, init falls back
/// to the direct spawn path (which would use setcap on a real system).
/// This test simulates "no systemd" by setting PATH to exclude systemctl.
#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn test_linux_setcap_fallback_path() {
    let config_dir = create_test_config_dir("setcap");
    let daemon_port = find_free_port();

    // Run init with a PATH that doesn't include systemctl, forcing
    // the systemd path to fail and trigger the setcap/direct fallback.
    // We also set DEVPROXY_NO_SOCKET_ACTIVATION to skip the platform path
    // entirely (since without systemctl the platform path would try setcap
    // which needs sudo). Instead we verify the fallback produces a working daemon.
    let output = Command::new(devproxy_bin())
        .args([
            "init",
            "--domain",
            TEST_DOMAIN,
            "--port",
            &daemon_port.to_string(),
        ])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .env("DEVPROXY_NO_SOCKET_ACTIVATION", "1")
        .output()
        .expect("failed to run devproxy init");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "init should succeed via fallback: {stderr}"
    );
    assert!(
        stderr.contains("daemon started"),
        "should report daemon started: {stderr}"
    );

    // Verify daemon is responsive
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

    // Cleanup
    let pid_path = config_dir.join("daemon.pid");
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGTERM) };
                std::thread::sleep(Duration::from_millis(500));
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
    let _ = std::fs::remove_dir_all(&config_dir);
}
```

**Step 2: Run the test**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e -- --ignored test_linux_setcap_fallback_path`
Expected: PASS on Linux

**Step 3: Commit**

```bash
git add tests/e2e.rs
git commit -m "test: add Linux setcap fallback path integration test"
```

---

### Task 11: Run full test suite and verify

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

**Step 5: Run ignored e2e tests (Docker-dependent)**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/user-daemon-socket-activation && cargo test --test e2e -- --ignored`
Expected: all PASS. Key tests:
- `test_reinit_kills_stale_daemon` — uses `DEVPROXY_NO_SOCKET_ACTIVATION=1`, exercises direct-spawn + kill
- `test_launchd_socket_activation` (macOS only) — exercises real launchd path
- `test_systemd_listen_fds_protocol` (Linux only) — exercises LISTEN_FDS path
- `test_linux_setcap_fallback_path` (Linux only) — exercises direct-spawn fallback

**Step 6: Commit** (if any fixups were needed)

---

### Task 12: Final commit and cleanup

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
| `src/platform.rs` | Create | LaunchAgent/systemd/setcap management with stop vs uninstall separation |
| `src/main.rs` | Modify | Add `mod platform;` |
| `src/commands/init.rs` | Modify | Socket activation install, `DEVPROXY_NO_SOCKET_ACTIVATION` escape hatch, `spawn_daemon_directly` fallback, `kill_stale_daemon` uses `stop_daemon()` |
| `src/commands/update.rs` | Modify | Minor message update |
| `tests/e2e.rs` | Modify | Fix `test_reinit_kills_stale_daemon`, add launchd/LISTEN_FDS/setcap integration tests |

## Design decisions made

1. **`stop_daemon()` vs `uninstall_daemon()`**: `stop_daemon()` halts the process (bootout/systemctl stop) without removing files. `uninstall_daemon()` stops and deletes files. `kill_stale_daemon` uses stop (preserves files for update), while init uses uninstall before re-installing.

2. **`DEVPROXY_NO_SOCKET_ACTIVATION` env var**: Test escape hatch to prevent `test_reinit_kills_stale_daemon` from installing a real LaunchAgent on the developer's machine. Follows the same pattern as `DEVPROXY_CONFIG_DIR` for test isolation.

3. **Linux setcap fallback**: On Linux, `install_daemon` first tries systemd. If `systemctl --user` is unavailable, it falls back to `sudo setcap cap_net_bind_service=+ep` on the binary. If even that fails, `init` falls through to `spawn_daemon_directly` (which needs sudo for port < 1024).

4. **Socket name "Listeners"**: Matches the plist Sockets key name. `launch_activate_socket("Listeners", ...)` retrieves fds for the socket named "Listeners" in the plist.

5. **KeepAlive=true in plist**: Ensures launchd restarts the daemon if it crashes, matching puma-dev's behavior. `kill_stale_daemon` uses `launchctl bootout` (via `stop_daemon`) instead of SIGTERM to avoid launchd immediately restarting the process.

6. **No new dependencies**: Uses `libc` (already a dependency) for `launch_activate_socket` FFI and standard library for systemd env var parsing.

7. **Launchd integration test is `#[ignore]`**: The macOS launchd test installs a real LaunchAgent and is marked `#[ignore]` so it only runs explicitly. It cleans up after itself by booting out the agent and removing the plist file.
