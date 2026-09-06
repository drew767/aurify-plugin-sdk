//! C ABI over `aurify-plugin-core`.
//!
//! Strings cross the boundary as NUL-terminated UTF-8. Every string this library hands
//! out is owned by this library and must be returned to `aurify_plugin_string_free`;
//! every string a callback hands back must have been made by `aurify_plugin_string_alloc`,
//! so that both sides free with the allocator that allocated. Mixing allocators across a
//! DLL boundary is the classic silent crash, and this rule is what prevents it.
//!
//! Errors come back through an `error_out` slot rather than a thread-local "last error":
//! bindings call from whichever thread they like, and a slot cannot be overwritten by a
//! call they did not make.

#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use aurify_plugin_core::{
    HealthStatus, LaunchContext, LocalServer, Manifest, OperationError, OperationHandler,
    PlatformClient,
};
use serde_json::{json, Value};

/// Runs one operation. Receives the operation name and its arguments as JSON, returns a
/// string made by `aurify_plugin_string_alloc`: either `{"result": ...}` or
/// `{"error": {"code": "...", "message": "..."}}`. NULL is read as an internal error.
pub type AurifyPluginOperationFn =
    unsafe extern "C" fn(name: *const c_char, args_json: *const c_char, user_data: *mut c_void) -> *mut c_char;

/// A running plugin: the local server, and the platform client when the launch
/// environment carried one.
pub struct AurifyPluginHost {
    server: LocalServer,
    platform: Option<PlatformClient>,
}

/// The pointer handed across the C boundary is a key into this table, never the host's
/// address. Stopping joins the server thread, and a binding that disposes from two
/// threads at once used to hand the same pointer to `host_stop` twice: the second call
/// freed the host under the first, which then read freed memory once the join returned.
/// A key leaves the table before teardown begins, so the second call finds nothing, and
/// keys are never reused, so a stale one cannot name a host started later.
static HOSTS: OnceLock<Mutex<HashMap<usize, Arc<AurifyPluginHost>>>> = OnceLock::new();
static NEXT_HOST_KEY: AtomicUsize = AtomicUsize::new(1);

fn hosts() -> &'static Mutex<HashMap<usize, Arc<AurifyPluginHost>>> {
    HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_host(host: AurifyPluginHost) -> *mut AurifyPluginHost {
    let key = NEXT_HOST_KEY.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut table) = hosts().lock() {
        table.insert(key, Arc::new(host));
    }
    key as *mut AurifyPluginHost
}

fn find_host(handle: *const AurifyPluginHost) -> Option<Arc<AurifyPluginHost>> {
    hosts().lock().ok()?.get(&(handle as usize)).cloned()
}

fn take_host(handle: *mut AurifyPluginHost) -> Option<Arc<AurifyPluginHost>> {
    hosts().lock().ok()?.remove(&(handle as usize))
}

/// Wraps the raw callback so it can be shared with the server thread. The pointer is
/// only ever passed back to the callback that gave it; this type never reads it.
struct CallbackBridge {
    on_operation: AurifyPluginOperationFn,
    user_data: *mut c_void,
}

unsafe impl Send for CallbackBridge {}
unsafe impl Sync for CallbackBridge {}

fn to_c_string(text: &str) -> *mut c_char {
    // A NUL inside the text cannot cross a C boundary; it is dropped rather than
    // truncating the message at a point the caller would not expect.
    let clean: String = text.chars().filter(|c| *c != '\0').collect();
    CString::new(clean).map(CString::into_raw).unwrap_or(ptr::null_mut())
}

unsafe fn from_c_str<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    CStr::from_ptr(pointer).to_str().ok()
}

unsafe fn put_error(error_out: *mut *mut c_char, message: &str) {
    if !error_out.is_null() {
        *error_out = to_c_string(message);
    }
}

/// Version of the native library. Static; do not free.
#[no_mangle]
pub extern "C" fn aurify_plugin_version() -> *const c_char {
    static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr() as *const c_char
}

/// Copies `text` into a string owned by this library. Callbacks use it for their return
/// value so the server can free it with the right allocator.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_string_alloc(text: *const c_char) -> *mut c_char {
    match from_c_str(text) {
        Some(text) => to_c_string(text),
        None => ptr::null_mut(),
    }
}

/// Frees a string made by this library. NULL is accepted and ignored.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_string_free(text: *mut c_char) {
    if !text.is_null() {
        drop(CString::from_raw(text));
    }
}

/// Validates a manifest. Returns NULL when it is valid, otherwise a JSON array of
/// messages a person can act on. The array is owned by this library.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_manifest_validate(manifest_json: *const c_char) -> *mut c_char {
    let Some(json) = from_c_str(manifest_json) else {
        return to_c_string(&json!(["manifest_json is NULL or not UTF-8"]).to_string());
    };
    let errors = match Manifest::from_json(json) {
        Ok(manifest) => manifest.validate(),
        Err(message) => vec![message],
    };
    if errors.is_empty() {
        ptr::null_mut()
    } else {
        to_c_string(&Value::from(errors).to_string())
    }
}

/// Starts the plugin: validates the manifest, reads the launch context, binds the local
/// server. Returns NULL and fills `error_out` on failure.
///
/// `launch_json` carries the launch values as JSON (`port`, `secret`, `platformUrl`,
/// `platformToken`, `dataDir`); NULL reads them from this process's environment. A
/// binding passes them explicitly, because its runtime does not always share the
/// environment with this library — .NET on Unix keeps its own copy.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_host_start(
    manifest_json: *const c_char,
    launch_json: *const c_char,
    on_operation: Option<AurifyPluginOperationFn>,
    user_data: *mut c_void,
    error_out: *mut *mut c_char,
) -> *mut AurifyPluginHost {
    let Some(on_operation) = on_operation else {
        put_error(error_out, "on_operation must not be NULL");
        return ptr::null_mut();
    };
    let Some(json) = from_c_str(manifest_json) else {
        put_error(error_out, "manifest_json is NULL or not UTF-8");
        return ptr::null_mut();
    };
    let manifest = match Manifest::from_json(json) {
        Ok(manifest) => manifest,
        Err(message) => {
            put_error(error_out, &message);
            return ptr::null_mut();
        }
    };
    let launch = match from_c_str(launch_json) {
        Some(json) => LaunchContext::from_json(json),
        None => LaunchContext::from_env(),
    };
    let ctx = match launch {
        Ok(ctx) => ctx,
        Err(error) => {
            put_error(error_out, &error.to_string());
            return ptr::null_mut();
        }
    };

    let bridge = Arc::new(CallbackBridge { on_operation, user_data });
    let handler: Arc<OperationHandler> = Arc::new(move |name, args| dispatch(&bridge, name, args));

    let server = match LocalServer::start(manifest, &ctx, handler) {
        Ok(server) => server,
        Err(error) => {
            put_error(error_out, &error.to_string());
            return ptr::null_mut();
        }
    };
    let platform = PlatformClient::from_launch(&ctx);
    register_host(AurifyPluginHost { server, platform })
}

fn dispatch(bridge: &CallbackBridge, name: &str, args: &Value) -> Result<Value, OperationError> {
    let internal = |why: &str| OperationError::new("PLUGIN_INTERNAL", why);
    let c_name = CString::new(name).map_err(|_| internal("operation name contains NUL"))?;
    let c_args = CString::new(args.to_string()).map_err(|_| internal("arguments contain NUL"))?;

    // SAFETY: the callback and its user_data were given to host_start by the binding,
    // which keeps them alive for as long as the host exists.
    let raw = unsafe { (bridge.on_operation)(c_name.as_ptr(), c_args.as_ptr(), bridge.user_data) };
    if raw.is_null() {
        return Err(internal("the operation handler returned NULL"));
    }
    // SAFETY: the contract requires callbacks to return strings from string_alloc.
    let owned = unsafe { CString::from_raw(raw) };
    let text = owned.to_str().map_err(|_| internal("the operation handler returned non-UTF-8"))?;
    let envelope: Value =
        serde_json::from_str(text).map_err(|e| internal(&format!("the operation handler returned invalid JSON: {e}")))?;

    if let Some(error) = envelope.get("error") {
        let code = error.get("code").and_then(Value::as_str).unwrap_or("PLUGIN_ERROR");
        let message = error.get("message").and_then(Value::as_str).unwrap_or("the operation failed");
        return Err(OperationError::new(code, message));
    }
    Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
}

/// Port the local server is listening on. 0 once the host is stopped.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_host_port(host: *const AurifyPluginHost) -> u16 {
    find_host(host).map(|h| h.server.port()).unwrap_or(0)
}

/// Tells the client whether the plugin is ready. `status` is `starting`, `ready` or
/// `failed`; anything else is ignored. `message` may be NULL.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_host_set_health(
    host: *mut AurifyPluginHost,
    status: *const c_char,
    message: *const c_char,
) {
    let Some(host) = find_host(host) else { return };
    let status = match from_c_str(status) {
        Some("starting") => HealthStatus::Starting,
        Some("ready") => HealthStatus::Ready,
        Some("failed") => HealthStatus::Failed,
        _ => return,
    };
    host.server.set_health(status, from_c_str(message).map(str::to_string));
}

/// Whether the launch environment carried platform access. Without it
/// `aurify_plugin_platform_call` always fails.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_host_has_platform(host: *const AurifyPluginHost) -> bool {
    find_host(host).map(|h| h.platform.is_some()).unwrap_or(false)
}

/// One call to the platform on the person's behalf. `body_json` may be NULL. Returns the
/// answer as JSON (or `null` for an empty answer), or NULL with `error_out` filled.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_platform_call(
    host: *const AurifyPluginHost,
    method: *const c_char,
    path: *const c_char,
    body_json: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    let Some(host) = find_host(host) else {
        put_error(error_out, "host is NULL or already stopped");
        return ptr::null_mut();
    };
    let Some(platform) = host.platform.as_ref() else {
        put_error(error_out, "platform access was not configured for this process");
        return ptr::null_mut();
    };
    let (Some(method), Some(path)) = (from_c_str(method), from_c_str(path)) else {
        put_error(error_out, "method and path must be non-NULL UTF-8");
        return ptr::null_mut();
    };
    let body = match from_c_str(body_json) {
        None => None,
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => Some(value),
            Err(e) => {
                put_error(error_out, &format!("body_json is not valid JSON: {e}"));
                return ptr::null_mut();
            }
        },
    };
    match platform.call(method, path, body.as_ref()) {
        Ok(answer) => to_c_string(&answer.to_string()),
        Err(error) => {
            put_error(error_out, &error.to_string());
            ptr::null_mut()
        }
    }
}

/// Stops the local server and releases the host. Blocks until the server thread has
/// exited. NULL, a host already stopped, and a second call racing the first are all
/// accepted and do nothing.
#[no_mangle]
pub unsafe extern "C" fn aurify_plugin_host_stop(host: *mut AurifyPluginHost) {
    let Some(host) = take_host(host) else { return };
    host.server.stop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    unsafe extern "C" fn echo_operation(name: *const c_char, args: *const c_char, user_data: *mut c_void) -> *mut c_char {
        let name = CStr::from_ptr(name).to_str().unwrap();
        let args: Value = serde_json::from_str(CStr::from_ptr(args).to_str().unwrap()).unwrap();
        let counter = &mut *(user_data as *mut u32);
        *counter += 1;
        let reply = if name == "fail" {
            json!({ "error": { "code": "ON_PURPOSE", "message": "asked to fail" } })
        } else {
            json!({ "result": { "echo": args, "call": *counter } })
        };
        let text = CString::new(reply.to_string()).unwrap();
        aurify_plugin_string_alloc(text.as_ptr())
    }

    fn manifest() -> CString {
        CString::new(
            json!({
                "schemaVersion": 1, "id": "echo", "version": "1", "kind": "plugin", "title": "Echo",
                "entry": {"program": "echo"},
                "slots": [{"type": "apps-card", "screen": "main"}],
                "screens": [{"id": "main", "title": "Main", "modelOperation": "echo", "components": []}],
                "operations": [{"name": "echo"}, {"name": "fail"}]
            })
            .to_string(),
        )
        .unwrap()
    }

    fn post(port: u16, path: &str, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!(
            "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nX-Aurify-Plugin-Secret: 0123456789abcdef\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let status: u16 = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
    }

    #[test]
    fn strings_round_trip_through_the_library_allocator() {
        let original = CString::new("привет, plugin").unwrap();
        unsafe {
            let copy = aurify_plugin_string_alloc(original.as_ptr());
            assert_eq!(CStr::from_ptr(copy).to_str().unwrap(), "привет, plugin");
            aurify_plugin_string_free(copy);
            aurify_plugin_string_free(ptr::null_mut());
        }
    }

    #[test]
    fn validation_reports_through_the_c_boundary() {
        let broken = CString::new(r#"{"schemaVersion":1,"id":"x","version":"1","kind":"plugin","title":"X","entry":{"program":"x"}}"#).unwrap();
        unsafe {
            let errors = aurify_plugin_manifest_validate(broken.as_ptr());
            assert!(!errors.is_null());
            let text = CStr::from_ptr(errors).to_str().unwrap().to_string();
            aurify_plugin_string_free(errors);
            assert!(text.contains("apps-card"), "{text}");
            assert!(aurify_plugin_manifest_validate(manifest().as_ptr()).is_null());
        }
    }

    #[test]
    fn a_host_serves_operations_through_the_callback() {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let launch = CString::new(json!({ "port": port.to_string(), "secret": "0123456789abcdef" }).to_string()).unwrap();
        let mut calls: u32 = 0;
        let mut error: *mut c_char = ptr::null_mut();
        unsafe {
            let host = aurify_plugin_host_start(
                manifest().as_ptr(),
                launch.as_ptr(),
                Some(echo_operation),
                &mut calls as *mut u32 as *mut c_void,
                &mut error,
            );
            assert!(error.is_null(), "{}", CStr::from_ptr(error).to_str().unwrap());
            assert!(!host.is_null());
            assert_eq!(aurify_plugin_host_port(host), port);
            assert!(!aurify_plugin_host_has_platform(host));

            let (status, body) = post(port, "/plugin/v1/operations/echo", r#"{"args":{"x":1}}"#);
            assert_eq!(status, 200);
            assert!(body.contains(r#""echo":{"x":1}"#), "{body}");

            let (status, body) = post(port, "/plugin/v1/operations/fail", "{}");
            assert_eq!(status, 422);
            assert!(body.contains("ON_PURPOSE"), "{body}");

            aurify_plugin_host_stop(host);
            aurify_plugin_host_stop(ptr::null_mut());
        }
        assert_eq!(calls, 2);
    }

    #[test]
    fn a_host_stopped_twice_is_released_once_and_answers_nothing_after() {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let launch = CString::new(json!({ "port": port.to_string(), "secret": "0123456789abcdef" }).to_string()).unwrap();
        let mut calls: u32 = 0;
        let mut error: *mut c_char = ptr::null_mut();
        unsafe {
            let host = aurify_plugin_host_start(
                manifest().as_ptr(),
                launch.as_ptr(),
                Some(echo_operation),
                &mut calls as *mut u32 as *mut c_void,
                &mut error,
            );
            assert!(!host.is_null());
            assert_eq!(aurify_plugin_host_port(host), port);

            aurify_plugin_host_stop(host);
            aurify_plugin_host_stop(host);

            assert_eq!(aurify_plugin_host_port(host), 0);
            assert!(!aurify_plugin_host_has_platform(host));
            let mut call_error: *mut c_char = ptr::null_mut();
            let method = CString::new("GET").unwrap();
            let path = CString::new("/api/v1/me").unwrap();
            let answer = aurify_plugin_platform_call(host, method.as_ptr(), path.as_ptr(), ptr::null(), &mut call_error);
            assert!(answer.is_null());
            let text = CStr::from_ptr(call_error).to_str().unwrap().to_string();
            aurify_plugin_string_free(call_error);
            assert!(text.contains("already stopped"), "{text}");
        }
        // The listening socket lives on the accept thread, which stop() wakes but does
        // not join; it closes the socket a moment later.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", port)).is_ok() {
            assert!(std::time::Instant::now() < deadline, "the port must close after stop");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn a_null_launch_reads_the_environment_and_says_what_is_missing() {
        std::env::remove_var("AURIFY_PLUGIN_PORT");
        let mut error: *mut c_char = ptr::null_mut();
        unsafe {
            let host = aurify_plugin_host_start(manifest().as_ptr(), ptr::null(), Some(echo_operation), ptr::null_mut(), &mut error);
            assert!(host.is_null());
            let text = CStr::from_ptr(error).to_str().unwrap().to_string();
            aurify_plugin_string_free(error);
            assert!(text.contains("AURIFY_PLUGIN_PORT"), "{text}");
        }
    }
}
