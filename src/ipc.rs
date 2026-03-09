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

    // Limit reads to 64KB to prevent unbounded memory allocation from
    // malicious or malformed daemon responses.
    let mut buf_reader = BufReader::new(reader.take(64 * 1024));
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
