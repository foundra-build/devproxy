# devproxy — Full Build Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task.

**Goal:** Build devproxy from scratch — a single Rust binary that provides local HTTPS dev subdomains for Docker Compose projects, with an e2e test harness.

**Architecture:** A CLI binary (clap) that manages a background daemon process. The daemon runs two async tasks via `tokio::join!`: an HTTPS reverse proxy (tokio-rustls + hyper) and a Docker event watcher. CLI communicates with the daemon over a Unix domain socket using JSON-line IPC. Docker is the sole source of truth — no persistent state files.

**Tech Stack:** Rust 2024 edition, clap 4, tokio, hyper 1, hyper-util, tokio-rustls 0.26, rustls 0.23, rcgen, serde/serde_json, serde_yaml, anyhow, colored, rand, dirs, open.

**Key design decision:** hyper 0.14 (from spec) is legacy. We use hyper 1.x + hyper-util which is the current stable API. tokio-rustls 0.26 + rustls 0.23 match. The `--port` flag on the daemon enables e2e testing on high ports without sudo.

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
        Commands::Init { domain } => {
            eprintln!("init: domain={domain}");
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

**Step 1: Write the failing test**

Add to `src/slugs.rs`:

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

/// Global devproxy configuration, stored at ~/.config/devproxy/config.json
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub domain: String,
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
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

/// Write the port-override compose file
pub fn write_override_file(dir: &Path, service_name: &str, host_port: u16, container_port: u16) -> Result<PathBuf> {
    let path = dir.join(".devproxy-override.yml");
    let content = format!(
        "services:\n  {service_name}:\n    ports:\n      - \"{host_port}:{container_port}\"\n"
    );
    std::fs::write(&path, &content)?;
    Ok(path)
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
}
```

**Step 2: Add mod to main.rs**

Add `mod config;` to `src/main.rs`.

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test config`
Expected: 5 tests pass

**Step 4: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: add config module with compose parsing and unit tests"
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

Note: rcgen 0.13 uses the `time` crate internally. We need to add `time` to dependencies.

**Step 2: Add `time` to Cargo.toml**

Add to `[dependencies]` in Cargo.toml:
```toml
time = "0.3"
```

**Step 3: Run tests**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test cert`
Expected: 3 tests pass

**Step 4: Commit**

```bash
git add src/proxy/cert.rs Cargo.toml Cargo.lock
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

/// Docker event from `docker events --format json`
#[derive(Debug, Deserialize)]
struct DockerEvent {
    #[serde(rename = "Action")]
    action: Option<String>,
    #[serde(rename = "Actor")]
    actor: Option<EventActor>,
    // Also handle lowercase (docker versions differ)
    action_lower: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventActor {
    #[serde(rename = "ID")]
    id: Option<String>,
    #[serde(rename = "Attributes")]
    attributes: Option<std::collections::HashMap<String, String>>,
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
        // Docker events JSON can use different field casing across versions
        // Try to parse the event
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
                    // Try to get the project name from event attributes
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
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse};
use router::Router;
use std::path::PathBuf;
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

    // Build upstream request
    let uri = format!(
        "http://127.0.0.1:{host_port}{}",
        req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
    );

    // Forward the request to the container
    match proxy_to_upstream(&uri, req).await {
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
    uri: &str,
    _incoming_req: HyperRequest<Incoming>,
) -> Result<HyperResponse<Full<Bytes>>> {
    let stream = TcpStream::connect(format!(
        "127.0.0.1:{}",
        uri.split("//127.0.0.1:")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .unwrap_or("0")
    ))
    .await
    .context("could not connect to upstream")?;

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
    let upstream_uri: hyper::Uri = uri.parse().context("invalid upstream URI")?;
    let method = _incoming_req.method().clone();
    let headers = _incoming_req.headers().clone();

    let body = _incoming_req
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
    let headers = resp.headers().clone();
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

    for (name, value) in headers.iter() {
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

```rust
// src/commands/init.rs
use crate::config::Config;
use crate::proxy::cert;
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run(domain: &str) -> Result<()> {
    let config = Config { domain: domain.to_string() };

    // Create config directory
    let config_dir = Config::config_dir()?;
    std::fs::create_dir_all(&config_dir)?;

    // Generate CA if it doesn't exist
    let ca_cert_path = Config::ca_cert_path()?;
    let ca_key_path = Config::ca_key_path()?;

    if ca_cert_path.exists() && ca_key_path.exists() {
        eprintln!("{} CA certificate already exists", "✓".green());
    } else {
        eprintln!("generating CA certificate...");
        let (ca_cert_pem, ca_key_pem) = cert::generate_ca()?;
        cert::write_pem(&ca_cert_path, &ca_cert_pem)?;
        cert::write_pem(&ca_key_path, &ca_key_pem)?;
        eprintln!("{} CA certificate generated", "✓".green());

        // Trust the CA
        eprintln!("trusting CA in system keychain (may require sudo)...");
        match cert::trust_ca_in_system(&ca_cert_path) {
            Ok(()) => eprintln!("{} CA trusted in system keychain", "✓".green()),
            Err(e) => {
                eprintln!(
                    "{} could not trust CA automatically: {e}",
                    "⚠".yellow()
                );
                eprintln!(
                    "  manually trust: {}",
                    ca_cert_path.display().to_string().cyan()
                );
            }
        }
    }

    // Generate wildcard cert
    let tls_cert_path = Config::tls_cert_path()?;
    let tls_key_path = Config::tls_key_path()?;

    let ca_cert_pem = std::fs::read_to_string(&ca_cert_path)?;
    let ca_key_pem = std::fs::read_to_string(&ca_key_path)?;

    eprintln!("generating wildcard TLS certificate for *.{domain}...");
    let (tls_cert_pem, tls_key_pem) = cert::generate_wildcard_cert(domain, &ca_cert_pem, &ca_key_pem)?;
    cert::write_pem(&tls_cert_path, &tls_cert_pem)?;
    cert::write_pem(&tls_key_path, &tls_key_pem)?;
    eprintln!("{} TLS certificate generated", "✓".green());

    // Save config
    config.save()?;
    eprintln!("{} config saved", "✓".green());

    // Start daemon
    eprintln!("starting daemon...");
    let exe = std::env::current_exe().context("could not determine binary path")?;
    let child = std::process::Command::new(exe)
        .args(["daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("could not spawn daemon")?;

    eprintln!("{} daemon started (pid: {})", "✓".green(), child.id());

    eprintln!();
    eprintln!("{}", "Setup complete!".green().bold());
    eprintln!();
    eprintln!("Next steps:");
    eprintln!("  1. Set up wildcard DNS for *.{domain} → 127.0.0.1");
    eprintln!("     macOS: brew install dnsmasq");
    eprintln!("     Quick: echo 'address=/.{domain}/127.0.0.1' >> /opt/homebrew/etc/dnsmasq.conf");
    eprintln!("  2. Add a devproxy.port label to your docker-compose.yml");
    eprintln!("  3. Run: devproxy up");

    Ok(())
}
```

**Step 3: Write commands/up.rs**

```rust
// src/commands/up.rs
use crate::config::{self, Config};
use crate::slugs;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::path::Path;

pub fn run() -> Result<()> {
    // Check daemon is running
    let config = Config::load().context("run `devproxy init` first")?;

    // Find docker-compose.yml
    let compose_path = find_compose_file()?;
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

    // Write override file
    let override_path = config::write_override_file(compose_dir, &service_name, host_port, container_port)?;
    eprintln!(
        "override: {}",
        override_path.display().to_string().cyan()
    );

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
        // Clean up override file on failure
        let _ = std::fs::remove_file(&override_path);
        bail!("docker compose up failed");
    }

    let url = format!("https://{slug}.{}", config.domain);
    eprintln!();
    eprintln!("{} {}", "→".green().bold(), url.green().bold());

    Ok(())
}

fn find_compose_file() -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    for name in &["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"] {
        let path = cwd.join(name);
        if path.exists() {
            return Ok(path);
        }
    }
    bail!(
        "no docker-compose.yml found in {}",
        cwd.display()
    )
}
```

**Step 4: Write commands/down.rs**

```rust
// src/commands/down.rs
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Run docker compose down
    let status = std::process::Command::new("docker")
        .args(["compose", "down"])
        .current_dir(&cwd)
        .status()
        .context("failed to run docker compose down")?;

    if !status.success() {
        eprintln!("{} docker compose down exited with error", "⚠".yellow());
    }

    // Remove override file
    let override_path = cwd.join(".devproxy-override.yml");
    if override_path.exists() {
        std::fs::remove_file(&override_path)?;
        eprintln!("{} removed {}", "✓".green(), override_path.display());
    }

    eprintln!("{} project stopped", "✓".green());
    Ok(())
}
```

**Step 5: Write commands/ls.rs**

```rust
// src/commands/ls.rs
use crate::config::Config;
use crate::ipc::{self, Request, Response};
use anyhow::{Context, Result, bail};
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
            eprintln!("{} daemon is running", "✓".green());

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
            eprintln!("{} daemon is not running: {e}", "✗".red());
            eprintln!("  run `devproxy init` to start it");
        }
    }

    Ok(())
}
```

**Step 7: Write commands/open.rs**

```rust
// src/commands/open.rs
use crate::config::Config;
use crate::ipc::{self, Request, Response};
use anyhow::{Context, Result, bail};
use colored::Colorize;

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    match response {
        Response::Routes { routes } => {
            if routes.is_empty() {
                bail!("no active projects");
            }
            // For now, open the first route
            // TODO: match by cwd project name
            let route = &routes[0];
            let url = format!("https://{}", route.slug);
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
        Commands::Init { domain } => commands::init::run(&domain),
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

**Step 10: Run cargo check**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo check`
Expected: Compiles (might have warnings)

**Step 11: Run cargo clippy**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo clippy --all-targets -- -D warnings`
Expected: Passes (fix any warnings)

**Step 12: Commit**

```bash
git add src/commands/ src/main.rs
git commit -m "feat: add all CLI command implementations"
```

---

## Task 11: E2E Test Harness Setup

**Files:**
- Create: `tests/e2e/mod.rs` — (not needed, use `tests/e2e.rs`)
- Create: `tests/e2e.rs`
- Create: `tests/fixtures/docker-compose.yml`
- Create: `tests/fixtures/Dockerfile`

**Step 1: Create the test fixture — a minimal HTTP server**

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
//! - Tests run the daemon on a high port (8443) — no sudo needed
//!
//! Run with: cargo test --test e2e

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Helper to get the path to the devproxy binary
fn devproxy_bin() -> PathBuf {
    // cargo test builds to target/debug/devproxy
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("devproxy");
    path
}

/// Helper to get the fixtures directory
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Test daemon port
const DAEMON_PORT: u16 = 8443;
const TEST_DOMAIN: &str = "test.devproxy.dev";

/// Guards for cleanup
struct DaemonGuard {
    child: Child,
    config_dir: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Clean up config dir
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

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

        // Clean up override file
        let _ = std::fs::remove_file(self.compose_dir.join(".devproxy-override.yml"));
    }
}

/// Set up a temporary config directory and generate certs for testing
fn setup_test_config() -> PathBuf {
    let config_dir = std::env::temp_dir().join(format!("devproxy-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    // Set XDG_CONFIG_HOME so devproxy uses our test config dir
    // Note: devproxy uses dirs::config_dir() which respects this on Linux
    // On macOS, we'll need to override differently — we'll use a symlink approach
    // or pass config via env var. For now, let's generate certs manually.

    config_dir
}

/// Generate test certificates directly using the devproxy cert module
fn generate_test_certs(config_dir: &Path) {
    // We'll shell out to our binary with a custom HOME to isolate config
    // Actually, let's just write the config and certs directly

    // For testing, we generate certs in a subprocess
    let bin = devproxy_bin();

    // Create config.json manually
    let config_json = format!(r#"{{"domain":"{}"}}"#, TEST_DOMAIN);
    std::fs::write(config_dir.join("config.json"), config_json).unwrap();

    // Generate certs using our library (we can't import the lib directly from integration tests
    // easily, so we'll use openssl or rcgen as a dev dependency — but simpler: just call the
    // init command with a modified config dir)

    // Since we can't easily redirect dirs::config_dir(), we'll create a wrapper script
    // or use a more direct approach. Let's use rcgen directly in the test.

    // Actually the cleanest approach: devproxy init writes to dirs::config_dir()/devproxy.
    // We can set HOME env var to control this.
    // On macOS: dirs::config_dir() = $HOME/Library/Application Support
    // On Linux: dirs::config_dir() = $XDG_CONFIG_HOME or $HOME/.config

    // We'll set HOME and XDG_CONFIG_HOME to redirect config
}

/// Start the daemon and wait for it to be ready
fn start_daemon(config_dir: &Path) -> DaemonGuard {
    let bin = devproxy_bin();

    // Use HOME redirection to isolate config
    let home_dir = config_dir.parent().unwrap().join(
        format!("devproxy-home-{}", std::process::id())
    );

    #[cfg(target_os = "macos")]
    {
        // On macOS, dirs::config_dir() = $HOME/Library/Application Support
        let macos_config = home_dir.join("Library/Application Support/devproxy");
        std::fs::create_dir_all(&macos_config).unwrap();

        // Copy config files
        for entry in std::fs::read_dir(config_dir).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), macos_config.join(entry.file_name())).unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    {
        let linux_config = home_dir.join(".config/devproxy");
        std::fs::create_dir_all(&linux_config).unwrap();

        for entry in std::fs::read_dir(config_dir).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), linux_config.join(entry.file_name())).unwrap();
        }
    }

    let child = Command::new(&bin)
        .args(["daemon", "--port", &DAEMON_PORT.to_string()])
        .env("HOME", &home_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start daemon");

    let guard = DaemonGuard {
        child,
        config_dir: home_dir,
    };

    // Wait for daemon to be ready by checking IPC socket
    let socket_path = {
        #[cfg(target_os = "macos")]
        {
            guard.config_dir.join("Library/Application Support/devproxy/devproxy.sock")
        }
        #[cfg(target_os = "linux")]
        {
            guard.config_dir.join(".config/devproxy/devproxy.sock")
        }
    };

    for _ in 0..50 {
        if socket_path.exists() {
            // Try connecting
            if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    panic!("daemon did not start within 5 seconds");
}

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
    let home_dir = std::env::temp_dir().join(format!("devproxy-init-test-{}", std::process::id()));
    std::fs::create_dir_all(&home_dir).unwrap();

    let output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN])
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run devproxy init");

    // Check that cert files were created
    #[cfg(target_os = "macos")]
    let config_dir = home_dir.join("Library/Application Support/devproxy");
    #[cfg(target_os = "linux")]
    let config_dir = home_dir.join(".config/devproxy");

    assert!(config_dir.join("ca-cert.pem").exists(), "CA cert should exist");
    assert!(config_dir.join("ca-key.pem").exists(), "CA key should exist");
    assert!(config_dir.join("tls-cert.pem").exists(), "TLS cert should exist");
    assert!(config_dir.join("tls-key.pem").exists(), "TLS key should exist");
    assert!(config_dir.join("config.json").exists(), "config should exist");

    // Kill the daemon that init spawned
    let sock = config_dir.join("devproxy.sock");
    // Give daemon a moment to start
    std::thread::sleep(Duration::from_millis(500));

    // Clean up
    let _ = std::fs::remove_dir_all(&home_dir);
}

// Full e2e test: init → up → proxy request → down
// This test requires Docker and takes longer
#[test]
#[ignore] // Run with: cargo test --test e2e -- --ignored
fn test_full_e2e_workflow() {
    // 1. Init (generate certs, start daemon)
    let home_dir = std::env::temp_dir().join(format!("devproxy-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&home_dir).unwrap();

    let _output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN])
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run devproxy init");

    // Wait for daemon
    std::thread::sleep(Duration::from_secs(1));

    // 2. Build fixture image first
    let fixtures = fixtures_dir();
    let build_status = Command::new("docker")
        .args(["compose", "build"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to build fixture");
    assert!(build_status.success(), "fixture build should succeed");

    // 3. Up (run devproxy up in fixtures dir)
    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run devproxy up");

    let up_stderr = String::from_utf8_lossy(&up_output.stderr);
    eprintln!("up output: {up_stderr}");

    // Extract slug from output (look for the URL line)
    let slug = up_stderr
        .lines()
        .find(|l| l.contains(&format!(".{TEST_DOMAIN}")))
        .and_then(|l| {
            l.split("https://")
                .nth(1)
                .and_then(|s| s.split('.').next())
        })
        .expect("should find slug in up output");

    let _compose_guard = ComposeGuard {
        project_name: slug.to_string(),
        compose_dir: fixtures.clone(),
    };

    // Wait for container to be ready
    std::thread::sleep(Duration::from_secs(2));

    // 4. Status check
    let status_output = Command::new(devproxy_bin())
        .args(["status"])
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run devproxy status");
    let status_stderr = String::from_utf8_lossy(&status_output.stderr);
    assert!(
        status_stderr.contains("running"),
        "daemon should be running: {status_stderr}"
    );

    // 5. Ls check
    let ls_output = Command::new(devproxy_bin())
        .args(["ls"])
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run devproxy ls");
    let ls_stderr = String::from_utf8_lossy(&ls_output.stderr);
    assert!(
        ls_stderr.contains(slug),
        "ls should show our slug: {ls_stderr}"
    );

    // 6. Curl through the proxy (using --resolve to bypass DNS)
    // Read CA cert for TLS verification
    #[cfg(target_os = "macos")]
    let config_dir = home_dir.join("Library/Application Support/devproxy");
    #[cfg(target_os = "linux")]
    let config_dir = home_dir.join(".config/devproxy");

    let ca_cert_path = config_dir.join("ca-cert.pem");
    let host = format!("{slug}.{TEST_DOMAIN}");
    let url = format!("https://{host}:{DAEMON_PORT}/");

    let curl_output = Command::new("curl")
        .args([
            "-s",
            "--resolve",
            &format!("{host}:{DAEMON_PORT}:127.0.0.1"),
            "--cacert",
            &ca_cert_path.to_string_lossy(),
            &url,
        ])
        .output()
        .expect("failed to run curl");

    let curl_stdout = String::from_utf8_lossy(&curl_output.stdout);
    // Python http.server returns a directory listing HTML page
    assert!(
        curl_output.status.success(),
        "curl should succeed: stdout={curl_stdout}, stderr={}",
        String::from_utf8_lossy(&curl_output.stderr)
    );

    // 7. Down
    let _down_output = Command::new(devproxy_bin())
        .args(["down"])
        .current_dir(&fixtures)
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run devproxy down");

    // Clean up
    let _ = std::fs::remove_dir_all(&home_dir);
}

// Test self-healing: kill container externally → route removed
#[test]
#[ignore]
fn test_self_healing_route_removed_on_container_die() {
    let home_dir = std::env::temp_dir().join(format!("devproxy-heal-{}", std::process::id()));
    std::fs::create_dir_all(&home_dir).unwrap();

    // Init
    let _output = Command::new(devproxy_bin())
        .args(["init", "--domain", TEST_DOMAIN])
        .env("HOME", &home_dir)
        .output()
        .expect("failed to init");
    std::thread::sleep(Duration::from_secs(1));

    let fixtures = fixtures_dir();

    // Up
    let up_output = Command::new(devproxy_bin())
        .args(["up"])
        .current_dir(&fixtures)
        .env("HOME", &home_dir)
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

    // Kill container externally (not via devproxy)
    let kill_status = Command::new("docker")
        .args(["compose", "--project-name", slug, "kill"])
        .current_dir(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to kill");
    assert!(kill_status.success());

    // Wait for event watcher to process
    std::thread::sleep(Duration::from_secs(2));

    // Check ls — route should be gone
    let ls_output = Command::new(devproxy_bin())
        .args(["ls"])
        .env("HOME", &home_dir)
        .output()
        .expect("failed to ls");
    let ls_stderr = String::from_utf8_lossy(&ls_output.stderr);
    assert!(
        !ls_stderr.contains(slug) || ls_stderr.contains("no active"),
        "route should be removed after kill: {ls_stderr}"
    );

    let _ = std::fs::remove_dir_all(&home_dir);
}

// Test error: daemon not running
#[test]
fn test_status_without_daemon() {
    let home_dir = std::env::temp_dir().join(format!("devproxy-norun-{}", std::process::id()));
    std::fs::create_dir_all(&home_dir).unwrap();

    // Write a minimal config so status doesn't fail on config load
    #[cfg(target_os = "macos")]
    let config_dir = home_dir.join("Library/Application Support/devproxy");
    #[cfg(target_os = "linux")]
    let config_dir = home_dir.join(".config/devproxy");

    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{}"}}"#, TEST_DOMAIN),
    )
    .unwrap();

    let output = Command::new(devproxy_bin())
        .args(["status"])
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run status");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not running") || stderr.contains("could not connect"),
        "should report daemon not running: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&home_dir);
}

// Test error: missing devproxy.port label
#[test]
fn test_up_without_label() {
    let home_dir = std::env::temp_dir().join(format!("devproxy-nolabel-{}", std::process::id()));
    std::fs::create_dir_all(&home_dir).unwrap();

    // Write config
    #[cfg(target_os = "macos")]
    let config_dir = home_dir.join("Library/Application Support/devproxy");
    #[cfg(target_os = "linux")]
    let config_dir = home_dir.join(".config/devproxy");

    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        format!(r#"{{"domain":"{}"}}"#, TEST_DOMAIN),
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
        .env("HOME", &home_dir)
        .output()
        .expect("failed to run up");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no service") || !output.status.success(),
        "should fail without devproxy.port label: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&home_dir);
    let _ = std::fs::remove_dir_all(&test_dir);
}
```

**Step 3: Update justfile for e2e tests**

Add to `justfile`:
```
# Run e2e tests (requires Docker)
e2e:
    cargo test --test e2e -- --include-ignored
```

**Step 4: Run the non-ignored tests first**

Run: `cd /Users/chrisfenton/Code/personal/devproxy/.worktrees/build-devproxy && cargo test --test e2e`
Expected: `test_cli_help`, `test_status_without_daemon`, `test_up_without_label` pass; ignored tests skipped

**Step 5: Commit**

```bash
git add tests/ justfile
git commit -m "feat: add e2e test harness with fixtures and test cases"
```

---

## Task 12: Build, Fix, and Verify

This is the integration phase. At this point, all code is written but likely has compilation errors, type mismatches, and other issues.

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
Expected: `test_cli_help`, `test_status_without_daemon`, `test_up_without_label` pass

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
