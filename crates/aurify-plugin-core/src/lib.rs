//! Plugin contract for the Aurify client.
//!
//! A plugin is a description of screens plus a program on the person's machine. The
//! client draws the screens with its own components and talks to the program directly
//! over loopback. This crate is that contract, in the form the program needs it:
//!
//! * [`manifest`] — what the plugin declares, and the rules the client enforces on it
//! * [`launch`] — what the client hands the process when it starts it
//! * [`local_server`] — the loopback protocol the process must serve
//! * [`platform`] — calls back into the platform on the person's behalf

pub mod launch;
pub mod local_server;
pub mod manifest;
pub mod platform;

pub use launch::{LaunchContext, LaunchError, LaunchValues};
pub use local_server::{Health, HealthStatus, LocalServer, OperationError, OperationHandler, StartError};
pub use manifest::{Kind, Manifest};
pub use platform::{PlatformClient, PlatformError};

/// Version of this crate, reported by bindings so a mismatch between the native
/// library and its wrapper is visible instead of silent.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
