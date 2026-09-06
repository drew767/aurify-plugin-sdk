//! The local process protocol: what a plugin process serves to the client on
//! `127.0.0.1`. The client talks to the process directly, never through a server.
//!
//! Three routes, one dispatch path:
//!
//! * `GET  /plugin/v1/health`             — is the process ready
//! * `GET  /plugin/v1/manifest`           — what this process claims to be
//! * `POST /plugin/v1/operations/{name}`  — run a declared operation; screen models
//!   are loaded through the same route, by the screen's `modelOperation`
//!
//! Every request carries `X-Aurify-Plugin-Secret`. The server binds the loopback
//! address only, but any process on the machine can reach loopback, and the secret is
//! what tells the client apart from a stray web page doing `fetch("http://127.0.0.1")`.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::launch::LaunchContext;
use crate::manifest::Manifest;

pub const SECRET_HEADER: &str = "X-Aurify-Plugin-Secret";
pub const ROUTE_PREFIX: &str = "/plugin/v1/";
pub const HEALTH_ROUTE: &str = "/plugin/v1/health";
pub const MANIFEST_ROUTE: &str = "/plugin/v1/manifest";
pub const OPERATIONS_ROUTE_PREFIX: &str = "/plugin/v1/operations/";

/// Bodies larger than this are refused before they are read. Operations carry
/// arguments, not files; a file goes through the platform's storage and arrives as an id.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// How long the accept loop waits before checking whether it was asked to stop.
const ACCEPT_POLL: Duration = Duration::from_millis(200);

/// The server thread runs the plugin's operation handler, and through a binding that
/// handler runs in a managed runtime whose JIT and GC borrow the calling thread's stack.
/// The 2 MiB a Rust thread gets by default is enough for Rust and not always for that.
const SERVER_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Starting,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// What an operation handler returns when it cannot do the work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationError {
    /// Machine code, `UPPER_SNAKE_CASE`, for the client to branch on.
    pub code: String,
    /// Sentence for the person.
    pub message: String,
}

impl OperationError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

/// Runs one declared operation. Called on the server thread; a slow handler blocks the
/// next request, so anything long should be started and reported through a model.
pub type OperationHandler = dyn Fn(&str, &Value) -> Result<Value, OperationError> + Send + Sync;

pub struct LocalServer {
    port: u16,
    health: Arc<Mutex<Health>>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Debug)]
pub enum StartError {
    Bind(String),
    Manifest(Vec<String>),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Bind(why) => write!(f, "could not bind the local server: {why}"),
            StartError::Manifest(errors) => write!(f, "manifest is invalid: {}", errors.join("; ")),
        }
    }
}

impl std::error::Error for StartError {}

impl LocalServer {
    /// Starts serving on `127.0.0.1:{ctx.port}` in a background thread. The manifest is
    /// validated first: a process that answers for a manifest the client would refuse
    /// is a process nobody can install.
    pub fn start(
        manifest: Manifest,
        ctx: &LaunchContext,
        handler: Arc<OperationHandler>,
    ) -> Result<Self, StartError> {
        let errors = manifest.validate();
        if !errors.is_empty() {
            return Err(StartError::Manifest(errors));
        }

        let server = Server::http(("127.0.0.1", ctx.port)).map_err(|e| StartError::Bind(e.to_string()))?;
        let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(ctx.port);

        let health = Arc::new(Mutex::new(Health { status: HealthStatus::Starting, message: None }));
        let stop = Arc::new(AtomicBool::new(false));
        let operations: Vec<String> = manifest.operations.iter().map(|o| o.name.clone()).collect();
        let manifest_json = manifest.to_json();
        let secret = ctx.secret.clone();

        let thread = {
            let health = Arc::clone(&health);
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("aurify-plugin-local-server".into())
                .stack_size(SERVER_THREAD_STACK_BYTES)
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match server.recv_timeout(ACCEPT_POLL) {
                            Ok(Some(request)) => {
                                handle(request, &secret, &manifest_json, &operations, &health, &handler)
                            }
                            Ok(None) => continue,
                            Err(_) => break,
                        }
                    }
                })
                .map_err(|e| StartError::Bind(e.to_string()))?
        };

        Ok(Self { port, health, stop, thread: Mutex::new(Some(thread)) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The plugin decides when it is ready; the server only reports it. Starting is the
    /// initial state so a probe during warm-up says "wait", not "broken".
    pub fn set_health(&self, status: HealthStatus, message: Option<String>) {
        if let Ok(mut health) = self.health.lock() {
            *health = Health { status, message };
        }
    }

    /// Stops serving and waits for the server thread. Every caller returns only once the
    /// thread has exited, including a second caller that arrives while the first is
    /// still waiting: the join happens under the lock, so the second one waits for it.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let Ok(mut thread) = self.thread.lock() else { return };
        if let Some(thread) = thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle(
    mut request: Request,
    secret: &str,
    manifest_json: &str,
    operations: &[String],
    health: &Mutex<Health>,
    handler: &Arc<OperationHandler>,
) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();

    if !path.starts_with(ROUTE_PREFIX) {
        respond_error(request, 404, "NOT_FOUND", "no such route");
        return;
    }
    if !secret_matches(&request, secret) {
        // The same answer for a missing and a wrong secret: telling them apart would
        // let a guessing process learn that it is getting warmer.
        respond_error(request, 401, "SECRET_REQUIRED", "the plugin secret is missing or wrong");
        return;
    }

    match (request.method(), path.as_str()) {
        (Method::Get, HEALTH_ROUTE) => {
            let body = health.lock().map(|h| serde_json::to_string(&*h).unwrap_or_default()).unwrap_or_default();
            respond_json(request, 200, body);
        }
        (Method::Get, MANIFEST_ROUTE) => respond_json(request, 200, manifest_json.to_string()),
        (Method::Post, path) if path.starts_with(OPERATIONS_ROUTE_PREFIX) => {
            let name = &path[OPERATIONS_ROUTE_PREFIX.len()..];
            if !operations.iter().any(|known| known == name) {
                respond_error(request, 404, "OPERATION_UNKNOWN", "the manifest does not declare this operation");
                return;
            }
            let body_len = request.body_length().unwrap_or(0);
            if body_len > MAX_BODY_BYTES {
                respond_error(request, 413, "BODY_TOO_LARGE", "operation arguments are limited to 1 MiB");
                return;
            }
            let mut raw = String::new();
            if request.as_reader().take(MAX_BODY_BYTES as u64).read_to_string(&mut raw).is_err() {
                respond_error(request, 400, "BODY_UNREADABLE", "could not read the request body");
                return;
            }
            let args = if raw.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                match serde_json::from_str::<Value>(&raw) {
                    Ok(Value::Object(mut envelope)) => envelope.remove("args").unwrap_or(Value::Object(Default::default())),
                    Ok(_) => {
                        respond_error(request, 400, "BODY_INVALID", "the body must be a JSON object with an args field");
                        return;
                    }
                    Err(_) => {
                        respond_error(request, 400, "BODY_INVALID", "the body is not valid JSON");
                        return;
                    }
                }
            };
            match handler(name, &args) {
                Ok(result) => respond_json(request, 200, json!({ "result": result }).to_string()),
                Err(error) => respond_json(request, 422, json!({ "error": error }).to_string()),
            }
        }
        _ => respond_error(request, 405, "METHOD_NOT_ALLOWED", "this route does not take that method"),
    }
}

fn secret_matches(request: &Request, secret: &str) -> bool {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(SECRET_HEADER))
        .map(|h| constant_time_eq(h.value.as_str().as_bytes(), secret.as_bytes()))
        .unwrap_or(false)
}

/// Compares without leaving early: a comparison that stops at the first wrong byte tells
/// a patient caller how many bytes it already has right.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn respond_json(request: Request, status: u16, body: String) {
    let response = Response::from_string(body)
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes("Content-Type", "application/json; charset=utf-8").expect("static header"));
    let _ = request.respond(response);
}

fn respond_error(request: Request, status: u16, code: &str, message: &str) {
    respond_json(request, status, json!({ "error": { "code": code, "message": message } }).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;

    fn manifest() -> Manifest {
        Manifest::from_json(
            &json!({
                "schemaVersion": 1, "id": "hello", "version": "1", "kind": "plugin", "title": "Hello",
                "slots": [{"type": "apps-card", "screen": "main"}],
                "screens": [{"id": "main", "title": "Main", "modelOperation": "main.model", "components": []}],
                "operations": [{"name": "main.model"}, {"name": "greet"}]
            })
            .to_string(),
        )
        .unwrap()
    }

    fn ctx(port: u16) -> LaunchContext {
        LaunchContext { port, secret: "0123456789abcdef".into(), platform_url: None, platform_token: None, data_dir: None }
    }

    /// A raw HTTP/1.0 exchange: the crate has no client dependency, and one is not worth
    /// adding for tests that send a handful of bytes.
    fn call(port: u16, method: &str, path: &str, secret: Option<&str>, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut head = format!("{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n", body.len());
        if let Some(secret) = secret {
            head.push_str(&format!("{SECRET_HEADER}: {secret}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let status: u16 = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
    }

    fn start(port: u16) -> LocalServer {
        let handler: Arc<OperationHandler> = Arc::new(|name, args| match name {
            "main.model" => Ok(json!({ "items": ["a", "b"] })),
            "greet" => match args.get("name").and_then(Value::as_str) {
                Some(who) => Ok(json!({ "text": format!("hello, {who}") })),
                None => Err(OperationError::new("NAME_REQUIRED", "say who to greet")),
            },
            _ => unreachable!("undeclared operations never reach the handler"),
        });
        LocalServer::start(manifest(), &ctx(port), handler).unwrap()
    }

    #[test]
    fn health_starts_as_starting_and_follows_the_plugin() {
        let port = free_port();
        let server = start(port);
        let (status, body) = call(port, "GET", HEALTH_ROUTE, Some("0123456789abcdef"), "");
        assert_eq!(status, 200);
        assert!(body.contains("\"starting\""), "{body}");
        server.set_health(HealthStatus::Ready, None);
        let (_, body) = call(port, "GET", HEALTH_ROUTE, Some("0123456789abcdef"), "");
        assert!(body.contains("\"ready\""), "{body}");
    }

    #[test]
    fn every_route_needs_the_secret() {
        let port = free_port();
        let _server = start(port);
        assert_eq!(call(port, "GET", HEALTH_ROUTE, None, "").0, 401);
        assert_eq!(call(port, "GET", HEALTH_ROUTE, Some("wrong-wrong-wrong-wrong"), "").0, 401);
        assert_eq!(call(port, "GET", MANIFEST_ROUTE, Some("0123456789abcdef"), "").0, 200);
    }

    #[test]
    fn operations_dispatch_by_name_and_only_declared_ones() {
        let port = free_port();
        let _server = start(port);
        let (status, body) = call(port, "POST", "/plugin/v1/operations/greet", Some("0123456789abcdef"), r#"{"args":{"name":"Ann"}}"#);
        assert_eq!(status, 200);
        assert!(body.contains("hello, Ann"), "{body}");

        let (status, body) = call(port, "POST", "/plugin/v1/operations/greet", Some("0123456789abcdef"), "{}");
        assert_eq!(status, 422);
        assert!(body.contains("NAME_REQUIRED"), "{body}");

        let (status, _) = call(port, "POST", "/plugin/v1/operations/shutdown", Some("0123456789abcdef"), "{}");
        assert_eq!(status, 404);
    }

    #[test]
    fn an_invalid_manifest_never_starts_serving() {
        let mut broken = manifest();
        broken.slots.clear();
        let handler: Arc<OperationHandler> = Arc::new(|_, _| Ok(Value::Null));
        assert!(matches!(LocalServer::start(broken, &ctx(free_port()), handler), Err(StartError::Manifest(_))));
    }
}
