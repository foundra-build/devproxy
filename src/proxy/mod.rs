pub mod cert;
pub mod docker;
pub mod router;

use crate::config::Config;
use crate::ipc::{Request, Response};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse};
use router::Router;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixListener};

/// RAII guard that removes the IPC socket and PID file on drop, ensuring
/// cleanup on both normal shutdown and error paths.
struct DaemonCleanupGuard {
    socket_path: std::path::PathBuf,
    pid_path: std::path::PathBuf,
}

impl Drop for DaemonCleanupGuard {
    fn drop(&mut self) {
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
            eprintln!("cleaned up socket: {}", self.socket_path.display());
        }
        if self.pid_path.exists() {
            let _ = std::fs::remove_file(&self.pid_path);
            eprintln!("cleaned up pid file: {}", self.pid_path.display());
        }
    }
}

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

    // Set up IPC socket -- check for an already-running daemon first
    let socket_path = Config::socket_path()?;

    // Unix domain socket paths are limited to ~104 bytes on macOS (108 on Linux).
    // Validate upfront to give a clear error instead of a confusing bind failure.
    let socket_path_len = socket_path.as_os_str().len();
    if socket_path_len > 100 {
        bail!(
            "socket path is too long ({socket_path_len} bytes, max ~100): {}. \
             Use a shorter DEVPROXY_CONFIG_DIR.",
            socket_path.display()
        );
    }

    if socket_path.exists() {
        // Send an actual IPC ping to verify the daemon is functional, not
        // just accepting connections (which could happen during shutdown).
        let is_alive = matches!(
            crate::ipc::send_request(&socket_path, &Request::Ping).await,
            Ok(Response::Pong)
        );
        if is_alive {
            bail!(
                "another daemon appears to be running (socket {} responded to ping). \
                 Stop it first or remove the stale socket.",
                socket_path.display()
            );
        }
        // Stale socket from a previous crash or mid-shutdown -- safe to remove
        std::fs::remove_file(&socket_path)?;
    }

    let ipc_listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("could not bind IPC socket at {}", socket_path.display()))?;
    // Make socket world-writable so non-root users can connect when daemon runs as root
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o777))?;
    }

    // Write PID file so init can find and kill stale daemons
    let pid_path = Config::pid_path()?;
    std::fs::write(&pid_path, std::process::id().to_string())
        .with_context(|| format!("could not write PID file at {}", pid_path.display()))?;

    // Ensure the socket and PID files are removed when the daemon exits (normal or error).
    let _cleanup_guard = DaemonCleanupGuard {
        socket_path: socket_path.clone(),
        pid_path,
    };
    eprintln!("IPC listening on {}", socket_path.display());

    // Set up HTTPS listener
    let tcp_listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .with_context(|| format!("could not bind to port {port}"))?;
    eprintln!("HTTPS proxy listening on 127.0.0.1:{port}");

    // Run all three tasks concurrently with try_join!.
    // All three loops run forever under normal operation. If any returns
    // an error, try_join! cancels the remaining tasks and propagates the
    // first error immediately -- preventing a half-dead daemon.
    let r1 = router.clone();
    let r2 = router.clone();
    let r3 = router.clone();

    tokio::try_join!(
        async { https_proxy_loop(tcp_listener, tls_acceptor, r1).await.context("HTTPS proxy task failed") },
        async { docker::watch_events(&r2).await.context("Docker watcher task failed") },
        async { ipc_server_loop(ipc_listener, r3).await.context("IPC server task failed") },
    )?;

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
    // Limit reads to 64KB to prevent unbounded memory allocation from
    // malicious or malformed IPC messages.
    let mut buf_reader = BufReader::new(reader.take(64 * 1024));
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

/// Maximum number of concurrent connections the proxy will handle.
/// Prevents resource exhaustion from connection floods.
const MAX_CONCURRENT_CONNECTIONS: usize = 512;

/// Accept TLS connections and proxy them
async fn https_proxy_loop(
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    router: Router,
) -> Result<()> {
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    loop {
        let (tcp_stream, _addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        let permit = semaphore.clone().acquire_owned().await
            .context("connection semaphore closed")?;

        tokio::spawn(async move {
            let _permit = permit; // held until task completes
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
    let path_and_query = req.uri().path_and_query().map(|pq| pq.as_str().to_string()).unwrap_or_else(|| "/".to_string());
    let upstream_addr = format!("127.0.0.1:{host_port}");

    // Forward the request to the container
    match proxy_to_upstream(&upstream_addr, &path_and_query, req).await {
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

    // Timeout upstream connect + handshake at 30 seconds to avoid
    // hanging indefinitely on unresponsive containers.
    const UPSTREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    let stream = tokio::time::timeout(UPSTREAM_TIMEOUT, TcpStream::connect(upstream_addr))
        .await
        .context("upstream connect timed out")?
        .with_context(|| format!("could not connect to upstream at {upstream_addr}"))?;

    let io = hyper_util::rt::TokioIo::new(stream);

    let (mut sender, conn) = tokio::time::timeout(
        UPSTREAM_TIMEOUT,
        hyper::client::conn::http1::handshake(io),
    )
        .await
        .context("upstream handshake timed out")?
        .context("upstream handshake failed")?;

    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("  upstream connection error: {e}");
        }
    });

    // Build the upstream request preserving method, path, and headers.
    // Use path-only URI (not full http://host/path) since this is a
    // reverse proxy sending to an origin server, not a forward proxy.
    let upstream_uri: hyper::Uri = path_and_query
        .parse()
        .context("invalid upstream URI path")?;
    let method = incoming_req.method().clone();
    let headers = incoming_req.headers().clone();

    // Cap request body at 100MB to prevent memory exhaustion from
    // oversized uploads.
    const MAX_BODY_SIZE: usize = 100 * 1024 * 1024;
    let limited = http_body_util::Limited::new(incoming_req, MAX_BODY_SIZE);
    let body = limited
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("failed to collect body (max {MAX_BODY_SIZE} bytes): {e}"))?
        .to_bytes();

    // Preserve the original Host header so upstream frameworks see the
    // correct domain for URL generation, CSRF checks, and virtual host
    // routing. The upstream container is already targeted by the TCP
    // connection address.
    let mut builder = HyperRequest::builder()
        .method(method)
        .uri(upstream_uri);

    for (name, value) in headers.iter() {
        builder = builder.header(name.clone(), value.clone());
    }

    let upstream_req = builder
        .body(Full::new(body))
        .context("failed to build upstream request")?;

    let resp = sender
        .send_request(upstream_req)
        .await
        .context("upstream request failed")?;

    let status = resp.status();
    let resp_headers = resp.headers().clone();
    // Cap response body at 100MB, matching the request body limit.
    let limited_resp = http_body_util::Limited::new(resp.into_body(), MAX_BODY_SIZE);
    let body = limited_resp
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("upstream response too large (max {MAX_BODY_SIZE} bytes): {e}"))?
        .to_bytes();

    let mut resp_builder = HyperResponse::builder()
        .status(status);

    for (name, value) in resp_headers.iter() {
        resp_builder = resp_builder.header(name.clone(), value.clone());
    }

    let response = resp_builder
        .body(Full::new(body))
        .context("failed to build response")?;

    Ok(response)
}
