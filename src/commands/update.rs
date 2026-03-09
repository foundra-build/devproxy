use anyhow::{Context, Result, bail};
use colored::Colorize;

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

/// Strip leading 'v' prefix from a version tag string.
fn strip_version_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Determine the platform target triple at compile time.
/// Returns an error for unsupported architectures/OS combinations.
fn platform_target() -> Result<String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        bail!("unsupported architecture: devproxy update only supports aarch64 and x86_64");
    };
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else {
        bail!("unsupported OS: devproxy update only supports macOS and Linux");
    };
    Ok(format!("{arch}-{os}"))
}

/// Compare two version strings using semantic versioning.
/// Returns true if `remote` is strictly newer than `current`.
fn is_newer_version(current: &str, remote: &str) -> bool {
    let current = semver::Version::parse(current);
    let remote = semver::Version::parse(remote);
    match (current, remote) {
        (Ok(c), Ok(r)) => r > c,
        // If either fails to parse, fall back to string inequality
        _ => false,
    }
}

pub async fn run() -> Result<()> {
    tokio::task::spawn_blocking(run_blocking)
        .await
        .context("update task panicked")?
}

fn run_blocking() -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");
    let target = platform_target()?;

    // Check latest version via GitHub API
    eprintln!("{} checking for updates...", "info:".cyan());
    let output = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "https://api.github.com/repos/foundra-build/devproxy/releases/latest",
        ])
        .output()
        .context("failed to run curl to check for updates")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("failed to check for updates: {stderr}");
    }

    let release: GitHubRelease = serde_json::from_slice(&output.stdout)
        .context("failed to parse GitHub release response")?;
    let remote_version = strip_version_prefix(&release.tag_name);

    // Compare versions using semver
    if !is_newer_version(current_version, remote_version) {
        eprintln!(
            "{} already up to date (v{current_version})",
            "ok:".green()
        );
        return Ok(());
    }

    eprintln!(
        "{} updating v{current_version} -> v{remote_version}",
        "info:".cyan()
    );

    // Stop daemon before replacing binary, so there is a clean transition.
    // Config::socket_path() is a static method that does not depend on
    // loading a project config, so it works from any directory.
    let socket_path = crate::config::Config::socket_path()?;
    if socket_path.exists()
        && crate::ipc::ping_sync(&socket_path, std::time::Duration::from_secs(2))
    {
        eprintln!("{} stopping daemon for update...", "info:".cyan());
        super::init::kill_stale_daemon()?;
    }

    // Download new binary
    let download_url = format!(
        "https://github.com/foundra-build/devproxy/releases/latest/download/devproxy-{target}"
    );
    let exe_path = std::env::current_exe().context("could not determine binary path")?;
    let tmpfile = exe_path.with_extension("tmp");

    let download_result = (|| -> Result<()> {
        let dl_output = std::process::Command::new("curl")
            .args([
                "-fsSL",
                "-o",
                &tmpfile.to_string_lossy(),
                &download_url,
            ])
            .output()
            .context("failed to run curl to download update")?;

        if !dl_output.status.success() {
            let stderr = String::from_utf8_lossy(&dl_output.stderr);
            bail!("failed to download update from {download_url}: {stderr}");
        }

        // chmod 755
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmpfile, std::fs::Permissions::from_mode(0o755))
                .context("failed to set permissions on downloaded binary")?;
        }

        // macOS: clear quarantine and ad-hoc sign
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("xattr")
                .args(["-cr", &tmpfile.to_string_lossy()])
                .output();
            let _ = std::process::Command::new("codesign")
                .args(["--force", "--sign", "-", &tmpfile.to_string_lossy()])
                .output();
        }

        // Replace binary. Try rename first (atomic, but only works on same
        // filesystem). If rename fails with EXDEV (cross-device link), fall
        // back to copy + remove.
        match std::fs::rename(&tmpfile, &exe_path) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                std::fs::copy(&tmpfile, &exe_path)
                    .context("failed to copy new binary over existing one")?;
                let _ = std::fs::remove_file(&tmpfile);
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context("failed to replace binary"));
            }
        }

        Ok(())
    })();

    // Clean up tmpfile on error
    if download_result.is_err() {
        let _ = std::fs::remove_file(&tmpfile);
        return download_result;
    }

    eprintln!(
        "{} updated to v{remote_version}",
        "ok:".green()
    );
    eprintln!(
        "{} run {} to restart the daemon",
        "info:".cyan(),
        "devproxy init".bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison_up_to_date() {
        assert!(!is_newer_version("0.1.0", "0.1.0"));
    }

    #[test]
    fn test_version_comparison_newer() {
        assert!(is_newer_version("0.1.0", "0.2.0"));
        assert!(is_newer_version("0.1.0", "1.0.0"));
        assert!(is_newer_version("1.0.0", "1.0.1"));
    }

    #[test]
    fn test_version_comparison_older_no_downgrade() {
        assert!(!is_newer_version("0.2.0", "0.1.0"));
        assert!(!is_newer_version("1.0.0", "0.9.0"));
    }

    #[test]
    fn test_version_comparison_prerelease() {
        // Pre-release is considered older than the release
        assert!(is_newer_version("0.1.0-rc1", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.1.0-rc1"));
    }

    #[test]
    fn test_version_comparison_unparseable() {
        // Unparseable versions should not trigger an update
        assert!(!is_newer_version("0.1.0", "not-a-version"));
        assert!(!is_newer_version("not-a-version", "0.1.0"));
    }

    #[test]
    fn test_strip_version_prefix() {
        assert_eq!(strip_version_prefix("v1.2.3"), "1.2.3");
        assert_eq!(strip_version_prefix("1.2.3"), "1.2.3");
        assert_eq!(strip_version_prefix("v0.1.0"), "0.1.0");
        assert_eq!(strip_version_prefix(""), "");
    }

    #[test]
    fn test_platform_target() {
        let target = platform_target().expect("platform_target should succeed on test host");
        assert!(
            target.contains("apple-darwin") || target.contains("unknown-linux-gnu"),
            "target should contain a known OS: {target}"
        );
        assert!(
            target.contains("aarch64") || target.contains("x86_64"),
            "target should contain a known arch: {target}"
        );
    }

    #[test]
    fn test_github_release_deserialization() {
        let json = r#"{"tag_name":"v1.2.3","other_field":"ignored"}"#;
        let release: GitHubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v1.2.3");
    }

    #[test]
    fn test_github_release_missing_tag_name() {
        let json = r#"{"other_field":"ignored"}"#;
        let result: std::result::Result<GitHubRelease, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
