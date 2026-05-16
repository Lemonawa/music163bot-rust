use std::time::Duration;

use super::api::TelegramBot;
use super::error::TelegramError;
use super::types::Update;
use crate::utils::sanitize_sensitive_text;

/// Perform a single long-poll call to getUpdates.
/// Returns the updates received, or an empty vec on error (after logging).
pub async fn poll_once(bot: &TelegramBot, offset: &mut i64) -> Vec<Update> {
    match bot
        .get_updates(*offset, 10, &["message", "callback_query", "inline_query"])
        .await
    {
        Ok(updates) => {
            if let Some(last) = updates.last() {
                *offset = last.update_id + 1;
            }
            updates
        }
        Err(e) => {
            tracing::error!("getUpdates error: {}", format_poll_error_for_log(&e));
            tokio::time::sleep(Duration::from_secs(5)).await;
            Vec::new()
        }
    }
}

fn format_poll_error_for_log(error: &TelegramError) -> String {
    sanitize_sensitive_text(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::error::TelegramError;

    #[test]
    fn format_poll_error_for_log_redacts_bot_token_in_url() {
        let synthetic = "error sending request for url (https://api.telegram.org/bot123456789:fake_test_token/getUpdates): connection refused".to_string();
        let err = TelegramError::Api {
            error_code: 502,
            description: synthetic,
        };

        let logged = super::format_poll_error_for_log(&err);

        assert!(
            !logged.contains("123456789:fake_test_token"),
            "logged message must not contain the bot token: {logged}"
        );
        assert!(
            logged.contains("/bot<redacted>"),
            "logged message should keep redaction marker: {logged}"
        );
    }
}
