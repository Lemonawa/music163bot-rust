use super::{
    Arc, Bot, BotState, CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup,
    MaybeInaccessibleMessage, Message, ReplyParameters, ResponseResult, send_reply_text,
};
use crate::database::Database;
use crate::i18n::{self, ChatLanguage};
use crate::telegram::{Chat, User};

use dashmap::DashMap;

/// Locales compiled into the binary (from `locales/*.yml`). Leaked on
/// purpose: the locale set is static for the process lifetime.
pub(super) fn locales() -> Vec<&'static str> {
    crate::_rust_i18n_available_locales()
        .into_iter()
        .map(|l| Box::leak(l.into_owned().into_boxed_str()) as &'static str)
        .collect()
}

/// Resolve the Chat Language for an incoming message: cached Language
/// Override, else the persisted override from the database (re-cached),
/// else auto-detection / Default Language per `i18n::resolve_chat_language`.
pub(super) async fn resolve_message(
    database: &Database,
    chat_languages: &DashMap<i64, String>,
    default_language: &str,
    msg: &Message,
) -> ChatLanguage {
    let is_private = msg.chat.type_ == "private";
    let sender_code = msg.from.as_ref().and_then(|u| u.language_code.as_deref());
    let override_lang = cached_override(database, chat_languages, msg.chat.id.0).await;
    i18n::resolve_chat_language(
        is_private,
        sender_code,
        override_lang.as_deref(),
        default_language,
    )
    .0
}

/// Resolve the Chat Language for an inline query. Inline queries behave like
/// a private chat with the querent (their user id == the private chat id),
/// so their Language Override and their Telegram `language_code` both apply.
pub(super) async fn resolve_inline(
    database: &Database,
    chat_languages: &DashMap<i64, String>,
    default_language: &str,
    from: &User,
) -> ChatLanguage {
    let override_lang = cached_override(database, chat_languages, from.id).await;
    i18n::resolve_chat_language(
        true,
        from.language_code.as_deref(),
        override_lang.as_deref(),
        default_language,
    )
    .0
}

/// Single lookup behind the Chat Language seam: shared override cache first,
/// then the database (re-caching on hit).
async fn cached_override(
    database: &Database,
    chat_languages: &DashMap<i64, String>,
    key: i64,
) -> Option<String> {
    match chat_languages.get(&key) {
        Some(entry) => Some(entry.value().clone()),
        None => match database.get_chat_language(key).await.ok().flatten() {
            Some(lang) => {
                chat_languages.insert(key, lang.clone());
                Some(lang)
            }
            None => None,
        },
    }
}

/// Persist a Language Override: cache and database stay coherent here, in
/// the one place that writes them.
async fn set_override(
    database: &Database,
    chat_languages: &DashMap<i64, String>,
    chat_id: i64,
    locale: &str,
) {
    chat_languages.insert(chat_id, locale.to_string());
    if let Err(e) = database.set_chat_language(chat_id, locale).await {
        tracing::warn!("Failed to persist chat language for {chat_id}: {}", e);
    }
}

/// Clear a Language Override (Auto): cache and database stay coherent here.
async fn clear_override(database: &Database, chat_languages: &DashMap<i64, String>, chat_id: i64) {
    chat_languages.remove(&chat_id);
    if let Err(e) = database.clear_chat_language(chat_id).await {
        tracing::warn!("Failed to clear chat language: {}", e);
    }
}

/// Outcome of the shared group-privilege check. Private chats and configured
/// bot admins pass without consulting Telegram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Privilege {
    Allowed,
    Denied,
    CheckFailed,
}

/// The one place that decides who may change a chat language: private chats
/// allow anyone, configured bot admins always pass, otherwise the Telegram
/// member status decides.
async fn check_privilege(bot: &Bot, bot_admin: &[i64], chat: &Chat, user_id: i64) -> Privilege {
    if chat.type_ == "private" || bot_admin.contains(&user_id) {
        return Privilege::Allowed;
    }
    match bot.get_chat_member(chat.id, user_id).await {
        Ok(member) if member.is_privileged() => Privilege::Allowed,
        Ok(_) => Privilege::Denied,
        Err(e) => {
            tracing::warn!(
                "getChatMember failed for chat {} user {}: {}",
                chat.id.0,
                user_id,
                super::sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            Privilege::CheckFailed
        }
    }
}

/// Build the `setMyCommands` payload for one locale, from `cmd_desc.*` keys.
/// Commands without a translation in that locale fall back to the default
/// (empty `language_code`) list registered separately.
#[must_use]
pub fn bot_commands_for_locale(
    lang: &crate::i18n::ChatLanguage,
) -> Vec<crate::telegram::BotCommand> {
    const COMMANDS: [&str; 11] = [
        "start",
        "music",
        "netease",
        "search",
        "lyric",
        "lang",
        "status",
        "about",
        "rmcache",
        "clearallcache",
        "help",
    ];

    COMMANDS
        .iter()
        .map(|cmd| {
            crate::telegram::BotCommand::new(
                *cmd,
                crate::i18n::tr(lang, &format!("cmd_desc.{cmd}")),
            )
        })
        .collect()
}

/// Register localized command menus with Telegram at startup: one list per
/// compiled locale (clients pick by their UI language) plus the default
/// list for languages we do not ship.
pub async fn register_bot_commands(bot: &Bot) {
    let fallback = bot_commands_for_locale(&crate::i18n::default_lang_zh());

    if let Err(e) = bot.set_my_commands(fallback).await {
        tracing::warn!("Failed to register default command menu: {}", e);
        return;
    }

    for locale in locales() {
        if locale == "zh" {
            continue; // zh is the default list
        }
        let lang = crate::i18n::ChatLanguage::new(locale);
        if let Err(e) = bot
            .set_my_commands(bot_commands_for_locale(&lang))
            .language_code(locale)
            .await
        {
            tracing::warn!("Failed to register {locale} command menu: {}", e);
        }
    }
    tracing::info!("Registered localized command menus for {:?}", locales());
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
    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
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
            let user_id = authorize_lang_change(bot, msg, state, &lang).await?;
            apply_lang_argument(bot, msg, state, &lang, &arg, user_id).await?;
        }
    }

    Ok(())
}

/// In groups, only Telegram admins (or configured bot admins) may change the
/// chat language; in private chats anyone may. Takes the already-resolved
/// language so the override lookup happens once. Returns the caller's user
/// id on success.
async fn authorize_lang_change(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    lang: &ChatLanguage,
) -> ResponseResult<Option<i64>> {
    let user_id = msg.from.as_ref().map_or(0, |u| u.id);

    match check_privilege(bot, &state.config.bot_admin, &msg.chat, user_id).await {
        Privilege::Allowed => Ok(Some(user_id)),
        Privilege::Denied => {
            send_reply_text(bot, msg, i18n::tr(lang, "lang_admin_only")).await?;
            Ok(None)
        }
        Privilege::CheckFailed => {
            send_reply_text(bot, msg, i18n::tr(lang, "lang_admin_check_failed")).await?;
            Ok(None)
        }
    }
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
            set_override(
                &state.database,
                &state.chat_languages,
                msg.chat.id.0,
                locale,
            )
            .await;
            send_reply_text(
                bot,
                msg,
                i18n::tr_with(&ChatLanguage::new(locale), "lang_set", "lang", &locale),
            )
            .await?;
        }
        Ok(None) => {
            clear_override(&state.database, &state.chat_languages, msg.chat.id.0).await;
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

    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        chat_msg,
    )
    .await;

    if action == "auto" {
        clear_override(&state.database, &state.chat_languages, chat_msg.chat.id.0).await;
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
    match check_privilege(bot, &state.config.bot_admin, &chat_msg.chat, user).await {
        Privilege::Allowed => {}
        Privilege::Denied => {
            bot.answer_callback_query(query.id.clone())
                .text(i18n::tr(&lang, "lang_admin_only"))
                .await?;
            return Ok(true);
        }
        Privilege::CheckFailed => {
            bot.answer_callback_query(query.id.clone())
                .text(i18n::tr(&lang, "lang_admin_check_failed"))
                .await?;
            return Ok(true);
        }
    }

    set_override(
        &state.database,
        &state.chat_languages,
        chat_msg.chat.id.0,
        action,
    )
    .await;
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
