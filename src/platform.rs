//! Platform-specific daemon installation and management.
//!
//! macOS: LaunchAgent plist with socket activation (like puma-dev).
//! Linux: systemd user socket + service units, with setcap fallback.
//!
//! Key operations:
//! - `install_daemon()` writes plist/unit files and starts the daemon.
//! - `stop_daemon()` halts the running daemon without removing files
//!   (used by kill_stale_daemon). Returns `Ok(true)` if it actually stopped
//!   something, `Ok(false)` if there was nothing to stop.
//! - `restart_daemon()` atomically restarts the daemon in-place (used by update).
//!
//! All public functions respect `DEVPROXY_NO_SOCKET_ACTIVATION` for test isolation
//! and check for file existence before touching global system state.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The launchd label for the devproxy daemon.
pub const LAUNCHD_LABEL: &str = "com.devproxy.daemon";

/// The systemd unit name prefix.
/// Used on Linux for unit file paths and systemctl commands.
/// On macOS, referenced only by tests for the generation functions.
#[allow(dead_code)]
const SYSTEMD_UNIT_NAME: &str = "devproxy";

/// Returns true if socket activation is disabled via env var.
/// Used for test isolation: prevents tests from touching real
/// LaunchAgents/systemd units on the host.
pub fn is_socket_activation_disabled() -> bool {
    std::env::var("DEVPROXY_NO_SOCKET_ACTIVATION").is_ok()
}

// ---- Plist / unit file generation ------------------------------------------

/// Escape a string for safe inclusion in XML text content.
/// Handles the three characters that are special in XML text nodes.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Generate the LaunchAgent plist XML for macOS.
/// The plist uses Sockets to have launchd bind the port and pass the fd.
/// If `config_dir` is Some, an `EnvironmentVariables` dict is included
/// so the daemon uses the specified config directory instead of the default.
pub fn generate_launchagent_plist(
    binary_path: &str,
    port: u16,
    config_dir: Option<&str>,
) -> String {
    let binary_path = xml_escape(binary_path);
    let config_dir_escaped = config_dir.map(xml_escape);

    // launchd provides a minimal PATH (/usr/bin:/bin:/usr/sbin:/sbin) that
    // excludes /usr/local/bin (Docker on macOS) and ~/.local/bin. Include
    // the common paths so the daemon can find docker, etc.
    let path_value = std::env::var("PATH")
        .unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    let path_value = xml_escape(&path_value);

    let mut env_entries = format!(
        r#"        <key>PATH</key>
        <string>{path_value}</string>
"#
    );
    if let Some(dir) = &config_dir_escaped {
        env_entries.push_str(&format!(
            r#"        <key>DEVPROXY_CONFIG_DIR</key>
        <string>{dir}</string>
"#
        ));
    }

    let env_block = format!(
        r#"    <key>EnvironmentVariables</key>
    <dict>
{env_entries}    </dict>
"#
    );

    // Use the same log path as Config::daemon_log_path() — inside the config dir.
    // When config_dir is overridden, log goes there; otherwise use the default.
    let log_path = xml_escape(&match config_dir {
        Some(dir) => format!("{dir}/daemon.log"),
        None => {
            // Match the default from Config::config_dir() -> ~/.config/devproxy/
            dirs::home_dir()
                .map(|h| {
                    h.join(".config/devproxy/daemon.log")
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_else(|| "/tmp/devproxy-daemon.log".to_string())
        }
    });

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
        <string>run</string>
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
{env_block}    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>{log_path}</string>
    <key>StandardOutPath</key>
    <string>/dev/null</string>
</dict>
</plist>
"#
    )
}

/// Generate a systemd .socket unit for Linux.
/// Binds to 127.0.0.1 only — never expose the dev proxy to the network.
#[allow(dead_code)]
pub fn generate_systemd_socket_unit(port: u16) -> String {
    format!(
        r#"[Unit]
Description=devproxy HTTPS socket

[Socket]
ListenStream=127.0.0.1:{port}

[Install]
WantedBy=sockets.target
"#
    )
}

/// Generate a systemd .service unit for Linux.
/// Includes `--port` so that if socket activation fails and the daemon
/// falls back to `TcpListener::bind`, it binds the correct port.
/// If `config_dir` is Some, an `Environment=` directive is included.
#[allow(dead_code)]
pub fn generate_systemd_service_unit(
    binary_path: &str,
    port: u16,
    config_dir: Option<&str>,
) -> String {
    let env_line = match config_dir {
        Some(dir) => format!("Environment=\"DEVPROXY_CONFIG_DIR={dir}\"\n"),
        None => String::new(),
    };

    format!(
        r#"[Unit]
Description=devproxy HTTPS reverse proxy daemon
Requires={SYSTEMD_UNIT_NAME}.socket
After={SYSTEMD_UNIT_NAME}.socket

[Service]
Type=simple
{env_line}ExecStart="{binary_path}" daemon run --port {port}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#
    )
}

// ---- Path helpers ----------------------------------------------------------

/// Path to the LaunchAgent plist file.
#[cfg(target_os = "macos")]
pub fn launchagent_plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
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
///
/// Returns `Ok(true)` if a platform-managed daemon was actually stopped,
/// `Ok(false)` if there was nothing to stop (no plist/unit files, or
/// socket activation disabled). Callers can use this to skip unnecessary
/// post-stop delays.
pub fn stop_daemon() -> Result<bool> {
    if is_socket_activation_disabled() {
        return Ok(false);
    }

    #[cfg(target_os = "macos")]
    {
        // Only bootout if we know we installed a plist
        let plist_path = launchagent_plist_path()?;
        if plist_path.exists() {
            bootout_launchagent()?;
            return Ok(true);
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Only stop if unit files exist
        let unit_dir = systemd_user_dir()?;
        if unit_dir
            .join(format!("{SYSTEMD_UNIT_NAME}.socket"))
            .exists()
        {
            stop_systemd_units()?;
            return Ok(true);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // No-op on unsupported platforms (don't bail — caller handles fallback)
    }

    Ok(false)
}

// ---- Restart (stop + start without re-installing) --------------------------

/// Restart a platform-managed daemon in-place. Used by `devproxy update`
/// after replacing the binary: the plist/unit files still point to the
/// same path, so we just need to restart the process.
///
/// macOS: `launchctl kickstart -k` kills and restarts the agent in one step.
/// Linux: `systemctl --user restart` restarts the service via its socket.
///
/// Respects `DEVPROXY_NO_SOCKET_ACTIVATION` for test isolation.
/// Returns Ok(false) if no platform-managed daemon was found to restart.
pub fn restart_daemon() -> Result<bool> {
    if is_socket_activation_disabled() {
        return Ok(false);
    }

    #[cfg(target_os = "macos")]
    {
        let plist_path = launchagent_plist_path()?;
        if plist_path.exists() {
            let uid = unsafe { libc::getuid() };
            let status = std::process::Command::new("launchctl")
                .args(["kickstart", "-k", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
                .status()
                .context("failed to run launchctl kickstart")?;
            if !status.success() {
                bail!(
                    "launchctl kickstart failed (exit {})",
                    status.code().unwrap_or(-1)
                );
            }
            return Ok(true);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let unit_dir = systemd_user_dir()?;
        if unit_dir
            .join(format!("{SYSTEMD_UNIT_NAME}.socket"))
            .exists()
        {
            let status = std::process::Command::new("systemctl")
                .args(["--user", "restart", &format!("{SYSTEMD_UNIT_NAME}.service")])
                .status()
                .context("failed to run systemctl --user restart")?;
            if !status.success() {
                bail!("systemctl --user restart failed");
            }
            return Ok(true);
        }
    }

    Ok(false)
}

// ---- Install (writes files + starts) ---------------------------------------

/// Install the daemon for the current platform. Returns Ok(()) on success.
///
/// Respects `DEVPROXY_NO_SOCKET_ACTIVATION` — returns Err so caller
/// falls through to `spawn_daemon_directly`.
///
/// `config_dir` is an optional override for `DEVPROXY_CONFIG_DIR`. When
/// `Some`, it is embedded in the plist/unit file so the daemon uses the
/// specified directory. Pass `None` for the default (`~/.config/devproxy/`).
///
/// macOS: writes plist and runs `launchctl bootstrap`.
/// Linux: writes systemd units and runs `systemctl --user enable --now`.
///        Falls back to `setcap` if systemd is not available.
pub fn install_daemon(binary_path: &Path, port: u16, config_dir: Option<&str>) -> Result<()> {
    if is_socket_activation_disabled() {
        bail!("socket activation disabled via DEVPROXY_NO_SOCKET_ACTIVATION");
    }

    let binary_str = binary_path
        .to_str()
        .context("binary path is not valid UTF-8")?;

    #[cfg(target_os = "macos")]
    {
        install_launchagent(binary_str, port, config_dir)?;
    }
    #[cfg(target_os = "linux")]
    {
        install_linux_daemon(binary_str, binary_path, port, config_dir)?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (binary_str, port, config_dir);
        bail!("daemon installation is not supported on this platform");
    }

    Ok(())
}

// ---- macOS launchd ---------------------------------------------------------

#[cfg(target_os = "macos")]
fn bootout_launchagent() -> Result<()> {
    let uid = unsafe { libc::getuid() };
    // Suppress stderr — bootout failing is expected on fresh install
    // (agent not loaded) and during install's pre-bootstrap cleanup.
    let status = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
        .stderr(std::process::Stdio::null())
        .status()
        .context("failed to run launchctl bootout")?;

    if !status.success() {
        // Not fatal — agent may not be loaded. Callers handle this.
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_launchagent(binary_path: &str, port: u16, config_dir: Option<&str>) -> Result<()> {
    use colored::Colorize;

    let plist_path = launchagent_plist_path()?;
    let plist_dir = plist_path.parent().context("plist path has no parent")?;

    std::fs::create_dir_all(plist_dir)
        .with_context(|| format!("could not create {}", plist_dir.display()))?;

    // Silently try to bootout any existing agent. This is a no-op if
    // kill_stale_daemon already booted it out — we just ensure the
    // launchd session is clean before bootstrap. No warning on failure
    // (the agent may not be loaded, which is expected).
    let _ = bootout_launchagent();

    let plist_content = generate_launchagent_plist(binary_path, port, config_dir);
    std::fs::write(&plist_path, &plist_content)
        .with_context(|| format!("could not write plist at {}", plist_path.display()))?;
    eprintln!("{} wrote {}", "ok:".green(), plist_path.display());

    // Bootstrap the agent (loads and starts it)
    let uid = unsafe { libc::getuid() };
    let status = std::process::Command::new("launchctl")
        .args([
            "bootstrap",
            &format!("gui/{uid}"),
            &plist_path.to_string_lossy(),
        ])
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

// ---- Linux: systemd preferred, setcap fallback -----------------------------

#[cfg(target_os = "linux")]
fn install_linux_daemon(
    binary_str: &str,
    binary_path: &Path,
    port: u16,
    config_dir: Option<&str>,
) -> Result<()> {
    // Try systemd first
    match install_systemd_units(binary_str, port, config_dir) {
        Ok(()) => return Ok(()),
        Err(e) => {
            use colored::Colorize;
            eprintln!("{} systemd setup failed: {e}", "info:".cyan());
            eprintln!("{} trying setcap fallback...", "info:".cyan());
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
fn install_systemd_units(binary_path: &str, port: u16, config_dir: Option<&str>) -> Result<()> {
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

    std::fs::write(
        &service_path,
        generate_systemd_service_unit(binary_path, port, config_dir),
    )
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
        .args([
            "--user",
            "enable",
            "--now",
            &format!("{SYSTEMD_UNIT_NAME}.socket"),
        ])
        .status()
        .context("failed to run systemctl --user enable")?;

    if !enable.success() {
        bail!("systemctl --user enable --now {SYSTEMD_UNIT_NAME}.socket failed");
    }

    eprintln!(
        "{} systemd socket unit installed and enabled",
        "ok:".green()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn stop_systemd_units() -> Result<()> {
    // Stop without disabling or removing files.
    // Log warnings on failure but don't bail — the caller will fall back
    // to signal-based kill if the daemon is still alive.
    for unit in [
        format!("{SYSTEMD_UNIT_NAME}.service"),
        format!("{SYSTEMD_UNIT_NAME}.socket"),
    ] {
        match std::process::Command::new("systemctl")
            .args(["--user", "stop", &unit])
            .status()
        {
            Ok(status) if !status.success() => {
                eprintln!(
                    "  warn: systemctl --user stop {unit} exited {}",
                    status.code().unwrap_or(-1)
                );
            }
            Err(e) => {
                eprintln!("  warn: failed to run systemctl --user stop {unit}: {e}");
            }
            _ => {}
        }
    }
    Ok(())
}

// ---- Query helpers ---------------------------------------------------------

/// Check if the daemon is managed by a platform service manager
/// (launchd plist exists on macOS, systemd unit exists on Linux).
/// Returns false when socket activation is disabled (test isolation).
pub fn is_managed() -> bool {
    if is_socket_activation_disabled() {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        launchagent_plist_path()
            .map(|p| p.exists())
            .unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    {
        systemd_user_dir()
            .map(|d| d.join("devproxy.socket").exists())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launchagent_plist_contains_required_fields() {
        let plist = generate_launchagent_plist("/usr/local/bin/devproxy", 443, None);
        assert!(plist.contains("com.devproxy.daemon"), "should have label");
        assert!(
            plist.contains("/usr/local/bin/devproxy"),
            "should have binary path"
        );
        assert!(plist.contains("<key>Sockets</key>"), "should have Sockets");
        assert!(plist.contains("443"), "should have port 443");
        assert!(
            plist.contains("Listeners"),
            "should have socket name matching code"
        );
        assert!(plist.contains("127.0.0.1"), "should bind to localhost only");
        assert!(plist.contains("<string>run</string>"), "should have run subcommand");
        assert!(
            plist.contains("EnvironmentVariables"),
            "should have env vars (at least PATH)"
        );
        assert!(plist.contains("<key>PATH</key>"), "should have PATH");
        assert!(
            !plist.contains("DEVPROXY_CONFIG_DIR"),
            "should not have DEVPROXY_CONFIG_DIR when config_dir is None"
        );
        assert!(
            plist.contains("daemon.log"),
            "should have a log path in StandardErrorPath"
        );
        assert!(
            !plist.contains("/tmp/devproxy-daemon.log"),
            "should not use hardcoded /tmp log path"
        );
    }

    #[test]
    fn test_launchagent_plist_with_config_dir() {
        let plist =
            generate_launchagent_plist("/usr/local/bin/devproxy", 443, Some("/tmp/test-config"));
        assert!(
            plist.contains("EnvironmentVariables"),
            "should have env vars"
        );
        assert!(plist.contains("<key>PATH</key>"), "should have PATH");
        assert!(
            plist.contains("DEVPROXY_CONFIG_DIR"),
            "should have config dir key"
        );
        assert!(
            plist.contains("/tmp/test-config"),
            "should have config dir value"
        );
        assert!(
            plist.contains("/tmp/test-config/daemon.log"),
            "should use config-dir-based log path"
        );
    }

    #[test]
    fn test_launchagent_plist_custom_port() {
        let plist = generate_launchagent_plist("/opt/devproxy", 8443, None);
        assert!(plist.contains("8443"), "should use custom port");
        assert!(
            plist.contains("/opt/devproxy"),
            "should use custom binary path"
        );
    }

    #[test]
    fn test_systemd_socket_unit_binds_localhost() {
        let unit = generate_systemd_socket_unit(443);
        assert!(
            unit.contains("ListenStream=127.0.0.1:443"),
            "should listen on localhost:443"
        );
        assert!(unit.contains("[Socket]"), "should have Socket section");
    }

    #[test]
    fn test_systemd_socket_unit_custom_port() {
        let unit = generate_systemd_socket_unit(8443);
        assert!(
            unit.contains("ListenStream=127.0.0.1:8443"),
            "should use custom port on localhost"
        );
    }

    #[test]
    fn test_systemd_service_unit_contains_binary_and_port() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy", 443, None);
        assert!(
            unit.contains("/usr/local/bin/devproxy"),
            "should have binary path"
        );
        assert!(
            unit.contains("daemon run --port 443"),
            "should run daemon run subcommand with port"
        );
        assert!(unit.contains("Type=simple"), "should have Type=simple");
        assert!(
            !unit.contains("Environment="),
            "should not have Environment when config_dir is None"
        );
    }

    #[test]
    fn test_systemd_service_unit_custom_port() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy", 8443, None);
        assert!(
            unit.contains("daemon run --port 8443"),
            "should use custom port in ExecStart"
        );
    }

    #[test]
    fn test_systemd_service_unit_with_config_dir() {
        let unit =
            generate_systemd_service_unit("/usr/local/bin/devproxy", 443, Some("/tmp/test-config"));
        assert!(
            unit.contains("Environment=\"DEVPROXY_CONFIG_DIR=/tmp/test-config\""),
            "should have config dir env"
        );
    }

    #[test]
    fn test_systemd_service_references_socket() {
        let unit = generate_systemd_service_unit("/usr/local/bin/devproxy", 443, None);
        assert!(
            unit.contains("Requires=devproxy.socket"),
            "should require socket unit"
        );
    }

    #[test]
    fn test_is_socket_activation_disabled_default() {
        // In normal test runs, env var should not be set
        // (unless the runner explicitly sets it, which is fine)
        // This test just verifies the function doesn't panic
        let _ = is_socket_activation_disabled();
    }
}
