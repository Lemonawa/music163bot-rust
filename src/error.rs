use thiserror::Error;

use crate::telegram::TelegramError;

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
