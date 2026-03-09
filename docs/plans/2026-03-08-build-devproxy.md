# devproxy — Full Build Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Build devproxy from scratch — a single Rust binary that provides local HTTPS dev subdomains for Docker Compose projects, with an e2e test harness.

**Architecture:** A CLI binary (clap) that manages a background daemon process. The daemon runs two async tasks via `tokio::join!`: an HTTPS reverse proxy (tokio-rustls + hyper) and a Docker event watcher. CLI communicates with the daemon over a Unix domain socket using JSON-line IPC. Docker is the sole source of truth for routing — no persistent route state. A small `.devproxy-project` file in each project directory tracks which slug was assigned (enabling `down` and `open` to target the correct project).

**Tech Stack:** Rust 2024 edition, clap 4, tokio, hyper 1, hyper-util, tokio-rustls 0.26, rustls 0.23, rcgen, serde/serde_json, serde_yaml, anyhow, colored, rand, dirs, open.

**Key design decisions:**
- hyper 0.14 (from spec) is legacy. We use hyper 1.x + hyper-util which is the current stable API. tokio-rustls 0.26 + rustls 0.23 match.
- The `--port` flag on both `daemon` and `init` enables e2e testing on high ports without sudo. `init --port 8443` forwards the port to the spawned daemon. `init --no-daemon` generates certs without spawning a daemon (used by e2e tests that manage the daemon lifecycle themselves).
- `DEVPROXY_CONFIG_DIR` env var overrides the default config directory (`dirs::config_dir()/devproxy`). This is essential for test isolation because `dirs::config_dir()` on macOS uses `NSSearchPathForDirectoriesInDomains` which ignores `HOME`.
- `devproxy up` writes `.devproxy-project` (containing the slug) and `.devproxy-override.yml` (containing port binding). `devproxy down` and `devproxy open` read `.devproxy-project` to identify the current project's slug. Both files are `.gitignore`d.
- `init` is idempotent: it skips CA generation if the CA already exists, and skips wildcard cert generation if the cert already exists. Running `init` twice is safe and will not invalidate a running daemon's certs.
- E2e tests each copy fixtures into an isolated temp directory and bind the daemon to a unique ephemeral port, so they can run in parallel without interfering.
- `find_compose_file()` lives in `config.rs` as a shared utility used by both `up` and `down`.

---

## Task 1: Set Up Cargo.toml with All Dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Write the full Cargo.toml**

```toml
[package]
name = "devproxy"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "devproxy"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
anyhow = "1"
dirs = "5"
colored = "2"
open = "5"
rand = "0.9"
tokio = { version = "1", features = ["full"] }
hyper = { version = "1", features = ["http1", "server", "client"] }
hyper-util = { version = "0.1", features = ["tokio", "http1", "server", "client-legacy"] }
http-body-util = "0.1"
tokio-rustls = "0.26"
rustls = { version = "0.23", default-features = false, features = ["ring", "logging", "std", "tls12"] }
rustls-pemfile = "2"
rcgen = "0.13"
bytes = "1"
time = "0.3"

[dev-dependencies]
reqwest = { version = "0.12", features = ["rustls-tls", "json"], default-features = false }
tempfile = "3"
tokio-test = "0.4"
assert_cmd = "2"
predicates = "3"
```

**Step 2: Verify it compiles**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo check`
Expected: Compiles with warnings about unused imports (that's fine, no code yet)

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add all dependencies to Cargo.toml"
```

---

## Task 2: CLI Definitions (cli.rs)

**Files:**
- Create: `src/cli.rs`
- Modify: `src/main.rs`

**Step 1: Write the CLI module**

Note: `init` accepts `--port` (forwarded to daemon) and `--no-daemon` (skip daemon spawn). The `daemon` subcommand accepts `--port`.

```rust
// src/cli.rs
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "devproxy", about = "Local HTTPS dev subdomains for Docker Compose")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// One-time setup: generate certs, trust CA, start daemon
    Init {
        /// Domain for dev subdomains (e.g., mysite.dev)
        #[arg(long, default_value = "mysite.dev")]
        domain: String,
        /// Port for the daemon to listen on (default: 443)
        #[arg(long, default_value = "443")]
        port: u16,
        /// Skip starting the daemon (useful for CI or testing)
        #[arg(long, default_value = "false")]
        no_daemon: bool,
    },
    /// Start this project and assign a dev subdomain
    Up,
    /// Stop this project and remove override file
    Down,
    /// List all running projects with slugs and URLs
    Ls,
    /// Open this project's URL in the browser
    Open,
    /// Show daemon health and active route count
    Status,
    /// Run the proxy daemon (internal, hidden)
    #[command(hide = true)]
    Daemon {
        /// Port to listen on (default: 443)
        #[arg(long, default_value = "443")]
        port: u16,
    },
}
```

**Step 2: Write main.rs to parse CLI**

```rust
// src/main.rs
mod cli;

use clap::Parser;
use cli::{Cli, Commands};

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { domain, port, no_daemon } => {
            eprintln!("init: domain={domain} port={port} no_daemon={no_daemon}");
        }
        Commands::Up => {
            eprintln!("up");
        }
        Commands::Down => {
            eprintln!("down");
        }
        Commands::Ls => {
            eprintln!("ls");
        }
        Commands::Open => {
            eprintln!("open");
        }
        Commands::Status => {
            eprintln!("status");
        }
        Commands::Daemon { port } => {
            eprintln!("daemon: port={port}");
        }
    }
}
```

**Step 3: Run to verify**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo run -- --help`
Expected: Shows help with Init, Up, Down, Ls, Open, Status commands (Daemon hidden)

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo run -- init --help`
Expected: Shows `--domain`, `--port`, and `--no-daemon` flags

**Step 4: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat: add CLI definitions with clap derive"
```

---

## Task 3: Slug Generator (slugs.rs)

**Files:**
- Create: `src/slugs.rs`
- Modify: `src/main.rs` (add `mod slugs;`)

**Step 1: Write slugs.rs with implementation and tests**

```rust
// src/slugs.rs
use rand::seq::IndexedRandom;

const ADJECTIVES: &[&str] = &[
    "swift", "bright", "calm", "bold", "keen",
    "warm", "cool", "wild", "fair", "glad",
    "quick", "brave", "proud", "true", "wise",
];

const ANIMALS: &[&str] = &[
    "penguin", "falcon", "otter", "fox", "heron",
    "whale", "eagle", "tiger", "panda", "koala",
    "raven", "wolf", "lynx", "hawk", "crane",
];

pub fn generate_slug() -> String {
    let mut rng = rand::rng();
    let adj = ADJECTIVES.choose(&mut rng).expect("adjectives not empty");
    let animal = ANIMALS.choose(&mut rng).expect("animals not empty");
    format!("{adj}-{animal}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_has_adjective_dash_animal_format() {
        let slug = generate_slug();
        let parts: Vec<&str> = slug.split('-').collect();
        assert_eq!(parts.len(), 2, "slug should be adjective-animal: {slug}");
        assert!(ADJECTIVES.contains(&parts[0]), "first word should be an adjective: {slug}");
        assert!(ANIMALS.contains(&parts[1]), "second word should be an animal: {slug}");
    }

    #[test]
    fn slugs_are_not_always_identical() {
        let slugs: Vec<String> = (0..20).map(|_| generate_slug()).collect();
        let unique: std::collections::HashSet<&String> = slugs.iter().collect();
        assert!(unique.len() > 1, "20 slugs should not all be identical");
    }
}
```

**Step 2: Add mod to main.rs**

Add `mod slugs;` to the top of `src/main.rs`.

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test slugs`
Expected: 2 tests pass

**Step 4: Commit**

```bash
git add src/slugs.rs src/main.rs
git commit -m "feat: add random slug generator with unit tests"
```

---

## Task 4: Config Module (config.rs)

This is the foundation module. It provides all path resolution, compose file discovery, and project file management.

Key points:
- `config_dir()` checks `DEVPROXY_CONFIG_DIR` env var first (essential for test isolation, since `dirs::config_dir()` on macOS ignores `HOME`).
- `find_compose_file()` is a shared utility used by both `up` and `down` (not duplicated).
- `write_project_file()` / `read_project_file()` manage the `.devproxy-project` slug tracking file.

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

**Step 1: Write config.rs with tests**

```rust
// src/config.rs
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
                    if let Some((k, v)) = item.split_once('=') {
                        if k.trim() == key {
                            return Some(v.trim().to_string());
                        }
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
        // Save and restore the env var
        let old = std::env::var("DEVPROXY_CONFIG_DIR").ok();
        std::env::set_var("DEVPROXY_CONFIG_DIR", "/tmp/test-devproxy-config");
        let dir = Config::config_dir().unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/test-devproxy-config"));
        match old {
            Some(v) => std::env::set_var("DEVPROXY_CONFIG_DIR", v),
            None => std::env::remove_var("DEVPROXY_CONFIG_DIR"),
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
```

**Step 2: Add mod to main.rs**

Add `mod config;` to `src/main.rs`.

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test config`
Expected: 10 tests pass

**Step 4: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: add config module with DEVPROXY_CONFIG_DIR support and unit tests"
```

---

## Task 5: IPC Types (ipc.rs)

**Files:**
- Create: `src/ipc.rs`
- Modify: `src/main.rs` (add `mod ipc;`)

**Step 1: Write ipc.rs**

```rust
// src/ipc.rs
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    List,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RouteInfo {
    pub slug: String,
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Routes { routes: Vec<RouteInfo> },
    Error { message: String },
}

/// Send a request to the daemon and get a response
pub async fn send_request(socket_path: &Path, request: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| {
            format!(
                "could not connect to daemon at {}. Is the daemon running? Try `devproxy init`.",
                socket_path.display()
            )
        })?;

    let (reader, mut writer) = stream.into_split();

    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.shutdown().await?;

    let mut buf_reader = BufReader::new(reader);
    let mut response_line = String::new();
    buf_reader.read_line(&mut response_line).await?;

    let response: Response = serde_json::from_str(response_line.trim())
        .context("could not parse daemon response")?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_ping_request() {
        let req = Request::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"cmd":"ping"}"#);
    }

    #[test]
    fn serialize_list_request() {
        let req = Request::List;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"cmd":"list"}"#);
    }

    #[test]
    fn deserialize_pong_response() {
        let json = r#"{"status":"pong"}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        assert!(matches!(resp, Response::Pong));
    }

    #[test]
    fn deserialize_routes_response() {
        let json = r#"{"status":"routes","routes":[{"slug":"swift-penguin.mysite.dev","port":51234}]}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        match resp {
            Response::Routes { routes } => {
                assert_eq!(routes.len(), 1);
                assert_eq!(routes[0].slug, "swift-penguin.mysite.dev");
                assert_eq!(routes[0].port, 51234);
            }
            _ => panic!("expected Routes response"),
        }
    }
}
```

**Step 2: Add mod to main.rs**

Add `mod ipc;` to `src/main.rs`.

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test ipc`
Expected: 4 tests pass

**Step 4: Commit**

```bash
git add src/ipc.rs src/main.rs
git commit -m "feat: add IPC types with serialization unit tests"
```

---

## Task 6: Router (proxy/router.rs)

**Files:**
- Create: `src/proxy/mod.rs`
- Create: `src/proxy/router.rs`
- Modify: `src/main.rs` (add `mod proxy;`)

**Step 1: Write proxy/mod.rs**

```rust
// src/proxy/mod.rs
pub mod router;
pub mod cert;
pub mod docker;
```

**Step 2: Write proxy/router.rs**

```rust
// src/proxy/router.rs
use crate::ipc::RouteInfo;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct Route {
    pub slug: String,
    pub host_port: u16,
}

#[derive(Debug, Clone)]
pub struct Router {
    routes: Arc<RwLock<HashMap<String, Route>>>,
    domain: String,
}

impl Router {
    pub fn new(domain: &str) -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
            domain: domain.to_string(),
        }
    }

    /// Insert a route: slug -> host_port. The full hostname is slug.domain.
    pub fn insert(&self, slug: &str, host_port: u16) {
        let hostname = format!("{slug}.{}", self.domain);
        let route = Route {
            slug: slug.to_string(),
            host_port,
        };
        self.routes.write().expect("lock poisoned").insert(hostname, route);
    }

    /// Remove a route by slug
    pub fn remove(&self, slug: &str) {
        let hostname = format!("{slug}.{}", self.domain);
        self.routes.write().expect("lock poisoned").remove(&hostname);
    }

    /// Look up a host_port by full hostname (e.g., "swift-penguin.mysite.dev")
    pub fn get(&self, hostname: &str) -> Option<u16> {
        self.routes
            .read()
            .expect("lock poisoned")
            .get(hostname)
            .map(|r| r.host_port)
    }

    /// List all routes
    pub fn list(&self) -> Vec<RouteInfo> {
        self.routes
            .read()
            .expect("lock poisoned")
            .iter()
            .map(|(hostname, route)| RouteInfo {
                slug: hostname.clone(),
                port: route.host_port,
            })
            .collect()
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let router = Router::new("mysite.dev");
        router.insert("swift-penguin", 51234);
        assert_eq!(router.get("swift-penguin.mysite.dev"), Some(51234));
    }

    #[test]
    fn get_missing_returns_none() {
        let router = Router::new("mysite.dev");
        assert_eq!(router.get("nonexistent.mysite.dev"), None);
    }

    #[test]
    fn remove_route() {
        let router = Router::new("mysite.dev");
        router.insert("swift-penguin", 51234);
        router.remove("swift-penguin");
        assert_eq!(router.get("swift-penguin.mysite.dev"), None);
    }

    #[test]
    fn list_routes() {
        let router = Router::new("mysite.dev");
        router.insert("swift-penguin", 51234);
        router.insert("calm-otter", 51235);
        let routes = router.list();
        assert_eq!(routes.len(), 2);
    }
}
```

**Step 3: Add mod to main.rs**

Add `mod proxy;` to `src/main.rs`.

**Step 4: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test router`
Expected: 4 tests pass

**Step 5: Commit**

```bash
git add src/proxy/ src/main.rs
git commit -m "feat: add in-memory router with unit tests"
```

---

## Task 7: Certificate Generation (proxy/cert.rs)

**Files:**
- Create: `src/proxy/cert.rs`

**Step 1: Write cert.rs**

```rust
// src/proxy/cert.rs
use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::path::Path;
use std::time::Duration;

/// Generate a self-signed CA certificate and key pair
pub fn generate_ca() -> Result<(String, String)> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "devproxy Local CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "devproxy");
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    params.not_before = time::OffsetDateTime::now_utc() - Duration::from_secs(3600);
    params.not_after = time::OffsetDateTime::now_utc() + Duration::from_secs(365 * 24 * 3600 * 10);

    let key_pair = KeyPair::generate().context("failed to generate CA key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("failed to self-sign CA certificate")?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// Generate a wildcard TLS certificate signed by the given CA
pub fn generate_wildcard_cert(
    domain: &str,
    ca_cert_pem: &str,
    ca_key_pem: &str,
) -> Result<(String, String)> {
    let ca_key = KeyPair::from_pem(ca_key_pem).context("failed to parse CA key")?;
    let ca_params = CertificateParams::from_ca_cert_pem(ca_cert_pem)
        .context("failed to parse CA cert params")?;
    let ca_cert = ca_params.self_signed(&ca_key).context("failed to reconstruct CA cert")?;

    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, &format!("*.{domain}"));
    params.subject_alt_names = vec![
        SanType::DnsName(format!("*.{domain}").try_into().context("invalid wildcard DNS name")?),
        SanType::DnsName(domain.to_string().try_into().context("invalid DNS name")?),
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = time::OffsetDateTime::now_utc() - Duration::from_secs(3600);
    params.not_after = time::OffsetDateTime::now_utc() + Duration::from_secs(365 * 24 * 3600);

    let key_pair = KeyPair::generate().context("failed to generate TLS key pair")?;
    let cert = params
        .signed_by(&key_pair, &ca_cert, &ca_key)
        .context("failed to sign wildcard certificate")?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// Write PEM data to a file, creating parent directories
pub fn write_pem(path: &Path, pem: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, pem)?;
    Ok(())
}

/// Load TLS certificate and key into rustls ServerConfig
pub fn load_tls_config(cert_path: &Path, key_path: &Path) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls::ServerConfig;
    use rustls_pemfile::{certs, pkcs8_private_keys};
    use std::io::BufReader;
    use std::sync::Arc;

    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("could not open cert file: {}", cert_path.display()))?;
    let key_file = std::fs::File::open(key_path)
        .with_context(|| format!("could not open key file: {}", key_path.display()))?;

    let certs: Vec<_> = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .context("could not parse certificates")?;

    let keys: Vec<_> = pkcs8_private_keys(&mut BufReader::new(key_file))
        .collect::<Result<Vec<_>, _>>()
        .context("could not parse private keys")?;

    let key = keys
        .into_iter()
        .next()
        .context("no private key found in key file")?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()))
        .context("invalid TLS configuration")?;

    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// Trust the CA certificate in the system keychain (macOS only for now)
pub fn trust_ca_in_system(ca_cert_path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("security")
            .args([
                "add-trusted-cert",
                "-d",
                "-r", "trustRoot",
                "-k", "/Library/Keychains/System.keychain",
            ])
            .arg(ca_cert_path)
            .status()
            .context("failed to run security command")?;

        if !status.success() {
            anyhow::bail!("failed to trust CA cert. You may need to run with sudo.");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let dest = Path::new("/usr/local/share/ca-certificates/devproxy-ca.crt");
        std::fs::copy(ca_cert_path, dest)
            .context("failed to copy CA cert. You may need to run with sudo.")?;
        let status = std::process::Command::new("update-ca-certificates")
            .status()
            .context("failed to run update-ca-certificates")?;
        if !status.success() {
            anyhow::bail!("failed to update CA certificates");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_ca_produces_valid_pem() {
        let (cert_pem, key_pem) = generate_ca().unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn generate_wildcard_cert_produces_valid_pem() {
        let (ca_cert, ca_key) = generate_ca().unwrap();
        let (cert_pem, key_pem) = generate_wildcard_cert("mysite.dev", &ca_cert, &ca_key).unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn tls_config_loads_from_generated_certs() {
        let (ca_cert, ca_key) = generate_ca().unwrap();
        let (cert_pem, key_pem) = generate_wildcard_cert("mysite.dev", &ca_cert, &ca_key).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, &cert_pem).unwrap();
        std::fs::write(&key_path, &key_pem).unwrap();

        let result = load_tls_config(&cert_path, &key_path);
        assert!(result.is_ok(), "TLS config should load: {:?}", result.err());
    }
}
```

**Step 2: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test cert`
Expected: 3 tests pass

**Step 3: Commit**

```bash
git add src/proxy/cert.rs
git commit -m "feat: add certificate generation with rcgen and unit tests"
```

---

## Task 8: Docker Event Watcher (proxy/docker.rs)

**Files:**
- Create: `src/proxy/docker.rs`

**Step 1: Write docker.rs**

```rust
// src/proxy/docker.rs
use crate::proxy::router::Router;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Container info from `docker inspect`
#[derive(Debug, Deserialize)]
struct ContainerInspect {
    #[serde(rename = "Config")]
    config: ContainerConfig,
    #[serde(rename = "NetworkSettings")]
    network_settings: NetworkSettings,
}

#[derive(Debug, Deserialize)]
struct ContainerConfig {
    #[serde(rename = "Labels")]
    labels: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct NetworkSettings {
    #[serde(rename = "Ports")]
    ports: std::collections::HashMap<String, Option<Vec<PortBinding>>>,
}

#[derive(Debug, Deserialize)]
struct PortBinding {
    #[serde(rename = "HostIp")]
    _host_ip: Option<String>,
    #[serde(rename = "HostPort")]
    host_port: Option<String>,
}

/// Inspect a container and extract routing info
async fn inspect_container(container_id: &str) -> Result<Option<(String, u16)>> {
    let output = Command::new("docker")
        .args(["inspect", container_id])
        .output()
        .await
        .context("failed to run docker inspect")?;

    if !output.status.success() {
        return Ok(None);
    }

    let json: Vec<ContainerInspect> =
        serde_json::from_slice(&output.stdout).context("failed to parse docker inspect output")?;

    let inspect = match json.into_iter().next() {
        Some(i) => i,
        None => return Ok(None),
    };

    let devproxy_port = match inspect.config.labels.get("devproxy.port") {
        Some(p) => p.clone(),
        None => return Ok(None),
    };

    let slug = match inspect.config.labels.get("com.docker.compose.project") {
        Some(s) => s.clone(),
        None => return Ok(None),
    };

    // Find the host port for the devproxy.port
    let container_port_key = format!("{devproxy_port}/tcp");
    let host_port = inspect
        .network_settings
        .ports
        .get(&container_port_key)
        .and_then(|bindings| bindings.as_ref())
        .and_then(|bindings| bindings.first())
        .and_then(|b| b.host_port.as_ref())
        .and_then(|p| p.parse::<u16>().ok());

    match host_port {
        Some(port) => Ok(Some((slug, port))),
        None => Ok(None),
    }
}

/// Load existing routes from running containers
pub async fn load_routes(router: &Router) -> Result<()> {
    let output = Command::new("docker")
        .args([
            "ps",
            "--filter",
            "label=devproxy.port",
            "--format",
            "{{.ID}}",
        ])
        .output()
        .await
        .context("failed to run docker ps")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let container_id = line.trim();
        if container_id.is_empty() {
            continue;
        }
        if let Ok(Some((slug, port))) = inspect_container(container_id).await {
            eprintln!("  loaded route: {slug} -> 127.0.0.1:{port}");
            router.insert(&slug, port);
        }
    }

    Ok(())
}

/// Watch Docker events and update routes in real-time
pub async fn watch_events(router: &Router) -> Result<()> {
    loop {
        eprintln!("  starting docker event watcher...");
        let result = watch_events_inner(router).await;
        if let Err(e) = result {
            eprintln!("  docker event watcher error: {e}, restarting in 2s...");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
}

async fn watch_events_inner(router: &Router) -> Result<()> {
    let mut child = Command::new("docker")
        .args([
            "events",
            "--filter",
            "label=devproxy.port",
            "--filter",
            "type=container",
            "--format",
            "{{json .}}",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn docker events")?;

    let stdout = child.stdout.take().context("no stdout from docker events")?;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        // Docker events JSON can use different field casing across versions.
        // Parse as generic Value to handle both.
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
            let action = event
                .get("Action")
                .or_else(|| event.get("action"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let container_id = event
                .get("Actor")
                .or_else(|| event.get("actor"))
                .and_then(|a| a.get("ID").or_else(|| a.get("id")))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match action {
                "start" => {
                    if let Ok(Some((slug, port))) = inspect_container(container_id).await {
                        eprintln!("  route added: {slug} -> 127.0.0.1:{port}");
                        router.insert(&slug, port);
                    }
                }
                "die" | "stop" | "kill" => {
                    // Get the project name from event attributes
                    let slug = event
                        .get("Actor")
                        .or_else(|| event.get("actor"))
                        .and_then(|a| a.get("Attributes").or_else(|| a.get("attributes")))
                        .and_then(|attrs| {
                            attrs
                                .get("com.docker.compose.project")
                                .and_then(|v| v.as_str())
                        });

                    if let Some(slug) = slug {
                        eprintln!("  route removed: {slug}");
                        router.remove(slug);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}
```

**Step 2: Run cargo check to verify compilation**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo check`
Expected: Compiles (possibly with dead code warnings, which is fine)

**Step 3: Commit**

```bash
git add src/proxy/docker.rs
git commit -m "feat: add Docker event watcher and route loader"
```

---

## Task 9: Daemon Entry Point (proxy/mod.rs + IPC server)

**Files:**
- Modify: `src/proxy/mod.rs`

**Step 1: Write the full proxy/mod.rs with daemon runner and IPC server**

```rust
// src/proxy/mod.rs
pub mod cert;
pub mod docker;
pub mod router;

use crate::config::Config;
use crate::ipc::{Request, Response};
use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse};
use router::Router;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixListener};

/// Run the daemon: HTTPS proxy + Docker watcher + IPC server
pub async fn run_daemon(port: u16) -> Result<()> {
    let config = Config::load().context("failed to load config. Run `devproxy init` first.")?;
    let router = Router::new(&config.domain);

    // Load TLS config
    let tls_acceptor = cert::load_tls_config(
        &Config::tls_cert_path()?,
        &Config::tls_key_path()?,
    )?;

    // Load existing routes from running containers
    eprintln!("loading existing routes...");
    docker::load_routes(&router).await?;

    // Set up IPC socket
    let socket_path = Config::socket_path()?;
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let ipc_listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("could not bind IPC socket at {}", socket_path.display()))?;
    eprintln!("IPC listening on {}", socket_path.display());

    // Set up HTTPS listener
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .with_context(|| format!("could not bind to port {port}"))?;
    eprintln!("HTTPS proxy listening on :{port}");

    // Run all three tasks
    let r1 = router.clone();
    let r2 = router.clone();
    let r3 = router.clone();

    tokio::select! {
        result = https_proxy_loop(tcp_listener, tls_acceptor, r1) => {
            result.context("HTTPS proxy task failed")?;
        }
        result = docker::watch_events(&r2) => {
            result.context("Docker watcher task failed")?;
        }
        result = ipc_server_loop(ipc_listener, r3) => {
            result.context("IPC server task failed")?;
        }
    }

    Ok(())
}

/// Accept and handle IPC connections
async fn ipc_server_loop(listener: UnixListener, router: Router) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let router = router.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_ipc_connection(stream, &router).await {
                eprintln!("  IPC error: {e}");
            }
        });
    }
}

async fn handle_ipc_connection(
    stream: tokio::net::UnixStream,
    router: &Router,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    buf_reader.read_line(&mut line).await?;

    let request: Request = serde_json::from_str(line.trim())
        .context("could not parse IPC request")?;

    let response = match request {
        Request::Ping => Response::Pong,
        Request::List => Response::Routes {
            routes: router.list(),
        },
    };

    let mut resp_line = serde_json::to_string(&response)?;
    resp_line.push('\n');
    writer.write_all(resp_line.as_bytes()).await?;

    Ok(())
}

/// Accept TLS connections and proxy them
async fn https_proxy_loop(
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    router: Router,
) -> Result<()> {
    loop {
        let (tcp_stream, _addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let router = router.clone();

        tokio::spawn(async move {
            match acceptor.accept(tcp_stream).await {
                Ok(tls_stream) => {
                    let router = router.clone();
                    let service = service_fn(move |req: HyperRequest<Incoming>| {
                        let router = router.clone();
                        async move { handle_request(req, &router).await }
                    });

                    if let Err(e) =
                        http1::Builder::new()
                            .serve_connection(hyper_util::rt::TokioIo::new(tls_stream), service)
                            .await
                    {
                        eprintln!("  HTTP error: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("  TLS handshake error: {e}");
                }
            }
        });
    }
}

/// Handle a single HTTP request by reverse-proxying to the right container
async fn handle_request(
    req: HyperRequest<Incoming>,
    router: &Router,
) -> Result<HyperResponse<Full<Bytes>>, hyper::Error> {
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");

    let host_port = match router.get(host) {
        Some(port) => port,
        None => {
            return Ok(HyperResponse::builder()
                .status(502)
                .body(Full::new(Bytes::from(format!(
                    "devproxy: no route for host '{host}'\n"
                ))))
                .expect("response build"));
        }
    };

    // Build upstream URI
    let path_and_query = req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let upstream_addr = format!("127.0.0.1:{host_port}");

    // Forward the request to the container
    match proxy_to_upstream(&upstream_addr, path_and_query, req).await {
        Ok(resp) => Ok(resp),
        Err(e) => Ok(HyperResponse::builder()
            .status(502)
            .body(Full::new(Bytes::from(format!(
                "devproxy: upstream error: {e}\n"
            ))))
            .expect("response build")),
    }
}

async fn proxy_to_upstream(
    upstream_addr: &str,
    path_and_query: &str,
    incoming_req: HyperRequest<Incoming>,
) -> Result<HyperResponse<Full<Bytes>>> {
    use http_body_util::BodyExt;

    let stream = TcpStream::connect(upstream_addr)
        .await
        .with_context(|| format!("could not connect to upstream at {upstream_addr}"))?;

    let io = hyper_util::rt::TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .context("upstream handshake failed")?;

    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("  upstream connection error: {e}");
        }
    });

    // Build the upstream request preserving method, path, and headers
    let upstream_uri: hyper::Uri = format!("http://{upstream_addr}{path_and_query}")
        .parse()
        .context("invalid upstream URI")?;
    let method = incoming_req.method().clone();
    let headers = incoming_req.headers().clone();

    let body = incoming_req
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("failed to collect body: {e}"))?
        .to_bytes();

    let mut upstream_req = HyperRequest::builder()
        .method(method)
        .uri(upstream_uri)
        .body(Full::new(body))
        .context("failed to build upstream request")?;

    // Copy headers (except host)
    for (name, value) in headers.iter() {
        if name != "host" {
            upstream_req.headers_mut().insert(name.clone(), value.clone());
        }
    }

    let resp = sender
        .send_request(upstream_req)
        .await
        .context("upstream request failed")?;

    let status = resp.status();
    let resp_headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("failed to collect upstream response: {e}"))?
        .to_bytes();

    let mut response = HyperResponse::builder()
        .status(status)
        .body(Full::new(body))
        .context("failed to build response")?;

    for (name, value) in resp_headers.iter() {
        response.headers_mut().insert(name.clone(), value.clone());
    }

    Ok(response)
}
```

**Step 2: Run cargo check**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo check`
Expected: Compiles

**Step 3: Commit**

```bash
git add src/proxy/mod.rs
git commit -m "feat: add daemon with HTTPS proxy, Docker watcher, and IPC server"
```

---

## Task 10: Command Implementations (commands/)

**Files:**
- Create: `src/commands/mod.rs`
- Create: `src/commands/init.rs`
- Create: `src/commands/up.rs`
- Create: `src/commands/down.rs`
- Create: `src/commands/ls.rs`
- Create: `src/commands/open.rs`
- Create: `src/commands/status.rs`
- Create: `src/commands/daemon.rs`
- Modify: `src/main.rs`
- Modify: `.gitignore` (add `.devproxy-project`)

**Step 1: Write commands/mod.rs**

```rust
// src/commands/mod.rs
pub mod daemon;
pub mod down;
pub mod init;
pub mod ls;
pub mod open;
pub mod status;
pub mod up;
```

**Step 2: Write commands/init.rs**

Key behavior:
- `--port` is forwarded to the daemon subprocess.
- `--no-daemon` skips daemon spawn entirely (used by e2e tests).
- Idempotent: skips CA if it exists, skips wildcard cert if it exists. Running `init` twice does not invalidate a running daemon's certs.
- `DEVPROXY_CONFIG_DIR` is forwarded to the daemon subprocess so it uses the same config dir.

```rust
// src/commands/init.rs
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
        cert::write_pem(&ca_key_path, &ca_key_pem)?;
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
        cert::write_pem(&tls_key_path, &tls_key_pem)?;
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

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());

        let child = cmd.spawn().context("could not spawn daemon")?;
        eprintln!("{} daemon started (pid: {})", "ok:".green(), child.id());
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
```

**Step 3: Write commands/up.rs**

Uses `config::find_compose_file()` (shared utility, not duplicated). Writes both `.devproxy-override.yml` and `.devproxy-project`.

```rust
// src/commands/up.rs
use crate::config::{self, Config};
use crate::slugs;
use anyhow::{Context, Result, bail};
use colored::Colorize;

pub fn run() -> Result<()> {
    // Check config exists (implies init was run)
    let config = Config::load().context("run `devproxy init` first")?;

    // Find docker-compose.yml (shared utility from config module)
    let cwd = std::env::current_dir()?;
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_dir = compose_path
        .parent()
        .context("compose file has no parent directory")?;

    eprintln!(
        "found compose file: {}",
        compose_path.display().to_string().cyan()
    );

    // Parse compose file
    let compose = config::parse_compose_file(&compose_path)?;
    let (service_name, container_port) = config::find_devproxy_service(&compose)?;
    eprintln!(
        "service: {}, container port: {}",
        service_name.cyan(),
        container_port.to_string().cyan()
    );

    // Generate slug
    let slug = slugs::generate_slug();
    eprintln!("slug: {}", slug.cyan());

    // Find free port
    let host_port = config::find_free_port()?;
    eprintln!("host port: {}", host_port.to_string().cyan());

    // Write override file (port binding)
    let override_path = config::write_override_file(compose_dir, &service_name, host_port, container_port)?;
    eprintln!(
        "override: {}",
        override_path.display().to_string().cyan()
    );

    // Write project file (slug tracking -- used by `down` and `open`)
    config::write_project_file(compose_dir, &slug)?;

    // Run docker compose up
    let compose_file_name = compose_path
        .file_name()
        .context("no filename")?
        .to_string_lossy();

    let status = std::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose_file_name,
            "-f",
            ".devproxy-override.yml",
            "--project-name",
            &slug,
            "up",
            "-d",
        ])
        .current_dir(compose_dir)
        .status()
        .context("failed to run docker compose")?;

    if !status.success() {
        // Clean up on failure
        let _ = std::fs::remove_file(&override_path);
        let _ = std::fs::remove_file(compose_dir.join(".devproxy-project"));
        bail!("docker compose up failed");
    }

    let url = format!("https://{slug}.{}", config.domain);
    eprintln!();
    eprintln!("{} {}", "->".green().bold(), url.green().bold());

    Ok(())
}
```

**Step 4: Write commands/down.rs**

Uses `config::find_compose_file()` (same shared utility as `up`). Reads `.devproxy-project` to get the slug, then passes `--project-name <slug>` to `docker compose down` so it targets the correct containers.

```rust
// src/commands/down.rs
use crate::config;
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Read the slug from .devproxy-project so we target the right compose project
    let slug = config::read_project_file(&cwd)?;
    eprintln!("project: {}", slug.cyan());

    // Find the compose file name (shared utility from config module)
    let compose_path = config::find_compose_file(&cwd)?;
    let compose_file_name = compose_path
        .file_name()
        .context("no filename")?
        .to_string_lossy()
        .to_string();

    // Run docker compose down with the correct project name
    let status = std::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose_file_name,
            "-f",
            ".devproxy-override.yml",
            "--project-name",
            &slug,
            "down",
        ])
        .current_dir(&cwd)
        .status()
        .context("failed to run docker compose down")?;

    if !status.success() {
        eprintln!("{} docker compose down exited with error", "warn:".yellow());
    }

    // Remove override and project files
    let override_path = cwd.join(".devproxy-override.yml");
    if override_path.exists() {
        std::fs::remove_file(&override_path)?;
        eprintln!("{} removed .devproxy-override.yml", "ok:".green());
    }

    let project_path = cwd.join(".devproxy-project");
    if project_path.exists() {
        std::fs::remove_file(&project_path)?;
        eprintln!("{} removed .devproxy-project", "ok:".green());
    }

    eprintln!("{} project stopped", "ok:".green());
    Ok(())
}
```

**Step 5: Write commands/ls.rs**

```rust
// src/commands/ls.rs
use crate::config::Config;
use crate::ipc::{self, Request, Response};
use anyhow::{Result, bail};
use colored::Colorize;

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    match response {
        Response::Routes { routes } => {
            if routes.is_empty() {
                eprintln!("no active projects");
            } else {
                eprintln!("{:<30} {:<10}", "SLUG".bold(), "PORT".bold());
                for route in &routes {
                    eprintln!(
                        "{:<30} {:<10}",
                        format!("https://{}", route.slug).cyan(),
                        route.port
                    );
                }
                eprintln!();
                eprintln!("{} active project(s)", routes.len());
            }
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        _ => bail!("unexpected response from daemon"),
    }

    Ok(())
}
```

**Step 6: Write commands/status.rs**

```rust
// src/commands/status.rs
use crate::config::Config;
use crate::ipc::{self, Request, Response};
use anyhow::{Result, bail};
use colored::Colorize;

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;

    match ipc::send_request(&socket_path, &Request::Ping).await {
        Ok(Response::Pong) => {
            eprintln!("{} daemon is running", "ok:".green());

            // Also get route count
            if let Ok(Response::Routes { routes }) =
                ipc::send_request(&socket_path, &Request::List).await
            {
                eprintln!("  active routes: {}", routes.len());
            }
        }
        Ok(Response::Error { message }) => bail!("daemon error: {message}"),
        Ok(_) => bail!("unexpected response from daemon"),
        Err(e) => {
            eprintln!("{} daemon is not running: {e}", "error:".red());
            eprintln!("  run `devproxy init` to start it");
        }
    }

    Ok(())
}
```

**Step 7: Write commands/open.rs**

Reads `.devproxy-project` to identify THIS project's slug, then queries daemon for the matching route.

```rust
// src/commands/open.rs
use crate::config::{self, Config};
use crate::ipc::{self, Request, Response};
use anyhow::{Context, Result, bail};
use colored::Colorize;

pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Read slug from .devproxy-project to identify THIS project
    let slug = config::read_project_file(&cwd)?;
    let config = Config::load()?;
    let full_host = format!("{slug}.{}", config.domain);

    // Verify the route is active by querying the daemon
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    match response {
        Response::Routes { routes } => {
            let found = routes.iter().any(|r| r.slug == full_host);
            if !found {
                bail!(
                    "project '{slug}' is not currently routed by the daemon. \
                     Is the container running?"
                );
            }
            let url = format!("https://{full_host}");
            eprintln!("opening {}...", url.cyan());
            open::that(&url).context("could not open browser")?;
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        _ => bail!("unexpected response from daemon"),
    }

    Ok(())
}
```

**Step 8: Write commands/daemon.rs**

```rust
// src/commands/daemon.rs
use crate::proxy;
use anyhow::Result;

pub async fn run(port: u16) -> Result<()> {
    eprintln!("devproxy daemon starting...");
    proxy::run_daemon(port).await
}
```

**Step 9: Update main.rs to wire everything together**

```rust
// src/main.rs
mod cli;
mod commands;
mod config;
mod ipc;
mod proxy;
mod slugs;

use clap::Parser;
use cli::{Cli, Commands};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { domain, port, no_daemon } => commands::init::run(&domain, port, no_daemon),
        Commands::Up => commands::up::run(),
        Commands::Down => commands::down::run(),
        Commands::Ls => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::ls::run())
        }
        Commands::Open => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::open::run())
        }
        Commands::Status => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::status::run())
        }
        Commands::Daemon { port } => {
            tokio::runtime::Runtime::new()?
                .block_on(commands::daemon::run(port))
        }
    }
}
```

**Step 10: Add `.devproxy-project` to `.gitignore`**

Add this line to `.gitignore`:
```
.devproxy-project
```

**Step 11: Run cargo check**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo check`
Expected: Compiles (might have warnings)

**Step 12: Run cargo clippy**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo clippy --all-targets -- -D warnings`
Expected: Passes (fix any warnings)

**Step 13: Commit**

```bash
git add src/commands/ src/main.rs .gitignore
git commit -m "feat: add all CLI command implementations with project file tracking"
```

---

## Task 11: E2E Test Harness Setup

All e2e tests use `DEVPROXY_CONFIG_DIR` for config isolation and each test gets:
- Its own temp directory for config.
- Its own copy of the fixtures directory (so `.devproxy-override.yml` and `.devproxy-project` writes don't collide).
- Its own ephemeral daemon port (via `config::find_free_port` equivalent), so tests can run in parallel without port conflicts.

The `init --no-daemon` flag is used to generate certs cleanly without spawning a daemon. Each test that needs a daemon starts one explicitly on its own unique port.

**Files:**
- Create: `tests/e2e.rs`
- Create: `tests/fixtures/docker-compose.yml`
- Create: `tests/fixtures/Dockerfile`

**Step 1: Create the test fixture -- a minimal HTTP server**

```dockerfile
# tests/fixtures/Dockerfile
FROM python:3.12-alpine
WORKDIR /app
CMD ["python", "-m", "http.server", "3000"]
```

```yaml
# tests/fixtures/docker-compose.yml
services:
  web:
    build: .
    labels:
      - "devproxy.port=3000"
```

**Step 2: Write the e2e test harness**

```rust
// tests/e2e.rs
//! End-to-end tests for devproxy.
//!
//! Requirements:
//! - Docker and Docker Compose must be available
//! - Tests run the daemon on ephemeral high ports -- no sudo needed
//!
//! Test isolation strategy:
//! - DEVPROXY_CONFIG_DIR env var points each test at its own temp config dir.
//!   (dirs::config_dir() on macOS ignores HOME, so DEVPROXY_CONFIG_DIR is the
//!   only reliable way to isolate config.)
//! - Each test copies tests/fixtures/ into its own temp dir so that
//!   .devproxy-override.yml and .devproxy-project writes do not collide.
//! - Each test binds the daemon to a unique ephemeral port, so parallel tests
//!   do not fight over a shared port.
//!
//! Run with: cargo test --test e2e
//! Run full suite: cargo test --test e2e -- --include-ignored

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Helper to get the path to the devproxy binary
fn devproxy_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("devproxy");
    path
}

/// Helper to get the source fixtures directory
fn fixtures_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

const TEST_DOMAIN: &str = "test.devproxy.dev";

/// Find a free ephemeral port. Each test calls this to get its own unique daemon port.
fn find_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Copy the fixtures directory into an isolated temp dir for one test.
/// Returns the path to the copy (which contains docker-compose.yml, Dockerfile, etc).
fn copy_fixtures(test_name: &str) -> PathBuf {
    let dest = std::env::temp_dir().join(format!("devproxy-fixtures-{test_name}-{}", std::process::id()));
    if dest.exists() {
        std::fs::remove_dir_all(&dest).unwrap();
    }
    std::fs::create_dir_all(&dest).unwrap();

    let src = fixtures_src_dir();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        let dest_path = dest.join(entry.file_name());
        std::fs::copy(entry.path(), &dest_path).unwrap();
    }
    dest
}

/// Create an isolated test config directory and generate certs using `init --no-daemon`.
/// Returns the path to the config directory (to be set as DEVPROXY_CONFIG_DIR).
fn create_test_config_dir(test_name: &str) -> PathBuf {
    let config_dir = std::env::temp_dir().join(format!("devproxy-config-{test_name}-{}", std::process::id()));
    if config_dir.exists() {
        std::fs::remove_dir_all(&config_dir).unwrap();
    }
    std::fs::create_dir_all(&config_dir).unwrap();

    // Generate certs without spawning a daemon (--no-daemon)
    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run devproxy init --no-daemon");

    assert!(
        output.status.success(),
        "init --no-daemon should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify certs were created
    assert!(config_dir.join("ca-cert.pem").exists(), "CA cert should exist after init");
    assert!(config_dir.join("tls-cert.pem").exists(), "TLS cert should exist after init");
    assert!(config_dir.join("config.json").exists(), "config should exist after init");

    config_dir
}

/// Guard that kills the daemon on drop and cleans up the config dir
struct DaemonGuard {
    child: Child,
    config_dir: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

/// Start a daemon on the given port with the given config dir.
/// Waits until the IPC socket is connectable.
fn start_test_daemon(config_dir: &Path, port: u16) -> DaemonGuard {
    let child = Command::new(devproxy_bin())
        .args(["daemon", "--port", &port.to_string()])
        .env("DEVPROXY_CONFIG_DIR", config_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start daemon");

    let guard = DaemonGuard {
        child,
        config_dir: config_dir.to_path_buf(),
    };

    // Wait for IPC socket to become connectable
    let socket_path = config_dir.join("devproxy.sock");
    for _ in 0..50 {
        if socket_path.exists() {
            if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    panic!("daemon did not start within 5 seconds (socket: {})", socket_path.display());
}

/// Guard that runs docker compose down on drop and cleans up the fixtures copy
struct ComposeGuard {
    project_name: String,
    compose_dir: PathBuf,
}

impl Drop for ComposeGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args([
                "compose",
                "--project-name",
                &self.project_name,
                "down",
                "--remove-orphans",
                "--timeout",
                "5",
            ])
            .current_dir(&self.compose_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // Clean up fixtures copy
        let _ = std::fs::remove_dir_all(&self.compose_dir);
    }
}

// ---- Non-Docker tests (always run) ----------------------------------------

#[test]
fn test_cli_help() {
    let output = Command::new(devproxy_bin())
        .arg("--help")
        .output()
        .expect("failed to run devproxy --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("devproxy"));
    assert!(stdout.contains("init"));
    assert!(stdout.contains("up"));
    assert!(stdout.contains("down"));
    assert!(stdout.contains("ls"));
    assert!(stdout.contains("status"));
}

#[test]
fn test_init_generates_certs() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-init-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init");

    assert!(output.status.success(), "init should succeed");
    assert!(config_dir.join("ca-cert.pem").exists(), "CA cert should exist");
    assert!(config_dir.join("ca-key.pem").exists(), "CA key should exist");
    assert!(config_dir.join("tls-cert.pem").exists(), "TLS cert should exist");
    assert!(config_dir.join("tls-key.pem").exists(), "TLS key should exist");
    assert!(config_dir.join("config.json").exists(), "config should exist");

    // Verify idempotency: running init again should succeed and not error
    let output2 = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN, "--no-daemon"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy init a second time");

    assert!(output2.status.success(), "init should be idempotent");
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(stderr2.contains("already exists"), "should report certs already exist");

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn test_status_without_daemon() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-norun-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    let output = Command::new(devproxy_bin())
        .args(["status"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run status");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not running") || stderr.contains("could not connect"),
        "should report daemon not running: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn test_up_without_label() {
    let config_dir = std::env::temp_dir().join(format!("devproxy-nolabel-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{TEST_DOMAIN}"}}"#),
    )
    .unwrap();

    // Create a compose file without devproxy.port
    let test_dir = std::env::temp_dir().join(format!("devproxy-nolabel-project-{}", std::process::id()));
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::write(
        test_dir.join("docker-compose.yml"),
        "services:\n  web:\n    image: alpine\n",
    )
    .unwrap();

    let output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&test_dir)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run up");

    assert!(
        !output.status.success(),
        "up should fail without devproxy.port label"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no service"),
        "should mention no devproxy.port label: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
    let _ = std::fs::remove_dir_all(&test_dir);
}

// ---- Docker-dependent tests (run with --include-ignored) -------------------

/// Full e2e: init -> up -> curl through proxy -> ls -> status -> down
#[test]
#[ignore] // Run with: cargo test --test e2e -- --ignored
fn test_full_e2e_workflow() {
    let config_dir = create_test_config_dir("e2e");
    let daemon_port = find_free_port();
    let _daemon = start_test_daemon(&config_dir, daemon_port);

    let fixtures = copy_fixtures("e2e");

    // Build fixture image (in the copy dir; Dockerfile is there)
    let build_status = Command::new("docker")
        .args(["compose", "build"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to build fixture");
    assert!(build_status.success(), "fixture build should succeed");

    // Up
    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy up");

    let up_stderr = String::from_utf8_lossy(&up_output.stderr);
    eprintln!("up output: {up_stderr}");
    assert!(up_output.status.success(), "devproxy up should succeed: {up_stderr}");

    // Extract slug from output (look for "-> https://<slug>.test.devproxy.dev")
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| {
            l.split("https://")
                .nth(1)
                .and_then(|s| s.split('.').next())
        })
        .expect("should find slug in up output");

    // Verify .devproxy-project was written with the correct slug
    let project_file = fixtures.join(".devproxy-project");
    assert!(project_file.exists(), ".devproxy-project should exist after up");
    let saved_slug = std::fs::read_to_string(&project_file).unwrap();
    assert_eq!(saved_slug.trim(), slug, ".devproxy-project should contain the slug");

    let _compose_guard = ComposeGuard {
        project_name: slug.to_string(),
        compose_dir: fixtures.clone(),
    };

    // Wait for container to be ready
    std::thread::sleep(Duration::from_secs(3));

    // Status check
    let status_output = Command::new(devproxy_bin())
        .args(["status"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy status");
    let status_stderr = String::from_utf8_lossy(&status_output.stderr);
    assert!(
        status_stderr.contains("running"),
        "daemon should be running: {status_stderr}"
    );

    // Ls check
    let ls_output = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy ls");
    let ls_stderr = String::from_utf8_lossy(&ls_output.stderr);
    assert!(
        ls_stderr.contains(slug),
        "ls should show our slug '{slug}': {ls_stderr}"
    );

    // Curl through the proxy (--resolve bypasses DNS, --cacert trusts our test CA)
    let ca_cert_path = config_dir.join("ca-cert.pem");
    let host = format!("{slug}.{TEST_DOMAIN}");
    let url = format!("https://{host}:{daemon_port}/");

    let curl_output = Command::new("curl")
        .args([
            "-s",
            "-f",
            "--max-time",
            "5",
            "--resolve",
            &format!("{host}:{daemon_port}:127.0.0.1"),
            "--cacert",
            &ca_cert_path.to_string_lossy(),
            &url,
        ])
        .output()
        .expect("failed to run curl");

    assert!(
        curl_output.status.success(),
        "curl should succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&curl_output.stdout),
        String::from_utf8_lossy(&curl_output.stderr)
    );

    // Down (reads .devproxy-project to get slug, passes --project-name to compose)
    let down_output = Command::new(devproxy_bin())
        .args(["down"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to run devproxy down");
    let down_stderr = String::from_utf8_lossy(&down_output.stderr);
    assert!(down_output.status.success(), "devproxy down should succeed: {down_stderr}");

    // Verify cleanup files are gone
    assert!(!fixtures.join(".devproxy-project").exists(), ".devproxy-project should be removed after down");
    assert!(!fixtures.join(".devproxy-override.yml").exists(), ".devproxy-override.yml should be removed after down");
}

/// Test self-healing: kill container externally -> route removed from daemon
#[test]
#[ignore]
fn test_self_healing_route_removed_on_container_die() {
    let config_dir = create_test_config_dir("heal");
    let daemon_port = find_free_port();
    let _daemon = start_test_daemon(&config_dir, daemon_port);

    let fixtures = copy_fixtures("heal");

    // Build + Up
    let _ = Command::new("docker")
        .args(["compose", "build"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to up");

    let up_stderr = String::from_utf8_lossy(&up_output.stderr);
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| l.split("https://").nth(1).and_then(|s| s.split('.').next()))
        .expect("should find slug");

    let _compose_guard = ComposeGuard {
        project_name: slug.to_string(),
        compose_dir: fixtures.clone(),
    };

    std::thread::sleep(Duration::from_secs(3));

    // Verify route exists
    let ls_before = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to ls");
    let ls_before_stderr = String::from_utf8_lossy(&ls_before.stderr);
    assert!(ls_before_stderr.contains(slug), "route should exist before kill: {ls_before_stderr}");

    // Kill container externally (not via devproxy)
    let kill_status = Command::new("docker")
        .args(["compose", "--project-name", slug, "kill"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to kill");
    assert!(kill_status.success());

    // Wait for event watcher to process the die event
    std::thread::sleep(Duration::from_secs(3));

    // Check ls -- route should be gone
    let ls_after = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to ls");
    let ls_after_stderr = String::from_utf8_lossy(&ls_after.stderr);
    assert!(
        !ls_after_stderr.contains(slug) || ls_after_stderr.contains("no active"),
        "route should be removed after external kill: {ls_after_stderr}"
    );
}

/// Test daemon restart: routes rebuild from running containers
#[test]
#[ignore]
fn test_daemon_restart_rebuilds_routes() {
    let config_dir = create_test_config_dir("restart");
    let daemon_port = find_free_port();
    let daemon = start_test_daemon(&config_dir, daemon_port);

    let fixtures = copy_fixtures("restart");

    // Build + Up
    let _ = Command::new("docker")
        .args(["compose", "build"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to up");

    let up_stderr = String::from_utf8_lossy(&up_output.stderr);
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| l.split("https://").nth(1).and_then(|s| s.split('.').next()))
        .expect("should find slug");

    let _compose_guard = ComposeGuard {
        project_name: slug.to_string(),
        compose_dir: fixtures.clone(),
    };

    std::thread::sleep(Duration::from_secs(2));

    // Kill the daemon (not the container) -- DaemonGuard::drop kills process and cleans config_dir,
    // but we need the config_dir to survive for the new daemon. So we kill manually.
    let mut daemon = daemon;
    let _ = daemon.child.kill();
    let _ = daemon.child.wait();
    // Prevent DaemonGuard::drop from deleting config_dir by forgetting it
    std::mem::forget(daemon);

    std::thread::sleep(Duration::from_millis(500));

    // Start a new daemon on a fresh port -- it should rebuild routes from running containers
    let daemon_port2 = find_free_port();
    let _daemon2 = start_test_daemon(&config_dir, daemon_port2);

    // Check that the route was rebuilt
    let ls_output = Command::new(devproxy_bin())
        .args(["ls"])
        .env("DEVPROXY_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to ls");
    let ls_stderr = String::from_utf8_lossy(&ls_output.stderr);
    assert!(
        ls_stderr.contains(slug),
        "route should be rebuilt after daemon restart: {ls_stderr}"
    );
}
```

**Step 3: Update justfile for e2e tests**

Add to `justfile`:
```
# Run e2e tests (requires Docker)
e2e:
    cargo test --test e2e -- --include-ignored --nocapture
```

**Step 4: Run the non-ignored tests first**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test --test e2e`
Expected: `test_cli_help`, `test_init_generates_certs`, `test_status_without_daemon`, `test_up_without_label` pass; ignored tests skipped

**Step 5: Commit**

```bash
git add tests/ justfile
git commit -m "feat: add e2e test harness with per-test isolation"
```

---

## Task 12: Build, Fix, and Verify

This is the integration phase. All code is written but likely has compilation errors.

**Step 1: Run cargo check and fix all compilation errors**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo check 2>&1`

Fix all errors iteratively. Common expected issues:
- Import paths needing adjustment
- Type mismatches between hyper 1.x APIs
- Missing trait implementations
- Lifetime issues

**Step 2: Run cargo clippy and fix all warnings**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo clippy --all-targets -- -D warnings`

**Step 3: Run all unit tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test`
Expected: All unit tests pass (slugs, config, ipc, router, cert)

**Step 4: Run CLI help to verify binary works**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo run -- --help`
Expected: Shows help output with all commands

**Step 5: Run non-ignored e2e tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test --test e2e`
Expected: `test_cli_help`, `test_init_generates_certs`, `test_status_without_daemon`, `test_up_without_label` pass

**Step 6: Commit fixes**

```bash
git add -A
git commit -m "fix: resolve compilation errors and clippy warnings"
```

---

## Task 13: Full E2E Test Run

**Step 1: Build the test fixture Docker image**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy/tests/fixtures && docker compose build`
Expected: Image builds successfully

**Step 2: Run the full e2e test suite including ignored tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test --test e2e -- --include-ignored --nocapture`
Expected: All tests pass

**Step 3: Fix any failures and commit**

```bash
git add -A
git commit -m "fix: e2e test fixes"
```

---

## Task 14: Final Polish

**Step 1: Run the full test suite**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && just check`
Expected: clippy + all tests pass

**Step 2: Build release binary**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && just build`
Expected: Release binary built at `target/release/devproxy`

**Step 3: Verify release binary**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && ./target/release/devproxy --help`
Expected: Shows help

**Step 4: Commit any final changes**

```bash
git add -A
git commit -m "chore: final polish and release build verification"
```
