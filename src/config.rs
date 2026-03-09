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
            let port: u16 = port_str
                .parse()
                .with_context(|| format!("invalid devproxy.port value '{port_str}' on service '{name}'"))?;
            found.push((name.clone(), port));
        }
    }

    match found.len() {
        0 => bail!("no service has a devproxy.port label"),
        1 => Ok(found.into_iter().next().expect("checked len")),
        _ => bail!(
            "multiple services have devproxy.port labels: {}. Only one is supported.",
            found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
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
    for name in &["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"] {
        let path = dir.join(name);
        if path.exists() {
            return Ok(path);
        }
    }
    bail!("no docker-compose.yml found in {}", dir.display())
}

/// Write the port-override compose file
pub fn write_override_file(dir: &Path, service_name: &str, host_port: u16, container_port: u16) -> Result<PathBuf> {
    let path = dir.join(".devproxy-override.yml");
    let content = format!(
        "services:\n  {service_name}:\n    ports:\n      - \"{host_port}:{container_port}\"\n"
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
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!(
            "no .devproxy-project file found in {}. Is this project running via `devproxy up`?",
            dir.display()
        ))?;
    Ok(content.trim().to_string())
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

    #[test]
    fn config_dir_respects_env_var() {
        let old = std::env::var("DEVPROXY_CONFIG_DIR").ok();
        unsafe { std::env::set_var("DEVPROXY_CONFIG_DIR", "/tmp/test-devproxy-config") };
        let dir = Config::config_dir().unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/test-devproxy-config"));
        match old {
            Some(v) => unsafe { std::env::set_var("DEVPROXY_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("DEVPROXY_CONFIG_DIR") },
        }
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
        assert!(result.unwrap_err().to_string().contains(".devproxy-project"));
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
        assert!(result.unwrap_err().to_string().contains("no docker-compose.yml"));
    }
}
