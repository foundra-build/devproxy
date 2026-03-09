use crate::config::Config;
use crate::proxy::cert;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::time::Duration;

/// Validate that the domain looks reasonable: non-empty, contains only valid
/// DNS characters, has at least one dot, and each label is 1-63 chars.
fn validate_domain(domain: &str) -> Result<()> {
    if domain.is_empty() {
        bail!("domain must not be empty");
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        bail!("domain '{domain}' must have at least two labels (e.g. 'mysite.dev')");
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            bail!("domain label '{label}' must be 1-63 characters");
        }
        if !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            bail!("domain label '{label}' contains invalid characters (only a-z, 0-9, - allowed)");
        }
        if label.starts_with('-') || label.ends_with('-') {
            bail!("domain label '{label}' must not start or end with a hyphen");
        }
    }
    Ok(())
}

/// Check whether the process at `pid` is a devproxy process by inspecting
/// its command line. Returns false if we cannot determine (e.g., process
/// belongs to another user) -- we err on the side of not killing.
fn is_devproxy_process(pid: i32) -> bool {
    #[cfg(target_os = "macos")]
    {
        // On macOS, use `ps -p <pid> -o comm=` to get the process name.
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let comm = String::from_utf8_lossy(&out.stdout);
                // Exact match on basename to avoid false positives (e.g., "my-devproxy")
                comm.trim()
                    .rsplit('/')
                    .next()
                    .map(|name| name == "devproxy")
                    .unwrap_or(false)
            }
            _ => false,
        }
    }
    #[cfg(target_os = "linux")]
    {
        // On Linux, read /proc/<pid>/exe symlink.
        let exe = std::fs::read_link(format!("/proc/{pid}/exe"));
        match exe {
            Ok(path) => path
                .file_name()
                .map(|n| n == "devproxy")
                .unwrap_or(false),
            Err(_) => false,
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        false
    }
}

/// Kill a stale daemon process. Reads PID from the PID file, validates it
/// is actually a devproxy process (to avoid PID reuse races), then sends
/// SIGTERM/SIGKILL. Also removes stale socket and PID files.
///
/// # Residual TOCTOU race
///
/// There is a small window between `is_devproxy_process(pid)` returning true
/// and the SIGTERM being sent where the process could die and the OS could
/// recycle the PID. In practice this is extremely unlikely: PIDs cycle through
/// a large range and the window is microseconds. The process-name check makes
/// accidental kills vanishingly improbable -- the recycled PID would also need
/// to be named "devproxy".
fn kill_stale_daemon() -> Result<()> {
    let pid_path = Config::pid_path()?;
    let socket_path = Config::socket_path()?;

    if pid_path.exists() {
        let pid_str = std::fs::read_to_string(&pid_path).unwrap_or_default();
        let trimmed = pid_str.trim();
        if trimmed.is_empty() {
            eprintln!(
                "{} PID file is empty, cleaning up stale files",
                "warn:".yellow()
            );
        } else if let Ok(pid_u32) = trimmed.parse::<u32>() {
            // PID 0 would signal the entire process group via libc::kill,
            // and values > i32::MAX would wrap negative when cast to i32
            // (which libc::kill interprets as a process group ID).
            if pid_u32 == 0 || pid_u32 > i32::MAX as u32 {
                eprintln!(
                    "{} invalid PID {pid_u32} in PID file, cleaning up stale files",
                    "warn:".yellow()
                );
                let _ = std::fs::remove_file(&pid_path);
                if socket_path.exists() {
                    let _ = std::fs::remove_file(&socket_path);
                }
                return Ok(());
            }
            let pid = pid_u32 as i32;

            // Check if process is alive. kill(pid, 0) returns:
            //   0    — process exists and we can signal it
            //  -1/EPERM — process exists but owned by another user
            //  -1/ESRCH — process does not exist (PID is stale)
            let ret = unsafe { libc::kill(pid, 0) };
            if ret == 0 {
                // Process is alive and we can signal it
                // Verify this is actually a devproxy process, not a recycled PID
                if !is_devproxy_process(pid) {
                    eprintln!(
                        "{} PID {pid} is no longer a devproxy process (PID was recycled), cleaning up stale files",
                        "info:".cyan()
                    );
                    // Fall through to file cleanup
                } else {
                    eprintln!(
                        "{} killing stale daemon (pid: {pid})...",
                        "info:".cyan()
                    );
                    let mut killed = false;
                    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!(
                            "{} SIGTERM failed for pid {pid}: {err}",
                            "warn:".yellow()
                        );
                        // EPERM means we can't signal it -- bail with guidance
                        if err.raw_os_error() == Some(libc::EPERM) {
                            bail!(
                                "cannot kill stale daemon (pid: {pid}): permission denied. \
                                 Try: sudo kill {pid}"
                            );
                        }
                    } else {
                        // SIGTERM succeeded -- wait briefly for graceful shutdown
                        std::thread::sleep(Duration::from_millis(500));
                        if unsafe { libc::kill(pid, 0) } != 0 {
                            // Process is gone after SIGTERM
                            killed = true;
                        } else if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
                            // SIGKILL succeeded
                            std::thread::sleep(Duration::from_millis(200));
                            killed = true;
                        } else {
                            let err = std::io::Error::last_os_error();
                            eprintln!(
                                "{} SIGKILL failed for pid {pid}: {err}",
                                "warn:".yellow()
                            );
                        }
                    }
                    if killed {
                        eprintln!("{} stale daemon killed", "ok:".green());
                    } else {
                        eprintln!(
                            "{} could not confirm daemon (pid: {pid}) was killed",
                            "warn:".yellow()
                        );
                        // Don't remove PID file -- the process may still be alive.
                        // Leave it for manual cleanup.
                        return Ok(());
                    }
                }
            } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
                // Process exists but owned by another user (e.g., root)
                bail!(
                    "stale daemon (pid: {pid}) is running but owned by another user. \
                     Kill it first with: sudo kill {pid}"
                );
            }
            // else: process does not exist (ESRCH) -- fall through to file cleanup
        } else {
            eprintln!(
                "{} PID file contains unparseable content '{}', cleaning up stale files",
                "warn:".yellow(),
                trimmed,
            );
        }
        let _ = std::fs::remove_file(&pid_path);
    }

    // Clean up stale socket
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    Ok(())
}

/// Wait for the daemon to become responsive by polling the IPC socket
/// with an actual ping/pong exchange. Returns Ok(()) if the daemon
/// responds to a Ping within the timeout, or an error if it doesn't.
fn wait_for_daemon(timeout: Duration) -> Result<()> {
    let socket_path = Config::socket_path()?;
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(100);

    while start.elapsed() < timeout {
        if socket_path.exists()
            && crate::ipc::ping_sync(&socket_path, Duration::from_secs(1))
        {
            return Ok(());
        }
        std::thread::sleep(poll_interval);
    }

    bail!(
        "daemon did not start within {}s. Check if port is available and you have permissions.",
        timeout.as_secs()
    )
}

pub fn run(domain: &str, port: u16, no_daemon: bool) -> Result<()> {
    validate_domain(domain)?;
    let config = Config { domain: domain.to_string() };

    // Create config directory
    let config_dir = Config::config_dir()?;
    std::fs::create_dir_all(&config_dir)?;

    // Generate CA if it doesn't exist
    let ca_cert_path = Config::ca_cert_path()?;
    let ca_key_path = Config::ca_key_path()?;

    // Track whether CA trust needs manual action, so we can conditionally
    // print trust instructions in "Next steps".
    let mut ca_trust_needed = false;

    if ca_cert_path.exists() && ca_key_path.exists() {
        eprintln!("{} CA certificate already exists", "ok:".green());
    } else {
        eprintln!("generating CA certificate...");
        let (ca_cert_pem, ca_key_pem) = cert::generate_ca()?;
        cert::write_pem(&ca_cert_path, &ca_cert_pem)?;
        cert::write_key_pem(&ca_key_path, &ca_key_pem)?;
        eprintln!("{} CA certificate generated", "ok:".green());

        // Trust the CA
        eprintln!("trusting CA in system keychain (requires sudo)...");
        match cert::trust_ca_in_system(&ca_cert_path) {
            Ok(()) => eprintln!("{} CA trusted in system keychain", "ok:".green()),
            Err(e) => {
                ca_trust_needed = true;
                eprintln!(
                    "{} could not trust CA automatically: {e}",
                    "warn:".yellow()
                );
                eprintln!(
                    "  run manually with sudo:"
                );
                #[cfg(target_os = "macos")]
                eprintln!(
                    "    sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
                    ca_cert_path.display()
                );
                #[cfg(target_os = "linux")]
                eprintln!(
                    "    sudo cp {} /usr/local/share/ca-certificates/devproxy-ca.crt && sudo update-ca-certificates",
                    ca_cert_path.display()
                );
            }
        }
    }

    // Generate wildcard cert if it doesn't exist or if the domain has changed
    let tls_cert_path = Config::tls_cert_path()?;
    let tls_key_path = Config::tls_key_path()?;

    // Detect domain change: if an existing config has a different domain,
    // the wildcard cert needs to be regenerated for the new domain.
    let domain_changed = Config::load()
        .ok()
        .map(|existing| existing.domain != domain)
        .unwrap_or(false);

    if domain_changed && tls_cert_path.exists() {
        eprintln!(
            "{} domain changed, regenerating TLS certificate...",
            "info:".cyan()
        );
    }

    if tls_cert_path.exists() && tls_key_path.exists() && !domain_changed {
        eprintln!("{} TLS certificate already exists", "ok:".green());
    } else {
        let ca_cert_pem = std::fs::read_to_string(&ca_cert_path)?;
        let ca_key_pem = std::fs::read_to_string(&ca_key_path)?;

        eprintln!("generating wildcard TLS certificate for *.{domain}...");
        let (tls_cert_pem, tls_key_pem) = cert::generate_wildcard_cert(domain, &ca_cert_pem, &ca_key_pem)?;
        cert::write_pem(&tls_cert_path, &tls_cert_pem)?;
        cert::write_key_pem(&tls_key_path, &tls_key_pem)?;
        eprintln!("{} TLS certificate generated", "ok:".green());
    }

    // Save config
    config.save()?;
    eprintln!("{} config saved", "ok:".green());

    // Start daemon (unless --no-daemon)
    if no_daemon {
        eprintln!("{} daemon spawn skipped (--no-daemon)", "ok:".green());
    } else {
        // Kill any stale daemon from a previous init
        kill_stale_daemon()?;

        if port < 1024 {
            eprintln!(
                "{} port {port} requires root privileges (sudo)",
                "info:".cyan()
            );
        }

        eprintln!("starting daemon on port {port}...");
        let exe = std::env::current_exe().context("could not determine binary path")?;

        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["daemon", "--port", &port.to_string()]);

        // Forward DEVPROXY_CONFIG_DIR so the daemon uses the same config dir
        if let Ok(dir) = std::env::var("DEVPROXY_CONFIG_DIR") {
            cmd.env("DEVPROXY_CONFIG_DIR", dir);
        }

        // Use pre_exec to call setsid() so the daemon runs in its own
        // session and is fully detached from the parent process.
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        // Capture daemon stderr to a log file for debugging startup failures.
        // Append mode preserves the previous daemon's log for post-mortem debugging.
        let daemon_log_path = Config::daemon_log_path()?;
        let daemon_log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&daemon_log_path)
            .with_context(|| format!("could not open daemon log at {}", daemon_log_path.display()))?;

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(daemon_log_file));

        let mut child = cmd.spawn().context("could not spawn daemon")?;
        let pid = child.id();
        // Spawn a thread to reap the child so it does not become a zombie.
        // After setsid(), the child won't receive signals when the parent exits.
        std::thread::spawn(move || { let _ = child.wait(); });

        // Wait for daemon to become responsive (or fail fast)
        match wait_for_daemon(Duration::from_secs(5)) {
            Ok(()) => {
                eprintln!("{} daemon started (pid: {pid})", "ok:".green());
            }
            Err(e) => {
                eprintln!(
                    "{} daemon failed to start: {e}",
                    "error:".red()
                );
                // Show last few lines of daemon log to help diagnose startup failures
                if let Ok(log_contents) = std::fs::read_to_string(&daemon_log_path) {
                    let last_lines: Vec<&str> = log_contents.lines().rev().take(10).collect();
                    if !last_lines.is_empty() {
                        eprintln!("  {} daemon log ({}):", "log:".cyan(), daemon_log_path.display());
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
    }

    eprintln!();
    eprintln!("{}", "Setup complete!".green().bold());
    eprintln!();
    eprintln!("Next steps:");
    eprintln!();

    // DNS setup instructions
    eprintln!("  {} Set up wildcard DNS for *.{domain} -> 127.0.0.1", "1.".bold());
    #[cfg(target_os = "macos")]
    {
        eprintln!();
        eprintln!("     Install dnsmasq (if not already installed):");
        eprintln!("       brew install dnsmasq");
        eprintln!("       sudo brew services start dnsmasq");
        eprintln!();
        eprintln!("     Add wildcard DNS rule:");
        eprintln!("       echo 'address=/.{domain}/127.0.0.1' >> $(brew --prefix)/etc/dnsmasq.conf");
        eprintln!("       sudo brew services restart dnsmasq");
        eprintln!();
        // Extract the TLD for the resolver. rsplit('.').next() always
        // returns Some (the last segment, or the whole string if no dot),
        // but we already validated the domain has at least two labels above.
        let tld = domain.rsplit('.').next().expect("validated domain has a dot");
        eprintln!("     Create resolver for .{tld} domains:");
        eprintln!("       sudo mkdir -p /etc/resolver");
        eprintln!("       echo 'nameserver 127.0.0.1' | sudo tee /etc/resolver/{tld}");
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("     Example: echo 'address=/.{domain}/127.0.0.1' >> /etc/dnsmasq.conf");
    }
    eprintln!();

    // CA trust reminder -- only shown if trust was not already successful
    let mut step = 2;
    if ca_trust_needed {
        eprintln!("  {} Trust the CA certificate", format!("{step}.").bold());
        eprintln!("     CA cert: {}", ca_cert_path.display().to_string().cyan());
        #[cfg(target_os = "macos")]
        eprintln!(
            "     sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
            ca_cert_path.display()
        );
        #[cfg(target_os = "linux")]
        eprintln!(
            "     sudo cp {} /usr/local/share/ca-certificates/devproxy-ca.crt && sudo update-ca-certificates",
            ca_cert_path.display()
        );
        eprintln!();
        step += 1;
    }

    // Project setup
    eprintln!("  {} Add a devproxy.port label to your docker-compose.yml", format!("{step}.").bold());
    eprintln!();
    eprintln!("  {} Run: devproxy up", format!("{}.", step + 1).bold());

    Ok(())
}
