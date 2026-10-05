//! gitlarp-core: ALL shared domain logic. Every shell (cli, server,
//! worker) is a thin adapter over this crate: edit here, and
//! everywhere else just works.

pub mod crypto;
pub mod date;
pub mod engine;
pub mod gh;
pub mod plan;
pub mod runner;
pub mod schedule;
pub mod store;

pub mod http;
pub use http::{block_on, BoxFut, HttpRequest, HttpResponse, Runtime};
pub use serde_json;

use std::fmt;

/// Every core error carries an HTTP-appropriate status: 400/401 flow
/// through to clients, anything else maps to a 5xx upstream failure.
#[derive(Debug, Clone)]
pub struct Error {
    pub status: u16,
    pub message: String,
}

impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Error { status, message: message.into() }
    }

    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
