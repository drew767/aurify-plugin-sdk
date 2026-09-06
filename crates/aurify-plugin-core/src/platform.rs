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
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformError::NotConfigured => write!(f, "platform access was not configured for this process"),
            PlatformError::Transport(why) => write!(f, "could not reach the platform: {why}"),
            PlatformError::Rejected { status, body } => write!(f, "platform answered {status}: {body}"),
            PlatformError::InvalidJson(why) => write!(f, "platform answered with invalid JSON: {why}"),
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
    /// `None` when the launch context carries no platform address or token.
    pub fn from_launch(ctx: &LaunchContext) -> Option<Self> {
        let base_url = ctx.platform_url.as_ref()?.trim_end_matches('/').to_string();
        let token = ctx.platform_token.clone()?;
        Some(Self {
            base_url,
            token: RwLock::new(token),
            agent: ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build(),
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
