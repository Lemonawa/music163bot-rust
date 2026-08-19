use super::{
    Arc, Bot, BotState, CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup,
    MaybeInaccessibleMessage, Message, ReplyParameters, ResponseResult, send_reply_text,
};
use crate::i18n::{self, ChatLanguage};

/// Locales compiled into the binary (from `locales/*.yml`).
pub(super) fn locales() -> Vec<&'static str> {
    crate::_rust_i18n_available_locales()
}

/// Callback data prefix used by the language keyboard buttons.
const LANG_CALLBACK_PREFIX: &str = "lang:set:";

/// Build the `/lang` selector keyboard from the compiled-in locales —
/// the extension interface: a new YAML file under `locales/` appears here
/// automatically. The Auto button clears the override.
#[must_use]
pub(super) fn build_lang_keyboard() -> InlineKeyboardMarkup {
    let mut buttons: Vec<InlineKeyboardButton> = locales()
        .iter()
        .map(|locale| {
            InlineKeyboardButton::callback(
                locale.to_uppercase(),
                format!("{LANG_CALLBACK_PREFIX}{locale}"),
            )
        })
        .collect();
    buttons.push(InlineKeyboardButton::callback(
        i18n::tr(&ChatLanguage::new("auto"), "lang_automatic"),
        format!("{LANG_CALLBACK_PREFIX}auto"),
    ));
    InlineKeyboardMarkup::new(vec![buttons])
}

pub(super) async fn handle_lang_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let lang = i18n::chat_language_for_message(
        &state.database,
        &state.chat_languages,
        msg,
        &state.config.default_language,
    )
    .await;

    match args {
        None => {
            let prompt = i18n::tr(&lang, "lang_select");
            bot.send_message(msg.chat.id, prompt)
                .reply_parameters(ReplyParameters::new(msg.id))
                .reply_markup(build_lang_keyboard())
                .await?;
        }
        Some(arg) => {
            let user_id = authorize_lang_change(bot, msg, state).await?;
            apply_lang_argument(bot, msg, state, &lang, &arg, user_id).await?;
        }
    }

    Ok(())
}

/// In groups, only Telegram admins (or configured bot admins) may change the
/// chat language; in private chats anyone may. Returns the caller's user id
/// on success.
async fn authorize_lang_change(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<Option<i64>> {
    let user_id = msg.from.as_ref().map_or(0, |u| u.id);

    if msg.chat.type_ == "private" {
        return Ok(Some(user_id));
    }

    if state.config.bot_admin.contains(&user_id) {
        return Ok(Some(user_id));
    }

    let lang = i18n::chat_language_for_message(
        &state.database,
        &state.chat_languages,
        msg,
        &state.config.default_language,
    )
    .await;

    match bot.get_chat_member(msg.chat.id, user_id).await {
        Ok(member) if member.is_privileged() => Ok(Some(user_id)),
        Ok(_) => {
            send_reply_text(bot, msg, i18n::tr(&lang, "lang_admin_only")).await?;
            Ok(None)
        }
        Err(e) => {
            tracing::warn!(
                "getChatMember failed for chat {} user {}: {}",
                msg.chat.id.0,
                user_id,
                super::sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            send_reply_text(bot, msg, i18n::tr(&lang, "lang_admin_check_failed")).await?;
            Ok(None)
        }
    }
}

async fn lang_fallback(state: &Arc<BotState>, msg: &Message) -> ChatLanguage {
    i18n::chat_language_for_message(
        &state.database,
        &state.chat_languages,
        msg,
        &state.config.default_language,
    )
    .await
}

async fn apply_lang_argument(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    lang: &ChatLanguage,
    arg: &str,
    _user_id: Option<i64>,
) -> ResponseResult<()> {
    match i18n::parse_lang_argument(arg) {
        Ok(Some(locale)) => {
            persist_language(state, msg.chat.id.0, locale).await;
            send_reply_text(
                bot,
                msg,
                i18n::tr_with(&ChatLanguage::new(locale), "lang_set", "lang", &locale),
            )
            .await?;
        }
        Ok(None) => {
            state.chat_languages.remove(&msg.chat.id.0);
            if let Err(e) = state.database.clear_chat_language(msg.chat.id.0).await {
                tracing::warn!("Failed to clear chat language: {}", e);
            }
            send_reply_text(bot, msg, i18n::tr(lang, "lang_auto")).await?;
        }
        Err(()) => {
            let locales = locales().join(", ");
            send_reply_text(
                bot,
                msg,
                i18n::tr_with(lang, "lang_unknown", "locales", &locales),
            )
            .await?;
        }
    }
    Ok(())
}

async fn persist_language(state: &Arc<BotState>, chat_id: i64, locale: &str) {
    state.chat_languages.insert(chat_id, locale.to_string());
    if let Err(e) = state.database.set_chat_language(chat_id, locale).await {
        tracing::warn!("Failed to persist chat language for {chat_id}: {}", e);
    }
}

/// Handle `lang:set:<locale|auto>` callback buttons.
pub(super) async fn handle_lang_callback(
    bot: &Bot,
    query: &CallbackQuery,
    state: &Arc<BotState>,
    action: &str,
) -> ResponseResult<bool> {
    let Some(msg) = query.message.as_ref() else {
        return Ok(false);
    };
    let chat_msg = match msg {
        MaybeInaccessibleMessage::Regular(m) => m,
        MaybeInaccessibleMessage::Inaccessible { .. } => return Ok(false),
    };

    let user = query.from.id;

    let lang = lang_fallback(state, chat_msg).await;

    if action == "auto" {
        state.chat_languages.remove(&chat_msg.chat.id.0);
        if let Err(e) = state.database.clear_chat_language(chat_msg.chat.id.0).await {
            tracing::warn!("Failed to clear chat language: {}", e);
        }
        bot.answer_callback_query(query.id.clone())
            .text(i18n::tr(&lang, "lang_auto"))
            .await?;
        return Ok(true);
    }

    if !i18n::is_supported_locale(action) {
        bot.answer_callback_query(query.id.clone())
            .text(i18n::tr(&lang, "invalid_operation"))
            .await?;
        return Ok(true);
    }

    // Authorization: callback source must be privileged in groups.
    if chat_msg.chat.type_ != "private" && !state.config.bot_admin.contains(&user) {
        match bot.get_chat_member(chat_msg.chat.id, user).await {
            Ok(member) if member.is_privileged() => {}
            Ok(_) => {
                bot.answer_callback_query(query.id.clone())
                    .text(i18n::tr(&lang, "lang_admin_only"))
                    .await?;
                return Ok(true);
            }
            Err(e) => {
                tracing::warn!("getChatMember failed: {}", e);
                bot.answer_callback_query(query.id.clone())
                    .text(i18n::tr(&lang, "lang_admin_check_failed"))
                    .await?;
                return Ok(true);
            }
        }
    }

    persist_language(state, chat_msg.chat.id.0, action).await;
    bot.answer_callback_query(query.id.clone())
        .text(i18n::tr_with(
            &ChatLanguage::new(action),
            "lang_set",
            "lang",
            &action,
        ))
        .await?;
    Ok(true)
}
