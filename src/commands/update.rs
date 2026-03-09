use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::path::Path;

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

/// Known binary magic numbers for validation after download.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const MACHO_MAGICS: [[u8; 4]; 4] = [
    [0xfe, 0xed, 0xfa, 0xce], // MH_MAGIC (32-bit)
    [0xfe, 0xed, 0xfa, 0xcf], // MH_MAGIC_64
    [0xcf, 0xfa, 0xed, 0xfe], // MH_CIGAM_64 (reversed)
    [0xce, 0xfa, 0xed, 0xfe], // MH_CIGAM (reversed)
];

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
        // If either fails to parse, assume no update is needed
        _ => false,
    }
}

/// Verify that the file at `path` starts with a recognized binary magic number
/// (ELF or Mach-O). This prevents replacing the binary with an HTML error page
/// or other non-binary content from a failed download.
fn validate_binary_magic(path: &Path) -> Result<()> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("could not open downloaded file at {}", path.display()))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .context("downloaded file is too small to be a valid binary")?;

    if magic == ELF_MAGIC {
        return Ok(());
    }
    for macho_magic in &MACHO_MAGICS {
        if magic == *macho_magic {
            return Ok(());
        }
    }

    bail!(
        "downloaded file is not a valid binary (magic: {:02x} {:02x} {:02x} {:02x}). \
         The download may have returned an error page instead of the binary.",
        magic[0],
        magic[1],
        magic[2],
        magic[3]
    );
}

/// Check that `exe_path` is writable (or its parent directory is, for rename).
/// Bail early with a permission hint if not, to avoid stopping the daemon
/// before discovering we cannot replace the binary.
fn check_write_permission(exe_path: &Path) -> Result<()> {
    // Check if we can write to the exe itself (for copy fallback)
    // or its parent directory (for rename).
    let writable = if exe_path.exists() {
        // Try opening the file for writing as a permission check
        std::fs::OpenOptions::new().write(true).open(exe_path).is_ok()
    } else if let Some(parent) = exe_path.parent() {
        // Check parent directory is writable
        let test_path = parent.join(".devproxy-update-check");
        let ok = std::fs::File::create(&test_path).is_ok();
        let _ = std::fs::remove_file(&test_path);
        ok
    } else {
        false
    };

    if !writable {
        print_permission_hint();
        bail!(
            "cannot write to {}. Insufficient permissions.",
            exe_path.display()
        );
    }
    Ok(())
}

/// Download a file from `url` to `dest` using curl.
fn download_file(url: &str, dest: &Path) -> Result<()> {
    let dl_output = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .output()
        .context("failed to run curl to download update")?;

    if !dl_output.status.success() {
        let stderr = String::from_utf8_lossy(&dl_output.stderr);
        bail!(
            "failed to download update from {url}: {stderr}\n\
             Note: if you see a 403 error, this may be due to GitHub API rate limiting. \
             Try again in a few minutes."
        );
    }
    Ok(())
}

/// Set permissions and codesign the binary at `path`.
fn prepare_binary(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .context("failed to set permissions on downloaded binary")?;
    }

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("xattr")
            .arg("-cr")
            .arg(path)
            .output();
        let _ = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(path)
            .output();
    }

    Ok(())
}

fn print_permission_hint() {
    eprintln!(
        "{} try running with sudo, or re-run the install script:\n  \
         curl -fsSL https://raw.githubusercontent.com/foundra-build/devproxy/main/install.sh | sh",
        "hint:".yellow()
    );
}

/// Replace the binary at `exe_path` with the one at `tmpfile`.
/// Tries atomic rename first, falls back to copy on cross-device errors.
/// Provides a permission hint on failure.
fn replace_binary(tmpfile: &Path, exe_path: &Path) -> Result<()> {
    let result = match std::fs::rename(tmpfile, exe_path) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            let copy_result = std::fs::copy(tmpfile, exe_path)
                .map(|_| ())
                .context("failed to copy new binary over existing one");
            // Clean up tmpfile after copy (rename would have consumed it)
            let _ = std::fs::remove_file(tmpfile);
            copy_result
        }
        Err(e) => Err(anyhow::Error::new(e).context("failed to replace binary")),
    };

    if let Err(ref e) = result {
        // Check the root cause for a permission error
        if has_permission_error(e) {
            print_permission_hint();
        }
    }

    result
}

/// Walk the anyhow error chain looking for a std::io::Error with PermissionDenied.
fn has_permission_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(io_err) = cause.downcast_ref::<std::io::Error>()
            && io_err.kind() == std::io::ErrorKind::PermissionDenied
        {
            return true;
        }
    }
    false
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
        bail!(
            "failed to check for updates: {stderr}\n\
             Note: if you see a 403 error, this may be due to GitHub API rate limiting. \
             Try again in a few minutes."
        );
    }

    let release: GitHubRelease = serde_json::from_slice(&output.stdout)
        .context("failed to parse GitHub release response")?;
    let remote_version = strip_version_prefix(&release.tag_name).to_owned();

    // Compare versions using semver
    if !is_newer_version(current_version, &remote_version) {
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

    // Download new binary to a tmpfile first, before touching the daemon
    // or the existing binary. If the download fails, nothing has changed.
    let download_url = format!(
        "https://github.com/foundra-build/devproxy/releases/latest/download/devproxy-{target}"
    );
    let exe_path = std::env::current_exe().context("could not determine binary path")?;
    let tmpfile = exe_path.with_extension("tmp");

    // Check write permissions early, before downloading or stopping daemon
    check_write_permission(&exe_path)?;

    // Ensure tmpfile is cleaned up on any error path
    let result = do_update(&download_url, &exe_path, &tmpfile);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmpfile);
        return result;
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

fn do_update(download_url: &str, exe_path: &Path, tmpfile: &Path) -> Result<()> {
    download_file(download_url, tmpfile)?;
    validate_binary_magic(tmpfile)?;
    prepare_binary(tmpfile)?;

    // Stop daemon after download succeeds but before binary replacement,
    // so a download failure is a no-op (daemon stays running), and the
    // binary replacement happens with no running daemon for a clean transition.
    let socket_path = crate::config::Config::socket_path()?;
    if socket_path.exists()
        && crate::ipc::ping_sync(&socket_path, std::time::Duration::from_secs(2))
    {
        eprintln!("{} stopping daemon for update...", "info:".cyan());
        super::init::kill_stale_daemon()?;
    }

    replace_binary(tmpfile, exe_path)?;

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

    #[test]
    fn test_validate_binary_magic_elf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-elf");
        // ELF magic + some padding
        std::fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        assert!(validate_binary_magic(&path).is_ok());
    }

    #[test]
    fn test_validate_binary_magic_macho() {
        let dir = tempfile::tempdir().unwrap();
        for (i, magic) in MACHO_MAGICS.iter().enumerate() {
            let path = dir.path().join(format!("test-macho-{i}"));
            let mut content = magic.to_vec();
            content.extend_from_slice(&[0; 4]);
            std::fs::write(&path, &content).unwrap();
            assert!(
                validate_binary_magic(&path).is_ok(),
                "Mach-O magic variant {i} should be valid"
            );
        }
    }

    #[test]
    fn test_validate_binary_magic_html() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-html");
        std::fs::write(&path, b"<!DOCTYPE html>").unwrap();
        assert!(validate_binary_magic(&path).is_err());
    }

    #[test]
    fn test_validate_binary_magic_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-small");
        std::fs::write(&path, b"ab").unwrap();
        assert!(validate_binary_magic(&path).is_err());
    }

    #[test]
    fn test_has_permission_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let anyhow_err = anyhow::Error::new(io_err).context("wrapping context");
        assert!(has_permission_error(&anyhow_err));

        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let anyhow_err = anyhow::Error::new(io_err).context("wrapping context");
        assert!(!has_permission_error(&anyhow_err));
    }
}
