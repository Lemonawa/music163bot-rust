use super::{
    Arc, Bot, BotState, InlineKeyboardButton, InlineKeyboardMarkup, Message, ResponseResult,
    append_search_result_line, dispatch_parsed_music_target, extract_first_trusted_music_share_url,
    format_artists, is_known_non_song_share_url, parse_direct_music_target, resolve_message,
    sanitize_sensitive_text, send_reply_message, send_reply_text,
};
use crate::i18n;

pub(super) async fn handle_music_url(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    text: &str,
) -> ResponseResult<()> {
    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    if let Some(target) = parse_direct_music_target(text) {
        return dispatch_parsed_music_target(bot, msg, state, target).await;
    }

    let Some(url) = extract_first_trusted_music_share_url(text) else {
        send_reply_text(bot, msg, i18n::tr(&lang, "music_id_extract_failed")).await?;
        return Ok(());
    };

    if is_known_non_song_share_url(&url) {
        tracing::debug!("Ignoring known non-song share page");
        return Ok(());
    }

    if let Some(target) = parse_direct_music_target(&url) {
        return dispatch_parsed_music_target(bot, msg, state, target).await;
    }

    let final_url = match state.music_api.resolve_share_link(&url).await {
        Ok(final_url) => final_url.to_string(),
        Err(e) => {
            tracing::warn!(
                "Failed to resolve share link: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            send_reply_text(bot, msg, i18n::tr(&lang, "music_id_extract_failed")).await?;
            return Ok(());
        }
    };

    if is_known_non_song_share_url(&final_url) {
        tracing::debug!(
            "Ignoring share link resolved to a known non-song page: {}",
            sanitize_sensitive_text(&final_url)
        );
        return Ok(());
    }

    if let Some(target) = parse_direct_music_target(&final_url) {
        dispatch_parsed_music_target(bot, msg, state, target).await
    } else {
        send_reply_text(bot, msg, i18n::tr(&lang, "music_id_extract_failed")).await?;
        Ok(())
    }
}

pub(super) async fn handle_search_command(
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
    let keyword = match args {
        Some(kw) if !kw.is_empty() => kw,
        _ => {
            send_reply_text(bot, msg, i18n::tr(&lang, "search_need_keyword")).await?;
            return Ok(());
        }
    };

    let search_msg = send_reply_message(bot, msg, i18n::tr(&lang, "search_searching")).await?;

    match state.music_api.search_songs(&keyword, 10).await {
        Ok(songs) => {
            if songs.is_empty() {
                bot.edit_message_text(
                    msg.chat.id,
                    search_msg.id,
                    i18n::tr(&lang, "search_no_results"),
                )
                .await?;
                return Ok(());
            }

            let mut results = String::new();
            let mut buttons = Vec::with_capacity(songs.len().min(8));

            for (i, song) in songs.iter().take(8).enumerate() {
                let artists = format_artists(&song.artists);
                append_search_result_line(&mut results, i + 1, &song.name, &artists);
                buttons.push(InlineKeyboardButton::callback(
                    (i + 1).to_string(),
                    format!("music {}", song.id),
                ));
            }

            let keyboard = InlineKeyboardMarkup::new(vec![buttons]);

            bot.edit_message_text(msg.chat.id, search_msg.id, results)
                .reply_markup(keyboard)
                .await?;
        }
        Err(e) => {
            tracing::warn!(
                "Search failed: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            bot.edit_message_text(msg.chat.id, search_msg.id, i18n::tr(&lang, "search_failed"))
                .await?;
        }
    }

    Ok(())
}
