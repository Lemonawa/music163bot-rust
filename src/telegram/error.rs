#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error {error_code}: {description}")]
    Api {
        error_code: i32,
        description: String,
        /// Server-provided wait hint (seconds) on 429 flood-wait responses;
        /// absent on other errors.
        #[doc(hidden)]
        retry_after: Option<u64>,
    },

    #[error("Deserialization error: {0}")]
    Deserialize(#[from] serde_json::Error),
}

impl TelegramError {
    /// The server's flood-wait hint, if this is a rate-limit error that
    /// carries one. The single typed source for retry pacing — callers no
    /// longer parse "retry after N" out of the rendered message.
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Api {
                error_code: 429,
                retry_after: Some(secs),
                ..
            } => Some(*secs),
            _ => None,
        }
    }

    /// True when this error is a rate limit (flood wait), with or without a
    /// known delay.
    #[must_use]
    pub fn is_rate_limit(&self) -> bool {
        matches!(
            self,
            Self::Api {
                error_code: 429,
                ..
            }
        )
    }
}

pub type ResponseResult<T> = std::result::Result<T, TelegramError>;
