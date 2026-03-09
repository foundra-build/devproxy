use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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

/// Default timeout for IPC operations (connect + request + response).
const IPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Send a request to the daemon and get a response, with the default timeout.
pub async fn send_request(socket_path: &Path, request: &Request) -> Result<Response> {
    send_request_with_timeout(socket_path, request, IPC_TIMEOUT).await
}

/// Send a request to the daemon and get a response, with a custom timeout.
/// Returns an error if the daemon does not respond within the given duration.
pub async fn send_request_with_timeout(
    socket_path: &Path,
    request: &Request,
    timeout: std::time::Duration,
) -> Result<Response> {
    tokio::time::timeout(timeout, send_request_inner(socket_path, request))
        .await
        .with_context(|| {
            format!(
                "timed out waiting for daemon response ({}s). The daemon may be dead. Try `devproxy init`.",
                timeout.as_secs()
            )
        })?
}

async fn send_request_inner(socket_path: &Path, request: &Request) -> Result<Response> {
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

    // Limit reads to 64KB to prevent unbounded memory allocation from
    // malicious or malformed daemon responses.
    let mut buf_reader = BufReader::new(reader.take(64 * 1024));
    let mut response_line = String::new();
    buf_reader.read_line(&mut response_line).await?;

    let response: Response = serde_json::from_str(response_line.trim())
        .context("could not parse daemon response")?;
    Ok(response)
}

/// Send a synchronous IPC Ping to the daemon and return true if it responds
/// with Pong within the given timeout. This is used by non-async code (init,
/// up) that needs to verify the daemon is alive without a tokio runtime.
///
/// The wire format matches the async `send_request` path: a JSON-line
/// `{"cmd":"ping"}` followed by shutdown(Write), then read a JSON-line
/// response containing `"pong"`.
pub fn ping_sync(socket_path: &Path, timeout: std::time::Duration) -> bool {
    use std::io::{BufRead, Write};

    let stream = match std::os::unix::net::UnixStream::connect(socket_path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return false,
    };
    if writer.write_all(b"{\"cmd\":\"ping\"}\n").is_err() {
        return false;
    }
    writer.shutdown(std::net::Shutdown::Write).ok();

    let mut reader = std::io::BufReader::new(&stream);
    let mut response = String::new();
    reader.read_line(&mut response).is_ok() && response.contains("pong")
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

    #[test]
    fn ping_sync_returns_false_on_nonexistent_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("nonexistent.sock");
        assert!(!ping_sync(&sock_path, std::time::Duration::from_millis(100)));
    }

    /// Verify that send_request_with_timeout returns an error when the
    /// daemon doesn't respond within the timeout (e.g., socket exists
    /// but nothing reads from it).
    #[tokio::test]
    async fn send_request_timeout_on_unresponsive_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");

        // Create a listener but never accept connections -- simulates
        // a hung daemon that is listening but not processing requests.
        let _listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let start = std::time::Instant::now();
        let result = send_request_with_timeout(
            &sock_path,
            &Request::Ping,
            std::time::Duration::from_millis(500),
        )
        .await;
        let elapsed = start.elapsed();

        assert!(result.is_err(), "should error on unresponsive socket");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("timed out"),
            "error should mention timeout: {err_msg}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "should not wait too long (took {elapsed:?})"
        );
    }
}
