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
    #[must_use]
    pub fn sanitized_chain(&self) -> String {
        sanitize_sensitive_text(&format_error_chain(self))
    }

    /// The server's flood-wait hint (seconds) when this error chain contains
    /// a rate-limited Telegram API response. Typed source for retry pacing —
    /// replaces regex-scraping the rendered message.
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        let mut current: Option<&dyn std::error::Error> = Some(self);
        while let Some(err) = current {
            if let Some(telegram_err) = err.downcast_ref::<TelegramError>()
                && let Some(secs) = telegram_err.retry_after_secs()
            {
                return Some(secs);
            }
            // Raw-upload errors embed a "(retry after N)" hint in their text
            // (the raw response parser folds it into the message).
            if let Some(secs) = crate::utils::extract_retry_after_seconds(&err.to_string()) {
                return Some(secs);
            }
            current = err.source();
        }
        None
    }

    /// True when any layer of this error chain is a Telegram rate limit.
    #[must_use]
    pub fn is_rate_limit(&self) -> bool {
        let mut current: Option<&dyn std::error::Error> = Some(self);
        while let Some(err) = current {
            if let Some(telegram_err) = err.downcast_ref::<TelegramError>()
                && telegram_err.is_rate_limit()
            {
                return true;
            }
            if crate::utils::extract_retry_after_seconds(&err.to_string()).is_some() {
                return true;
            }
            current = err.source();
        }
        false
    }
}

/// Same as [`BotError::sanitized_chain`] for any error type (e.g. a
/// `TelegramError` or `anyhow::Error` before it is wrapped).
#[must_use]
pub fn sanitized_error_chain(error: &dyn std::error::Error) -> String {
    sanitize_sensitive_text(&format_error_chain(error))
}
