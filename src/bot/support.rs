use super::{
    Arc, Bot, BotState, Bytes, CallbackQuery, ChatId, InlineQuery, InlineQueryResult,
    InlineQueryResultArticle, InputMessageContent, InputMessageContentText,
    MaybeInaccessibleMessage, Message, ParseMode, RawDocumentParams, ReplyParameters,
    ResponseResult, StatusTextParams, acquire_upload_client, acquire_upload_permit,
    build_status_text, clean_filename, clearallcache_confirmation_prompt, ensure_admin,
    format_artists, format_speed_line, format_uptime, handle_lang_callback, join_futures,
    parse_inline_query_keyword, parse_music_id, parse_song_id_or_search_first_result,
    process_music, raw_send_document_bytes, require_command_args_or_reply, resolve_inline,
    resolve_message, rmcache_usage_prompt, sample_resource_snapshot, sanitize_sensitive_text,
    send_reply_html, send_reply_message, send_reply_text, u64_to_i64_saturating,
};
use crate::i18n::{self, ChatLanguage};

pub(super) async fn handle_lyric_command(
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
    let Some(args) =
        require_command_args_or_reply(bot, msg, args, &i18n::tr(&lang, "music_need_id")).await?
    else {
        return Ok(());
    };

    let Some(music_id) =
        parse_song_id_or_search_first_result(bot, msg, state, &args, "Lyric search failed").await?
    else {
        return Ok(());
    };

    let status_msg = send_reply_message(bot, msg, i18n::tr(&lang, "lyric_searching")).await?;

    match join_futures(
        state.music_api.get_song_lyric(music_id),
        state.music_api.get_song_detail(music_id),
    )
    .await
    {
        (Ok(lyric), detail_result) => {
            handle_lyric_success(
                bot,
                msg,
                state,
                &lang,
                &status_msg,
                music_id,
                lyric,
                detail_result,
            )
            .await?;
        }
        (Err(e), _) => {
            tracing::warn!(
                "Failed to fetch lyric: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            bot.edit_message_text(msg.chat.id, status_msg.id, i18n::tr(&lang, "lyric_failed"))
                .await?;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_lyric_success(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    lang: &ChatLanguage,
    status_msg: &Message,
    music_id: u64,
    lyric: String,
    detail_result: Result<Arc<crate::music_api::SongDetail>, impl std::fmt::Display>,
) -> ResponseResult<()> {
    if lyric.trim().is_empty() || lyric == "No lyrics available" {
        bot.edit_message_text(msg.chat.id, status_msg.id, i18n::tr(lang, "lyric_none"))
            .await?;
        return Ok(());
    }

    let song_detail = match detail_result {
        Ok(detail) => detail,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch lyric song detail for {music_id}: {}",
                sanitize_sensitive_text(&e.to_string())
            );
            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                i18n::tr(lang, "lyric_song_info_failed"),
            )
            .await?;
            return Ok(());
        }
    };

    let artists = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
    let lrc_filename = clean_filename(&format!("{} - {}.lrc", artists, song_detail.name));
    let lyric_bytes = Bytes::from(lyric.into_bytes());

    let (client_result, permit_result) = join_futures(
        acquire_upload_client(state),
        acquire_upload_permit(&state.upload_semaphore),
    )
    .await;

    let (_upload_bot, raw_client, api_base_url) = match client_result {
        Ok(bundle) => bundle,
        Err(e) => {
            tracing::warn!(
                "Failed to initialize lyric upload client: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                i18n::tr(lang, "lyric_upload_client_failed"),
            )
            .await?;
            return Ok(());
        }
    };
    let _upload_permit = match permit_result {
        Ok(permit) => permit,
        Err(e) => {
            tracing::warn!(
                "Failed to acquire lyric upload permit: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                i18n::tr(lang, "lyric_upload_permit_failed"),
            )
            .await?;
            return Ok(());
        }
    };
    let params = RawDocumentParams {
        chat_id: msg.chat.id.0,
        caption: None,
        reply_to_message_id: msg.id.0,
        reply_markup_json: None,
    };

    let upload_result = raw_send_document_bytes(
        &raw_client,
        &api_base_url,
        &lrc_filename,
        lyric_bytes,
        &params,
    )
    .await;

    match upload_result {
        Ok(_) => {
            if let Err(e) = bot.delete_message(msg.chat.id, status_msg.id).await {
                tracing::debug!(
                    "Failed to delete lyric status message: {}",
                    sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "Failed to upload lyric file: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                i18n::tr(lang, "lyric_send_failed"),
            )
            .await?;
        }
    }

    Ok(())
}

pub(super) async fn handle_status_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    if !ensure_admin(bot, msg, state).await? {
        return Ok(());
    }

    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    let user_id = msg.from.as_ref().map_or(0, |u| u.id);
    let chat_id = msg.chat.id.0;

    let (total_count, user_count, chat_count) = state
        .database
        .count_status_stats(user_id, chat_id)
        .await
        .unwrap_or((0, 0, 0));
    let cache_snapshot = state.runtime_metrics.cache_snapshot();
    let resource_snapshot = sample_resource_snapshot();
    let (download_speed, upload_speed) = state.runtime_metrics.speed_snapshots();
    let uptime = format_uptime(state.runtime_metrics.uptime());
    let download_line = format_speed_line(
        &lang,
        &i18n::tr(&lang, "status_label_download"),
        download_speed,
    );
    let upload_line =
        format_speed_line(&lang, &i18n::tr(&lang, "status_label_upload"), upload_speed);
    let status_text = build_status_text(&StatusTextParams {
        lang: &lang,
        total_count,
        user_count,
        chat_count,
        cache_snapshot,
        resource_snapshot,
        uptime: &uptime,
        download_line: &download_line,
        upload_line: &upload_line,
    });

    bot.send_message(msg.chat.id, status_text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    Ok(())
}

pub(super) async fn handle_rmcache_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let Some(_user_id) = authorize_admin_command(bot, msg, state, "rmcache").await? else {
        return Ok(());
    };

    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    let args = args.unwrap_or_default();

    if args.is_empty() {
        send_reply_html(bot, msg, rmcache_usage_prompt(&lang)).await?;
        return Ok(());
    }

    if let Some(music_id) = parse_music_id(&args) {
        let music_id_i64 = u64_to_i64_saturating(music_id);

        if let Ok(Some(song_info)) = state.database.get_song_by_music_id(music_id_i64).await {
            match state.database.delete_song_by_music_id(music_id_i64).await {
                Ok(deleted) => {
                    if deleted {
                        send_reply_text(
                            bot,
                            msg,
                            i18n::tr_with(&lang, "rmcache_deleted", "name", &song_info.song_name),
                        )
                        .await?;
                    } else {
                        send_reply_text(bot, msg, i18n::tr(&lang, "rmcache_not_cached")).await?;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to delete cached song {music_id}: {}",
                        sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                    );
                    send_reply_text(bot, msg, i18n::tr(&lang, "rmcache_delete_failed")).await?;
                }
            }
        } else {
            send_reply_text(bot, msg, i18n::tr(&lang, "rmcache_not_cached")).await?;
        }
    } else {
        send_reply_text(bot, msg, i18n::tr(&lang, "rmcache_invalid_id")).await?;
    }

    Ok(())
}

pub(super) async fn handle_clearallcache_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let Some(_user_id) = authorize_admin_command(bot, msg, state, "clearallcache").await? else {
        return Ok(());
    };

    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    send_reply_html(bot, msg, clearallcache_confirmation_prompt(&lang)).await?;
    let user_id = msg.from.as_ref().map_or(0, |u| u.id);
    prune_expired_confirmations(&state.clearallcache_confirms);
    state
        .clearallcache_confirms
        .insert((user_id, msg.chat.id), std::time::Instant::now());

    Ok(())
}

pub(super) async fn handle_clearallcache_confirm_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let Some(user_id) = authorize_admin_command(bot, msg, state, "clearallcache confirm").await?
    else {
        return Ok(());
    };

    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;

    let should_allow = state
        .clearallcache_confirms
        .remove(&(user_id, msg.chat.id))
        .and_then(|(_, at)| (at.elapsed() <= CLEARALLCACHE_CONFIRM_WINDOW).then_some(()))
        .is_some();
    if !should_allow {
        send_reply_html(bot, msg, clearallcache_confirmation_prompt(&lang)).await?;
        return Ok(());
    }

    let status_msg = send_reply_message(bot, msg, i18n::tr(&lang, "clearallcache_started")).await?;

    match state.database.clear_all_songs().await {
        Ok(count) => {
            if let Err(e) = state.database.optimize().await {
                tracing::warn!("Database optimization failed after clear: {}", e);
            }

            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                i18n::tr_with(&lang, "clearallcache_done", "count", &count),
            )
            .await?;

            tracing::info!(
                "Admin {} cleared all cache, {} records deleted",
                user_id,
                count
            );
        }
        Err(e) => {
            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                i18n::tr(&lang, "clearallcache_failed"),
            )
            .await?;

            tracing::error!(
                "Failed to clear all cache: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
        }
    }

    Ok(())
}

pub(super) async fn ensure_admin_user_id(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<Option<i64>> {
    let user_id = msg.from.as_ref().map_or(0, |u| u.id);
    if ensure_admin(bot, msg, state).await? {
        Ok(Some(user_id))
    } else {
        Ok(None)
    }
}

pub(super) async fn authorize_admin_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    command_name: &str,
) -> ResponseResult<Option<i64>> {
    let Some(user_id) = ensure_admin_user_id(bot, msg, state).await? else {
        return Ok(None);
    };

    tracing::info!(
        "{} command from user_id: {}, configured admins: {:?}",
        command_name,
        user_id,
        state.config.bot_admin
    );

    Ok(Some(user_id))
}

pub(super) async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    state: Arc<BotState>,
) -> ResponseResult<()> {
    let callback_data = query.data.clone();
    let callback_lang = match query.message.as_ref() {
        Some(MaybeInaccessibleMessage::Regular(msg)) => Some(
            resolve_message(
                &state.database,
                &state.chat_languages,
                &state.config.default_language,
                msg,
            )
            .await,
        ),
        _ => None,
    };
    let tr = |key: &str| {
        callback_lang.as_ref().map_or_else(
            || i18n::tr(&i18n::default_language(&state.config), key),
            |l| i18n::tr(l, key),
        )
    };

    if let Some(data) = callback_data.as_deref()
        && let Some((cmd, rest)) = data.split_once(' ')
        && cmd == "music"
        && let Ok(music_id) = rest.trim_start().parse::<u64>()
        && let Some(MaybeInaccessibleMessage::Regular(msg)) = &query.message
    {
        match process_music(&bot, msg, &state, music_id).await {
            Ok(()) => {
                bot.answer_callback_query(query.id)
                    .text(tr("callback_download_started"))
                    .await?;
            }
            Err(e) => {
                tracing::error!(
                    "Error processing music from callback: {}",
                    sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                );
                bot.answer_callback_query(query.id)
                    .text(tr("error_generic"))
                    .await?;
            }
        }
        return Ok(());
    }

    if let Some(action) = callback_data
        .as_deref()
        .and_then(|d| d.strip_prefix("lang:set:"))
        && handle_lang_callback(&bot, &query, &state, action).await?
    {
        return Ok(());
    }

    bot.answer_callback_query(query.id)
        .text(tr("invalid_operation"))
        .await?;

    Ok(())
}

pub(super) async fn handle_inline_query(
    bot: Bot,
    query: InlineQuery,
    state: Arc<BotState>,
) -> ResponseResult<()> {
    // Inline queries always come from a private "chat" with the bot: resolve
    // the language from the querent's user id (== private chat id).
    let lang = resolve_inline(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        &query.from,
    )
    .await;

    // Support "search" prefix for consistency with Go version
    let (search_keyword, is_search_cmd) = parse_inline_query_keyword(&query.query);

    if search_keyword.is_empty() {
        if is_search_cmd {
            let help_article = InlineQueryResultArticle::new(
                "search_help",
                i18n::tr(&lang, "inline_search_need_keyword"),
                InputMessageContent::Text(InputMessageContentText::new(i18n::tr_with(
                    &lang,
                    "inline_search_usage",
                    "bot_username",
                    &state.bot_username,
                ))),
            )
            .description(i18n::tr(&lang, "inline_search_help_desc"));

            bot.answer_inline_query(query.id, vec![InlineQueryResult::Article(help_article)])
                .await?;
        } else {
            let help_article = InlineQueryResultArticle::new(
                "usage_help",
                i18n::tr(&lang, "inline_howto_title"),
                InputMessageContent::Text(InputMessageContentText::new(i18n::tr(
                    &lang,
                    "inline_howto_body",
                ))),
            )
            .description(i18n::tr(&lang, "inline_howto_desc"));

            bot.answer_inline_query(query.id, vec![InlineQueryResult::Article(help_article)])
                .await?;
        }
        return Ok(());
    }

    match state.music_api.search_songs(search_keyword, 10).await {
        Ok(songs) => {
            let mut results = Vec::with_capacity(songs.len().min(10));

            for (i, song) in songs.iter().take(10).enumerate() {
                let artists = format_artists(&song.artists);

                let article = InlineQueryResultArticle::new(
                    format!("{}_{}", song.id, i),
                    &song.name,
                    InputMessageContent::Text(InputMessageContentText::new(format!(
                        "/netease {}",
                        song.id
                    ))),
                )
                .description(artists);

                results.push(InlineQueryResult::Article(article));
            }

            bot.answer_inline_query(query.id, results)
                .cache_time(300)
                .await?;
        }
        Err(e) => {
            tracing::error!(
                "Inline search error: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            let error_article = InlineQueryResultArticle::new(
                "search_error",
                i18n::tr(&lang, "inline_search_error_title"),
                InputMessageContent::Text(InputMessageContentText::new(i18n::tr(
                    &lang,
                    "inline_search_error_desc",
                ))),
            )
            .description(i18n::tr(&lang, "inline_search_error_desc"));

            bot.answer_inline_query(query.id, vec![InlineQueryResult::Article(error_article)])
                .await?;
        }
    }

    Ok(())
}

pub(super) const CLEARALLCACHE_CONFIRM_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(30);

pub(super) fn prune_expired_confirmations(
    confirms: &dashmap::DashMap<(i64, ChatId), std::time::Instant>,
) {
    confirms.retain(|_, at| at.elapsed() <= CLEARALLCACHE_CONFIRM_WINDOW);
}

/// Build caption with exact format:
/// 「Title」- Artists
/// 专辑: Album
/// #网易云音乐 #ext {sizeMB}MB {kbps}kbps
/// via @`BotName`
#[allow(clippy::too_many_arguments)]
pub(super) fn build_caption(
    lang: &ChatLanguage,
    title: &str,
    artists: &str,
    album: &str,
    file_ext: &str,
    size_bytes: i64,
    bitrate_bps: i64,
    bot_username: &str,
) -> String {
    let size_mb = format_size_mb(size_bytes);
    let kbps = format_bitrate_kbps(bitrate_bps);
    i18n::tr_many(
        lang,
        "caption",
        &[
            ("title", &title),
            ("artists", &artists),
            ("album", &album),
            ("ext", &file_ext),
            ("size_mb", &size_mb),
            ("kbps", &kbps),
            ("bot_username", &bot_username),
        ],
    )
}

fn format_size_mb(bytes: i64) -> String {
    let bytes = bytes.unsigned_abs();
    let whole = bytes / (1024 * 1024);
    let frac = (bytes % (1024 * 1024)) * 100 / (1024 * 1024);
    format!("{whole}.{frac:02}")
}

#[must_use]
pub(super) fn format_bitrate_kbps(bitrate_bps: i64) -> String {
    let bps = bitrate_bps.unsigned_abs();
    let whole = bps / 1000;
    let frac = (bps % 1000) * 100 / 1000;
    format!("{whole}.{frac:02}")
}
