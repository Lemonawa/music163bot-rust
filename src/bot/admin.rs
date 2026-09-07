//! Admin authorization for admin-only commands: the config admin list is the
//! single source of truth, and denial always explains itself in the chat's
//! language.

use std::sync::Arc;

use super::lang_command::resolve_chat_language_for;
use super::replies::send_reply_text;
use super::wiring::BotState;
use crate::i18n;
use crate::telegram::{Message, ResponseResult, TelegramBot as Bot};

pub(super) fn is_admin(msg: &Message, config: &crate::config::Config) -> bool {
    let Some(user) = msg.from.as_ref() else {
        return false;
    };
    config.bot_admin.contains(&user.id)
}

/// Check admin rights; on denial, reply with the localized denial message and
/// return `false`.
pub(super) async fn ensure_admin(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<bool> {
    if is_admin(msg, &state.config) {
        Ok(true)
    } else {
        let lang = resolve_chat_language_for(state, msg).await;
        send_reply_text(bot, msg, i18n::tr(&lang, "cmd_only_admin")).await?;
        Ok(false)
    }
}

/// Admin gate that also returns the acting user's id (used by confirm flows).
pub(super) async fn authorize_admin_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    command_name: &str,
) -> ResponseResult<Option<i64>> {
    if !ensure_admin(bot, msg, state).await? {
        return Ok(None);
    }

    let user_id = msg.from.as_ref().map_or(0, |u| u.id);
    tracing::info!(
        "{} command from user_id: {}, configured admins: {:?}",
        command_name,
        user_id,
        state.config.bot_admin
    );

    Ok(Some(user_id))
}
