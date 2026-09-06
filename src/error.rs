use thiserror::Error;

use crate::telegram::TelegramError;
use crate::utils::{format_error_chain, sanitize_sensitive_text};

#[derive(Error, Debug)]
pub enum BotError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Failed to build HTTP client: {0}")]
    HttpClientBuild(String),

    #[error("Telegram error: {0}")]
    Telegram(#[from] TelegramError),

    #[error("Music API error: {0}")]
    MusicApi(String),

    #[error("File operation error: {0}")]
    FileOperation(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, BotError>;

impl BotError {
    /// The one way to turn any error into text safe to show or log: the full
    /// error chain, redacted. Every site that previously spelled out
    /// `sanitize_sensitive_text(&format_error_chain(&e))` — or forgot the
    /// sanitize half — goes through here instead.
    pub fn sanitized_chain(&self) -> String {
        sanitize_sensitive_text(&format_error_chain(self))
    }
}

/// Same as [`BotError::sanitized_chain`] for any error type (e.g. a
/// `TelegramError` or `anyhow::Error` before it is wrapped).
pub fn sanitized_error_chain(error: &dyn std::error::Error) -> String {
    sanitize_sensitive_text(&format_error_chain(error))
}
