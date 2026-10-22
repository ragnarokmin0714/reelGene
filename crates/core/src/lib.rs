//! reelgene's engine: everything except the user interface.
//!
//! This crate never depends on a UI library (no egui, no tray, no terminal
//! progress bars). The window and the CLI are two equal consumers of its public
//! API, so every behaviour here can be tested headless.

pub mod crypto;
pub mod detect;
pub mod engine;
pub mod error;
pub mod hls;
pub mod http;
pub mod mux;
pub mod net;
pub mod paths;
pub mod pgp;
pub mod resolver;
pub mod store;
pub mod tools;

pub use error::{Error, Result};
