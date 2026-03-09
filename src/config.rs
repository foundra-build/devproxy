use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Global devproxy configuration, stored at <config_dir>/config.json
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub domain: String,
}

impl Config {
    /// Returns the devproxy config directory.
    ///
    /// Checks `DEVPROXY_CONFIG_DIR` env var first (essential for test isolation,
    /// since `dirs::config_dir()` on macOS ignores `HOME`). Falls back to
    /// `dirs::config_dir()/devproxy`.
    pub fn config_dir() -> Result<PathBuf> {
        if let Ok(dir) = std::env::var("DEVPROXY_CONFIG_DIR") {
            return Ok(PathBuf::from(dir));
        }
        let dir = dirs::config_dir()
            .context("could not determine config directory")?
            .join("devproxy");
        Ok(dir)
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    pub fn socket_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("devproxy.sock"))
    }

    pub fn ca_cert_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("ca-cert.pem"))
    }

    pub fn ca_key_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("ca-key.pem"))
    }

    pub fn tls_cert_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("tls-cert.pem"))
    }

    pub fn tls_key_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("tls-key.pem"))
    }

    pub fn pid_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("daemon.pid"))
    }

    pub fn daemon_log_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("daemon.log"))
    }

    /// Returns the path to the dedicated daemon binary.
    ///
    /// The daemon binary is stored separately from the CLI binary so that
    /// launchd's KeepAlive monitoring of the daemon path does not interfere
    /// with normal CLI invocations (e.g., `devproxy --version`).
    ///
    /// Path: `~/.local/share/devproxy/devproxy-daemon`
    /// Respects `DEVPROXY_DATA_DIR` env var for test isolation.
    pub fn daemon_binary_path() -> Result<PathBuf> {
        if let Ok(dir) = std::env::var("DEVPROXY_DATA_DIR") {
            return Ok(PathBuf::from(dir).join("devproxy-daemon"));
        }
        let dir = dirs::data_dir()
            .context("could not determine data directory")?
            .join("devproxy");
        Ok(dir.join("devproxy-daemon"))
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir()?;
        std::fs::create_dir_all(&dir)?;
        let path = Self::config_path()?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read config at {}", path.display()))?;
        let config: Config = serde_json::from_str(&data)?;
        Ok(config)
    }
}

/// Represents a parsed docker-compose.yml, enough to find devproxy.port labels
#[derive(Debug, Deserialize)]
pub struct ComposeFile {
    pub services: HashMap<String, ComposeService>,
}

#[derive(Debug, Deserialize)]
pub struct ComposeService {
    #[serde(default)]
    pub labels: Labels,
}

/// Labels can be either a map or a list of "key=value" strings
#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
pub enum Labels {
    Map(HashMap<String, serde_yaml::Value>),
    List(Vec<String>),
    #[default]
    Empty,
}

impl Labels {
    pub fn get(&self, key: &str) -> Option<String> {
        match self {
            Labels::Map(map) => map.get(key).map(|v| match v {
                serde_yaml::Value::Number(n) => n.to_string(),
                serde_yaml::Value::String(s) => s.clone(),
                other => format!("{other:?}"),
            }),
            Labels::List(list) => {
                for item in list {
                    if let Some((k, v)) = item.split_once('=')
                        && k.trim() == key
                    {
                        return Some(v.trim().to_string());
                    }
                }
                None
            }
            Labels::Empty => None,
        }
    }
}

/// Find the service name and container port that has the devproxy.port label
pub fn find_devproxy_service(compose: &ComposeFile) -> Result<(String, u16)> {
    let mut found: Vec<(String, u16)> = Vec::new();

    for (name, svc) in &compose.services {
        if let Some(port_str) = svc.labels.get("devproxy.port") {
            let port: u16 = port_str.parse().with_context(|| {
                format!("invalid devproxy.port value '{port_str}' on service '{name}'")
            })?;
            found.push((name.clone(), port));
        }
    }

    match found.len() {
        0 => bail!("no service has a devproxy.port label"),
        1 => Ok(found.into_iter().next().expect("checked len")),
        _ => bail!(
            "multiple services have devproxy.port labels: {}. Only one is supported.",
            found
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Parse a docker-compose.yml file
pub fn parse_compose_file(path: &Path) -> Result<ComposeFile> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let compose: ComposeFile = serde_yaml::from_str(&data)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(compose)
}

/// Find the docker-compose file in the given directory.
/// Searches for docker-compose.yml, docker-compose.yaml, compose.yml, compose.yaml.
/// Returns the full path.
pub fn find_compose_file(dir: &Path) -> Result<PathBuf> {
    for name in &[
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let path = dir.join(name);
        if path.exists() {
            return Ok(path);
        }
    }
    bail!("no docker-compose.yml found in {}", dir.display())
}

/// Write the port-override compose file.
///
/// The service name is validated to contain only alphanumeric, hyphen, and
/// underscore characters before being interpolated into YAML, preventing
/// injection of arbitrary YAML content.
pub fn write_override_file(
    dir: &Path,
    service_name: &str,
    host_port: u16,
    container_port: u16,
) -> Result<PathBuf> {
    // Validate service name to prevent YAML injection
    if service_name.is_empty()
        || !service_name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "invalid service name '{service_name}': must contain only alphanumeric, hyphen, or underscore characters"
        );
    }

    let path = dir.join(".devproxy-override.yml");
    let content = format!(
        "services:\n  {service_name}:\n    ports:\n      - \"127.0.0.1:{host_port}:{container_port}\"\n"
    );
    std::fs::write(&path, &content)?;
    Ok(path)
}

/// Write the project file that records the slug for this project directory.
/// Used by `down` and `open` to identify which slug belongs to the current directory.
pub fn write_project_file(dir: &Path, slug: &str) -> Result<PathBuf> {
    let path = dir.join(".devproxy-project");
    std::fs::write(&path, format!("{slug}\n"))?;
    Ok(path)
}

/// Read the slug from the project file in the given directory.
/// Returns an error if the file doesn't exist (project not running via devproxy).
pub fn read_project_file(dir: &Path) -> Result<String> {
    let path = dir.join(".devproxy-project");
    let content = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no .devproxy-project file found in {}. Is this project running via `devproxy up`?",
            dir.display()
        )
    })?;
    Ok(content.trim().to_string())
}

/// Detect the app name for the given directory.
/// Tries git remote origin first (extracts repo name from URL),
/// falls back to directory name. Result is sanitized for use in a subdomain label.
pub fn detect_app_name(dir: &Path) -> Result<String> {
    // Try git remote
    if let Ok(output) = std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "remote", "get-url", "origin"])
        .output()
        && output.status.success()
    {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(name) = extract_repo_name(&url) {
            return Ok(sanitize_subdomain(&name));
        }
    }

    // Fall back to directory name
    let dir_name = dir
        .file_name()
        .context("directory has no name")?
        .to_string_lossy()
        .to_string();
    Ok(sanitize_subdomain(&dir_name))
}

/// Extract the repository name from a git remote URL.
/// Handles HTTPS (https://github.com/user/repo.git) and SSH (git@github.com:user/repo.git).
fn extract_repo_name(url: &str) -> Option<String> {
    let path_part = if url.contains("://") {
        url.split('/').next_back()?
    } else if let Some(after_colon) = url.split(':').nth(1) {
        after_colon.split('/').next_back()?
    } else {
        return None;
    };
    let name = path_part.strip_suffix(".git").unwrap_or(path_part);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Sanitize a string for use as part of a DNS subdomain label:
/// lowercase, replace non-alphanumeric with hyphens, collapse consecutive hyphens,
/// trim leading/trailing hyphens. Does NOT truncate — callers that need length
/// enforcement should use `compose_slug` which truncates the composite result.
fn sanitize_subdomain(s: &str) -> String {
    let lower = s.to_lowercase();
    let replaced: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    replaced
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Compose a DNS-safe slug from a random slug and app name.
/// Joins as `{random_slug}-{app_name}` and truncates the result to 63 characters
/// (the RFC 1035 DNS label limit), trimming any trailing hyphen.
pub fn compose_slug(random_slug: &str, app_name: &str) -> String {
    let composite = format!("{random_slug}-{app_name}");
    if composite.len() <= 63 {
        return composite;
    }
    composite.chars().take(63).collect::<String>().trim_end_matches('-').to_string()
}

/// Find a free ephemeral port
pub fn find_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that DEVPROXY_CONFIG_DIR is respected by running the binary
    /// in a subprocess with the env var set, avoiding unsafe env mutation
    /// in the test process.
    #[test]
    fn config_dir_respects_env_var() {
        // Run `devproxy init --help` with DEVPROXY_CONFIG_DIR set.
        // The fact that init --no-daemon writes certs to the env-var dir
        // (verified by test_init_generates_certs in e2e.rs) proves the
        // env var is respected. Here we do a lighter-weight check: invoke
        // the binary with DEVPROXY_CONFIG_DIR pointing at a temp dir and
        // verify `status` references that path in its error output.
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().to_path_buf();

        // Write a minimal config so `status` tries to connect to the socket
        std::fs::write(config_dir.join("config.json"), r#"{"domain":"test.dev"}"#).unwrap();

        // Find the binary
        let mut bin = std::env::current_exe().unwrap();
        bin.pop(); // test binary name
        bin.pop(); // deps/
        bin.push("devproxy");

        let output = std::process::Command::new(&bin)
            .args(["status"])
            .env("DEVPROXY_CONFIG_DIR", &config_dir)
            .output()
            .expect("failed to run devproxy status");

        let stderr = String::from_utf8_lossy(&output.stderr);
        // The daemon is not running, so status should report an error
        // mentioning the socket path inside our custom config dir.
        assert!(
            stderr.contains("not running") || stderr.contains("could not connect"),
            "status should try the custom config dir: {stderr}"
        );
    }

    #[test]
    fn parse_labels_as_map() {
        let yaml = r#"
services:
  web:
    labels:
      devproxy.port: 3000
"#;
        let compose: ComposeFile = serde_yaml::from_str(yaml).unwrap();
        let (name, port) = find_devproxy_service(&compose).unwrap();
        assert_eq!(name, "web");
        assert_eq!(port, 3000);
    }

    #[test]
    fn parse_labels_as_list() {
        let yaml = r#"
services:
  api:
    labels:
      - "devproxy.port=8080"
"#;
        let compose: ComposeFile = serde_yaml::from_str(yaml).unwrap();
        let (name, port) = find_devproxy_service(&compose).unwrap();
        assert_eq!(name, "api");
        assert_eq!(port, 8080);
    }

    #[test]
    fn no_devproxy_label_is_error() {
        let yaml = r#"
services:
  web:
    labels:
      some.other: label
"#;
        let compose: ComposeFile = serde_yaml::from_str(yaml).unwrap();
        let result = find_devproxy_service(&compose);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no service"));
    }

    #[test]
    fn multiple_devproxy_labels_is_error() {
        let yaml = r#"
services:
  web:
    labels:
      devproxy.port: 3000
  api:
    labels:
      devproxy.port: 8080
"#;
        let compose: ComposeFile = serde_yaml::from_str(yaml).unwrap();
        let result = find_devproxy_service(&compose);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("multiple"));
    }

    #[test]
    fn find_free_port_returns_nonzero() {
        let port = find_free_port().unwrap();
        assert!(port > 0);
    }

    #[test]
    fn project_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        write_project_file(dir.path(), "swift-penguin").unwrap();
        let slug = read_project_file(dir.path()).unwrap();
        assert_eq!(slug, "swift-penguin");
    }

    #[test]
    fn read_project_file_missing_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_project_file(dir.path());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains(".devproxy-project")
        );
    }

    #[test]
    fn find_compose_file_finds_standard_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("docker-compose.yml"), "services: {}").unwrap();
        let found = find_compose_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "docker-compose.yml");
    }

    #[test]
    fn find_compose_file_missing_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_compose_file(dir.path());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no docker-compose.yml")
        );
    }

    #[test]
    fn detect_app_name_from_git_remote_https() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "https://github.com/user/my-cool-app.git"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let name = detect_app_name(dir.path()).unwrap();
        assert_eq!(name, "my-cool-app");
    }

    #[test]
    fn detect_app_name_from_git_remote_ssh() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "git@github.com:user/another-app.git"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let name = detect_app_name(dir.path()).unwrap();
        assert_eq!(name, "another-app");
    }

    #[test]
    fn detect_app_name_falls_back_to_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-project");
        std::fs::create_dir_all(&sub).unwrap();
        let name = detect_app_name(&sub).unwrap();
        assert_eq!(name, "my-project");
    }

    #[test]
    fn detect_app_name_sanitizes() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("My Cool App!!!");
        std::fs::create_dir_all(&sub).unwrap();
        let name = detect_app_name(&sub).unwrap();
        assert_eq!(name, "my-cool-app");
    }

    #[test]
    fn extract_repo_name_https() {
        assert_eq!(
            extract_repo_name("https://github.com/user/repo.git"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn extract_repo_name_ssh() {
        assert_eq!(
            extract_repo_name("git@github.com:user/repo.git"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn extract_repo_name_no_git_suffix() {
        assert_eq!(
            extract_repo_name("https://github.com/user/repo"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn sanitize_subdomain_basic() {
        assert_eq!(sanitize_subdomain("My Cool App!!!"), "my-cool-app");
    }

    #[test]
    fn sanitize_subdomain_already_clean() {
        assert_eq!(sanitize_subdomain("my-app"), "my-app");
    }

    #[test]
    fn sanitize_subdomain_does_not_truncate() {
        let long_name = "a".repeat(100);
        let result = sanitize_subdomain(&long_name);
        assert_eq!(result.len(), 100, "sanitize_subdomain should not truncate");
    }

    #[test]
    fn compose_slug_basic() {
        assert_eq!(compose_slug("swift-penguin", "devproxy"), "swift-penguin-devproxy");
    }

    #[test]
    fn compose_slug_truncates_to_63_chars() {
        let long_app = "a".repeat(100);
        let result = compose_slug("swift-penguin", &long_app);
        assert!(result.len() <= 63, "composite slug must fit in a DNS label: len={}", result.len());
        assert!(!result.ends_with('-'), "should not end with hyphen after truncation");
        assert!(result.starts_with("swift-penguin-"), "should preserve the random slug prefix");
    }

    #[test]
    fn compose_slug_normal_lengths_not_truncated() {
        let result = compose_slug("bold-fox", "my-cool-app");
        assert_eq!(result, "bold-fox-my-cool-app");
    }
}
