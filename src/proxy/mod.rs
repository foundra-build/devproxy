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
    let tcp_listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .with_context(|| format!("could not bind to port {port}"))?;
    eprintln!("HTTPS proxy listening on 127.0.0.1:{port}");

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

    // Build headers on the builder before constructing the request
    let mut builder = HyperRequest::builder()
        .method(method)
        .uri(upstream_uri);

    for (name, value) in headers.iter() {
        if name != "host" {
            builder = builder.header(name.clone(), value.clone());
        }
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
    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("failed to collect upstream response: {e}"))?
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
