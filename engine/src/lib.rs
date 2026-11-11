//! PowerDB: structured records and files in one database. See docs/DESIGN.md.

pub mod api;
pub mod auth;
pub mod client;
pub mod config;
pub mod error;
pub mod failpoint;
pub mod fileio;
pub mod ops;
pub mod security;
pub mod server;
pub mod storage;
pub mod util;

pub use error::{Error, Result};
pub use storage::{Db, Durability, Options, model};
