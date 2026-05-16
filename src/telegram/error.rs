#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error {error_code}: {description}")]
    Api {
        error_code: i32,
        description: String,
    },

    #[error("Deserialization error: {0}")]
    Deserialize(#[from] serde_json::Error),
}

pub type ResponseResult<T> = std::result::Result<T, TelegramError>;
