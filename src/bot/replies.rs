//! Reply helpers: the one way handlers talk back to a chat. Every function
//! here sends (or edits) a Telegram message and handles its own failure
//! logging; callers only build the text.

use crate::error::sanitized_error_chain;
use crate::telegram::{
    ChatId, Message, MessageId, ParseMode, ReplyParameters, ResponseResult, TelegramBot as Bot,
};
use crate::utils::extract_retry_after_seconds;

pub(super) async fn send_reply_message(
    bot: &Bot,
    msg: &Message,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    bot.send_message(msg.chat.id, text)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await
}

pub(super) async fn send_reply_text(
    bot: &Bot,
    msg: &Message,
    text: impl Into<String>,
) -> ResponseResult<()> {
    send_reply_message(bot, msg, text).await?;
    Ok(())
}

pub(super) async fn send_reply_html(
    bot: &Bot,
    msg: &Message,
    text: impl Into<String>,
) -> ResponseResult<()> {
    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;
    Ok(())
}

/// Demand arguments for a command: replies with `prompt` and returns `None`
/// when the argument list is empty.
pub(super) async fn require_command_args_or_reply(
    bot: &Bot,
    msg: &Message,
    args: Option<String>,
    prompt: &str,
) -> ResponseResult<Option<String>> {
    let args = args.unwrap_or_default();
    if args.is_empty() {
        send_reply_text(bot, msg, prompt).await?;
        Ok(None)
    } else {
        Ok(Some(args))
    }
}

pub(super) async fn edit_status_message_resilient(
    bot: &Bot,
    chat_id: ChatId,
    message_id: MessageId,
    text: impl Into<String>,
) {
    let text = text.into();
    if let Err(e) = bot
        .edit_message_text(chat_id, message_id, text.clone())
        .await
    {
        let sanitized = sanitized_error_chain(&e);
        if let Some(delay_secs) = extract_retry_after_seconds(&sanitized) {
            let bot = bot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs.saturating_add(1)))
                    .await;
                if let Err(retry_err) = bot.edit_message_text(chat_id, message_id, text).await {
                    tracing::debug!(
                        "Status message edit retry failed: {}",
                        sanitized_error_chain(&retry_err)
                    );
                }
            });
        } else {
            tracing::debug!("Status message edit failed: {}", sanitized);
        }
    }
}

pub(super) async fn delete_status_message_resilient(
    bot: &Bot,
    chat_id: ChatId,
    message_id: MessageId,
) {
    if let Err(e) = bot.delete_message(chat_id, message_id).await {
        let sanitized = sanitized_error_chain(&e);
        if let Some(delay_secs) = extract_retry_after_seconds(&sanitized) {
            let bot = bot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs.saturating_add(1)))
                    .await;
                if let Err(retry_err) = bot.delete_message(chat_id, message_id).await {
                    tracing::debug!(
                        "Status message delete retry failed: {}",
                        sanitized_error_chain(&retry_err)
                    );
                }
            });
        } else {
            tracing::debug!("Status message delete failed: {}", sanitized);
        }
    }
}
