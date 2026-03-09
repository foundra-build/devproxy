use crate::config::Config;
use crate::proxy::cert;
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run(domain: &str, port: u16, no_daemon: bool) -> Result<()> {
    let config = Config { domain: domain.to_string() };

    // Create config directory
    let config_dir = Config::config_dir()?;
    std::fs::create_dir_all(&config_dir)?;

    // Generate CA if it doesn't exist
    let ca_cert_path = Config::ca_cert_path()?;
    let ca_key_path = Config::ca_key_path()?;

    if ca_cert_path.exists() && ca_key_path.exists() {
        eprintln!("{} CA certificate already exists", "ok:".green());
    } else {
        eprintln!("generating CA certificate...");
        let (ca_cert_pem, ca_key_pem) = cert::generate_ca()?;
        cert::write_pem(&ca_cert_path, &ca_cert_pem)?;
        cert::write_key_pem(&ca_key_path, &ca_key_pem)?;
        eprintln!("{} CA certificate generated", "ok:".green());

        // Trust the CA
        eprintln!("trusting CA in system keychain (may require sudo)...");
        match cert::trust_ca_in_system(&ca_cert_path) {
            Ok(()) => eprintln!("{} CA trusted in system keychain", "ok:".green()),
            Err(e) => {
                eprintln!(
                    "{} could not trust CA automatically: {e}",
                    "warn:".yellow()
                );
                eprintln!(
                    "  manually trust: {}",
                    ca_cert_path.display().to_string().cyan()
                );
            }
        }
    }

    // Generate wildcard cert if it doesn't exist
    let tls_cert_path = Config::tls_cert_path()?;
    let tls_key_path = Config::tls_key_path()?;

    if tls_cert_path.exists() && tls_key_path.exists() {
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
        eprintln!("starting daemon on port {port}...");
        let exe = std::env::current_exe().context("could not determine binary path")?;

        let mut cmd = std::process::Command::new(exe);
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

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn().context("could not spawn daemon")?;
        let pid = child.id();
        // Spawn a thread to reap the child so it does not become a zombie.
        // After setsid(), the child won't receive signals when the parent exits.
        std::thread::spawn(move || { let _ = child.wait(); });
        eprintln!("{} daemon started (pid: {})", "ok:".green(), pid);
    }

    eprintln!();
    eprintln!("{}", "Setup complete!".green().bold());
    eprintln!();
    eprintln!("Next steps:");
    eprintln!("  1. Set up wildcard DNS for *.{domain} -> 127.0.0.1");
    eprintln!("     macOS: brew install dnsmasq");
    eprintln!("     Quick: echo 'address=/.{domain}/127.0.0.1' >> /opt/homebrew/etc/dnsmasq.conf");
    eprintln!("  2. Add a devproxy.port label to your docker-compose.yml");
    eprintln!("  3. Run: devproxy up");

    Ok(())
}
