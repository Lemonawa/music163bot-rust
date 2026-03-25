use super::*;

pub(super) async fn handle_music_url(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    text: &str,
) -> ResponseResult<()> {
    if let Some(target) = parse_direct_music_target(text) {
        return dispatch_parsed_music_target(bot, msg, state, target).await;
    }

    let Some(url) = extract_first_url(text) else {
        send_reply_text(bot, msg, MUSIC_ID_EXTRACT_FAILED_TEXT).await?;
        return Ok(());
    };

    if !is_trusted_music_share_url(&url) {
        tracing::warn!(
            "Rejected untrusted share-link host: {}",
            sanitize_sensitive_text(&url)
        );
        send_reply_text(bot, msg, MUSIC_ID_EXTRACT_FAILED_TEXT).await?;
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
                sanitize_sensitive_text(&e.to_string())
            );
            send_reply_text(bot, msg, MUSIC_ID_EXTRACT_FAILED_TEXT).await?;
            return Ok(());
        }
    };

    if let Some(target) = parse_direct_music_target(&final_url) {
        dispatch_parsed_music_target(bot, msg, state, target).await
    } else {
        send_reply_text(bot, msg, MUSIC_ID_EXTRACT_FAILED_TEXT).await?;
        Ok(())
    }
}

pub(super) async fn handle_search_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let keyword = match args {
        Some(kw) if !kw.is_empty() => kw,
        _ => {
            send_reply_text(bot, msg, "请输入搜索关键词").await?;
            return Ok(());
        }
    };

    let search_msg = send_reply_message(bot, msg, "🔍 搜索中...").await?;

    match state.music_api.search_songs(&keyword, 10).await {
        Ok(songs) => {
            if songs.is_empty() {
                bot.edit_message_text(msg.chat.id, search_msg.id, "未找到相关歌曲")
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
            tracing::warn!("Search failed: {}", sanitize_sensitive_text(&e.to_string()));
            bot.edit_message_text(msg.chat.id, search_msg.id, "搜索失败，请稍后重试")
                .await?;
        }
    }

    Ok(())
}
