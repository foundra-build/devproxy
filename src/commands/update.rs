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
fn platform_target() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown-linux-gnu"
    };
    format!("{arch}-{os}")
}

pub async fn run() -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");
    let target = platform_target();

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

    // Compare versions
    if current_version == remote_version {
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

        // Atomic replace
        std::fs::rename(&tmpfile, &exe_path).context("failed to replace binary")?;

        Ok(())
    })();

    // Clean up tmpfile on error
    if download_result.is_err() {
        let _ = std::fs::remove_file(&tmpfile);
        return download_result;
    }

    // Stop daemon if running
    let config = crate::config::Config::load();
    if let Ok(_config) = config {
        let socket_path = crate::config::Config::socket_path()?;
        if socket_path.exists()
            && crate::ipc::ping_sync(&socket_path, std::time::Duration::from_secs(2))
        {
            eprintln!("{} stopping daemon for update...", "info:".cyan());
            super::init::kill_stale_daemon()?;
            eprintln!(
                "{} run {} to restart the daemon",
                "info:".cyan(),
                "devproxy init".bold()
            );
        }
    }

    eprintln!(
        "{} updated to v{remote_version}",
        "ok:".green()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison_up_to_date() {
        let current = "0.1.0";
        let remote = "0.1.0";
        assert_eq!(current, strip_version_prefix(remote));
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
        let target = platform_target();
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
