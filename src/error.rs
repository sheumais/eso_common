//! Error type used across the crate.
//!
//! The C++ code mostly reported failures with `return PrintError(...)`, i.e. "log the message,
//! return false".  Here the equivalent is `return Err(eso_error!(...))`, which logs the message and
//! produces an [`EsoError`] carrying the same text.

use std::fmt;

#[derive(Debug, Clone)]
pub struct EsoError {
    message: String,
}

impl EsoError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for EsoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EsoError {}

impl From<std::io::Error> for EsoError {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, EsoError>;
