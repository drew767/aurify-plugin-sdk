//! Calls from the plugin into the Aurify platform, on behalf of the person who
//! installed it and within the permissions they granted.
//!
//! The token arrives through the launch environment and is scoped to this plugin and
//! this person. A renewed one can only reach a running process in a response, so the
//! client keeps the current token in a slot the platform may replace.

use std::sync::RwLock;
use std::time::Duration;

use serde_json::Value;

use crate::launch::LaunchContext;

/// Header the platform puts a freshly minted token in.
pub const RENEWED_TOKEN_HEADER: &str = "X-Aurify-Plugin-Token";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAXIMUM_FILE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

pub struct FileChunk {
    pub bytes: Vec<u8>,
    pub content_range: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug)]
pub enum PlatformError {
    /// The client did not hand this process a platform address or token: the plugin was
    /// started without platform access, or by hand.
    NotConfigured,
    Transport(String),
    /// The platform answered with a non-success status; the body is passed through so the
    /// plugin can read the machine code.
    Rejected { status: u16, body: String },
    InvalidJson(String),
    InvalidRequest(String),
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformError::NotConfigured => write!(f, "platform access was not configured for this process"),
            PlatformError::Transport(why) => write!(f, "could not reach the platform: {why}"),
            PlatformError::Rejected { status, body } => write!(f, "platform answered {status}: {body}"),
            PlatformError::InvalidJson(why) => write!(f, "platform answered with invalid JSON: {why}"),
            PlatformError::InvalidRequest(why) => write!(f, "invalid platform request: {why}"),
        }
    }
}

impl std::error::Error for PlatformError {}

pub struct PlatformClient {
    base_url: String,
    token: RwLock<String>,
    agent: ureq::Agent,
}

impl PlatformClient {
    pub fn read_file_chunk(&self, media_id: &str, offset: u64, length: u64) -> Result<FileChunk, PlatformError> {
        use std::io::Read;
        if media_id.len() != 36 || !media_id.bytes().enumerate().all(|(index, byte)|
            if [8, 13, 18, 23].contains(&index) { byte == b'-' } else { byte.is_ascii_hexdigit() }) ||
            length == 0 || length > MAXIMUM_FILE_CHUNK_BYTES || offset.checked_add(length).is_none() {
            return Err(PlatformError::InvalidRequest("file id or byte range".into()));
        }
        let token = self.token.read().map(|token| token.clone()).unwrap_or_default();
        let response = self.agent.get(&format!("{}/files/{media_id}", self.base_url))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Range", &format!("bytes={offset}-{}", offset + length - 1)).call();
        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => return Err(PlatformError::Rejected { status, body: response.into_string().unwrap_or_default() }),
            Err(ureq::Error::Transport(error)) => return Err(PlatformError::Transport(error.to_string())),
        };
        if let Some(renewed) = response.header(RENEWED_TOKEN_HEADER) {
            if let Ok(mut token) = self.token.write() { *token = renewed.to_string(); }
        }
        let content_range = response.header("Content-Range").map(str::to_string);
        let content_type = response.header("Content-Type").map(str::to_string);
        let mut bytes = Vec::new();
        response.into_reader().take(length + 1).read_to_end(&mut bytes)
            .map_err(|error| PlatformError::Transport(error.to_string()))?;
        if bytes.len() as u64 > length { return Err(PlatformError::Transport("file range exceeded requested length".into())); }
        Ok(FileChunk { bytes, content_range, content_type })
    }

    /// `None` when the launch context carries no platform address or token.
    pub fn from_launch(ctx: &LaunchContext) -> Option<Self> {
        let base_url = ctx.platform_url.as_ref()?.trim_end_matches('/').to_string();
        let token = ctx.platform_token.clone()?;
        Some(Self {
            base_url,
            token: RwLock::new(token),
            agent: ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).redirects(0).build(),
        })
    }

    /// One call to a platform route. `path` is relative to the platform address the
    /// client handed this process, e.g. `/me`: who the plugin serves and what it may read.
    /// A JSON body is sent when given; the answer is parsed as JSON, or `Null` when
    /// the platform answered with no body.
    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, PlatformError> {
        let url = format!("{}{}", self.base_url, path);
        let token = self.token.read().map(|t| t.clone()).unwrap_or_default();
        let request = self
            .agent
            .request(method, &url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json");

        let response = match body {
            Some(json) => request.set("Content-Type", "application/json").send_string(&json.to_string()),
            None => request.call(),
        };

        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                return Err(PlatformError::Rejected { status, body });
            }
            Err(ureq::Error::Transport(transport)) => return Err(PlatformError::Transport(transport.to_string())),
        };

        if let Some(renewed) = response.header(RENEWED_TOKEN_HEADER) {
            if let Ok(mut slot) = self.token.write() {
                *slot = renewed.to_string();
            }
        }

        let text = response.into_string().map_err(|e| PlatformError::Transport(e.to_string()))?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| PlatformError::InvalidJson(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn file_reads_send_a_bounded_range_and_preserve_bytes() {
        let (port, server) = serve_once(206, "Content-Range: bytes 2-4/5\r\n", "abc");
        let client = PlatformClient {
            base_url: format!("http://127.0.0.1:{port}"),
            token: RwLock::new("plugin-token".into()), agent: ureq::AgentBuilder::new().build(),
        };
        let chunk = client.read_file_chunk("00000000-0000-4000-8000-000000000001", 2, 3).unwrap();
        assert_eq!(chunk.bytes, b"abc");
        assert_eq!(chunk.content_range.as_deref(), Some("bytes 2-4/5"));
        let request = server.join().unwrap();
        assert!(request.contains("Range: bytes=2-4"));
        assert!(client.read_file_chunk("../file", 0, 1).is_err());
        assert!(client.read_file_chunk("00000000-0000-4000-8000-000000000001", u64::MAX, 1).is_err());
    }

    /// A one-shot HTTP/1.1 responder: enough to see what the client sent and to hand a
    /// renewed token back.
    ///
    /// It reads the whole request, body included, before answering. Closing a socket
    /// with unread bytes makes Windows send a reset instead of a graceful close, and the
    /// client then sees a dropped connection where the test expects a 403.
    fn serve_once(status: u16, extra_header: &str, body: &str) -> (u16, thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (status, extra_header, body) = (status, extra_header.to_string(), body.to_string());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let received = read_full_request(&mut stream);
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_header}\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            received
        });
        (port, handle)
    }

    fn read_full_request(stream: &mut std::net::TcpStream) -> String {
        let mut raw: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                return String::from_utf8_lossy(&raw).to_string();
            }
            raw.extend_from_slice(&chunk[..read]);
            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
        let content_length: usize = head
            .lines()
            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
            .unwrap_or(0);
        while raw.len() < header_end + content_length {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&raw).to_string()
    }

    fn client(port: u16) -> PlatformClient {
        PlatformClient::from_launch(&LaunchContext {
            port: 1,
            secret: "0123456789abcdef".into(),
            platform_url: Some(format!("http://127.0.0.1:{port}/")),
            platform_token: Some("token-one".into()),
            data_dir: None,
        })
        .unwrap()
    }

    #[test]
    fn a_process_without_platform_access_gets_none_not_a_broken_client() {
        let ctx = LaunchContext { port: 1, secret: "0123456789abcdef".into(), platform_url: None, platform_token: None, data_dir: None };
        assert!(PlatformClient::from_launch(&ctx).is_none());
    }

    #[test]
    fn the_token_rides_as_bearer_and_a_renewed_one_replaces_it() {
        let (port, first) = serve_once(200, "X-Aurify-Plugin-Token: token-two\r\n", r#"{"ok":true}"#);
        let client = client(port);
        let answer = client.call("GET", "/api/v1/users/me", None).unwrap();
        assert_eq!(answer["ok"], Value::Bool(true));
        assert!(first.join().unwrap().contains("Authorization: Bearer token-one"));

        let (port_two, second) = serve_once(204, "", "");
        // Same client, new port: the base URL is fixed, so point a second responder at it
        // by rebuilding the client with the renewed token still inside.
        let renewed = client.token.read().unwrap().clone();
        assert_eq!(renewed, "token-two");
        let client_two = PlatformClient { base_url: format!("http://127.0.0.1:{port_two}"), token: RwLock::new(renewed), agent: client.agent.clone() };
        assert_eq!(client_two.call("DELETE", "/x", None).unwrap(), Value::Null);
        assert!(second.join().unwrap().contains("Bearer token-two"));
    }

    #[test]
    fn a_refusal_carries_the_platform_body_through() {
        let (port, _) = serve_once(403, "", r#"{"error":{"code":"SCOPE_REQUIRED"}}"#);
        match client(port).call("POST", "/api/v1/generation/jobs", Some(&serde_json::json!({"operation": "x"}))) {
            Err(PlatformError::Rejected { status, body }) => {
                assert_eq!(status, 403);
                assert!(body.contains("SCOPE_REQUIRED"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
