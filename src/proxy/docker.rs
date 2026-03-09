use crate::proxy::router::Router;
use anyhow::{Context, Result, bail};
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
    #[allow(dead_code)]
    #[serde(rename = "HostIp")]
    host_ip: Option<String>,
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

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("docker ps failed (exit {}): {stderr}", output.status);
    }

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

/// Maximum consecutive failures before giving up on Docker event watching.
const MAX_CONSECUTIVE_FAILURES: u32 = 10;

/// Watch Docker events and update routes in real-time.
///
/// Retries on transient errors (e.g., `docker events` process exits) with
/// exponential backoff up to 30 seconds. Gives up after MAX_CONSECUTIVE_FAILURES
/// consecutive failures. A successful event resets the failure counter.
pub async fn watch_events(router: &Router) -> Result<()> {
    let mut consecutive_failures: u32 = 0;

    loop {
        eprintln!("  starting docker event watcher...");
        match watch_events_inner(router).await {
            Ok(()) => {
                // docker events exited cleanly (unlikely but possible)
                consecutive_failures = 0;
                eprintln!("  docker event watcher exited, restarting...");
            }
            Err(e) => {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    bail!(
                        "docker event watcher failed {MAX_CONSECUTIVE_FAILURES} \
                         consecutive times, last error: {e:#}"
                    );
                }
                let delay = std::cmp::min(2u64.pow(consecutive_failures), 30);
                eprintln!(
                    "  docker event watcher error ({consecutive_failures}/\
                     {MAX_CONSECUTIVE_FAILURES}): {e:#}, retrying in {delay}s..."
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
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
        .kill_on_drop(true)
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

    // Ensure child process is cleaned up
    let _ = child.wait().await;

    Ok(())
}
