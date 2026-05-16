use std::time::Duration;

use super::api::TelegramBot;
use super::types::Update;

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
            tracing::error!("getUpdates error: {}", e);
            tokio::time::sleep(Duration::from_secs(5)).await;
            Vec::new()
        }
    }
}
