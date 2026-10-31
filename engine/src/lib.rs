//! PowerDB: structured records and files in one database. See docs/DESIGN.md.

pub mod error;
pub mod storage;
pub mod util;

pub use error::{Error, Result};
pub use storage::{model, Db, Options};
