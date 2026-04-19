use super::*;

pub(super) async fn handle_lyric_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let Some(args) = require_command_args_or_reply(bot, msg, args, "请输入歌曲ID或关键词").await?
    else {
        return Ok(());
    };

    let Some(music_id) =
        parse_song_id_or_search_first_result(bot, msg, state, &args, "Lyric search failed").await?
    else {
        return Ok(());
    };

    let status_msg = send_reply_message(bot, msg, "🎵 正在获取歌词...").await?;

    match join_futures(
        state.music_api.get_song_lyric(music_id),
        state.music_api.get_song_detail(music_id),
    )
    .await
    {
        (Ok(lyric), detail_result) => {
            if lyric.trim().is_empty() || lyric == "No lyrics available" {
                bot.edit_message_text(msg.chat.id, status_msg.id, "该歌曲暂无歌词")
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
                        "获取歌曲信息失败，请稍后重试",
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
                        sanitize_sensitive_text(&e.to_string())
                    );
                    bot.edit_message_text(
                        msg.chat.id,
                        status_msg.id,
                        "初始化上传客户端失败，请稍后重试",
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
                        sanitize_sensitive_text(&e.to_string())
                    );
                    bot.edit_message_text(
                        msg.chat.id,
                        status_msg.id,
                        "等待上传通道失败，请稍后重试",
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
                            sanitize_sensitive_text(&e.to_string())
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to upload lyric file: {}",
                        sanitize_sensitive_text(&e.to_string())
                    );
                    bot.edit_message_text(msg.chat.id, status_msg.id, "发送歌词失败，请稍后重试")
                        .await?;
                }
            }
        }
        (Err(e), _) => {
            tracing::warn!(
                "Failed to fetch lyric: {}",
                sanitize_sensitive_text(&e.to_string())
            );
            bot.edit_message_text(msg.chat.id, status_msg.id, "获取歌词失败，请稍后重试")
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
    if !ensure_admin(bot, msg, &state.config).await? {
        return Ok(());
    }

    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);
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
    let download_line = format_speed_line("下载", download_speed);
    let upload_line = format_speed_line("上传", upload_speed);
    let status_text = build_status_text(
        total_count,
        user_count,
        chat_count,
        cache_snapshot,
        resource_snapshot,
        &uptime,
        &download_line,
        &upload_line,
    );

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
    let Some(_user_id) = authorize_admin_command(bot, msg, &state.config, "rmcache").await? else {
        return Ok(());
    };

    let args = args.unwrap_or_default();

    if args.is_empty() {
        send_reply_html(bot, msg, rmcache_usage_prompt()).await?;
        return Ok(());
    }

    if let Some(music_id) = parse_music_id(&args) {
        let music_id_i64 = music_id as i64;

        if let Ok(Some(song_info)) = state.database.get_song_by_music_id(music_id_i64).await {
            match state.database.delete_song_by_music_id(music_id_i64).await {
                Ok(deleted) => {
                    if deleted {
                        send_reply_text(
                            bot,
                            msg,
                            format!("✅ 已删除歌曲缓存: {}", song_info.song_name),
                        )
                        .await?;
                    } else {
                        send_reply_text(bot, msg, "歌曲未缓存").await?;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to delete cached song {music_id}: {}",
                        sanitize_sensitive_text(&e.to_string())
                    );
                    send_reply_text(bot, msg, "删除缓存失败，请稍后重试").await?;
                }
            }
        } else {
            send_reply_text(bot, msg, "歌曲未缓存").await?;
        }
    } else {
        send_reply_text(bot, msg, "无效的歌曲ID").await?;
    }

    Ok(())
}

pub(super) async fn handle_clearallcache_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let Some(_user_id) = authorize_admin_command(bot, msg, &state.config, "clearallcache").await?
    else {
        return Ok(());
    };

    send_reply_html(bot, msg, clearallcache_confirmation_prompt()).await?;

    Ok(())
}

pub(super) async fn handle_clearallcache_confirm_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let Some(user_id) =
        authorize_admin_command(bot, msg, &state.config, "clearallcache confirm").await?
    else {
        return Ok(());
    };

    let status_msg = send_reply_message(bot, msg, "🗑️ 正在清除所有缓存...").await?;

    match state.database.clear_all_songs().await {
        Ok(count) => {
            if let Err(e) = state.database.optimize().await {
                tracing::warn!("Database optimization failed after clear: {}", e);
            }

            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                format!("✅ 成功清除所有缓存！\n\n删除了 {count} 条记录"),
            )
            .await?;

            tracing::info!(
                "Admin {} cleared all cache, {} records deleted",
                user_id,
                count
            );
        }
        Err(e) => {
            bot.edit_message_text(msg.chat.id, status_msg.id, "❌ 清除缓存失败，请稍后重试")
                .await?;

            tracing::error!(
                "Failed to clear all cache: {}",
                sanitize_sensitive_text(&e.to_string())
            );
        }
    }

    Ok(())
}

pub(super) async fn ensure_admin_user_id(
    bot: &Bot,
    msg: &Message,
    config: &Config,
) -> ResponseResult<Option<i64>> {
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);
    if ensure_admin(bot, msg, config).await? {
        Ok(Some(user_id))
    } else {
        Ok(None)
    }
}

pub(super) async fn authorize_admin_command(
    bot: &Bot,
    msg: &Message,
    config: &Config,
    command_name: &str,
) -> ResponseResult<Option<i64>> {
    let Some(user_id) = ensure_admin_user_id(bot, msg, config).await? else {
        return Ok(None);
    };

    tracing::info!(
        "{} command from user_id: {}, configured admins: {:?}",
        command_name,
        user_id,
        config.bot_admin
    );

    Ok(Some(user_id))
}

pub(super) async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    state: Arc<BotState>,
) -> ResponseResult<()> {
    if let Some(data) = query.data
        && let Some((cmd, rest)) = data.split_once(' ')
        && cmd == "music"
        && let Ok(music_id) = rest.trim_start().parse::<u64>()
        && let Some(MaybeInaccessibleMessage::Regular(msg)) = &query.message
    {
        match process_music(&bot, msg, &state, music_id).await {
            Ok(()) => {
                bot.answer_callback_query(query.id)
                    .text("✅ 开始下载")
                    .await?;
            }
            Err(e) => {
                tracing::error!(
                    "Error processing music from callback: {}",
                    sanitize_sensitive_text(&e.to_string())
                );
                bot.answer_callback_query(query.id)
                    .text("❌ 处理失败，请稍后重试")
                    .await?;
            }
        }
        return Ok(());
    }

    bot.answer_callback_query(query.id)
        .text("❌ 无效的操作")
        .await?;

    Ok(())
}

pub(super) async fn handle_inline_query(
    bot: Bot,
    query: InlineQuery,
    state: Arc<BotState>,
) -> ResponseResult<()> {
    // Support "search" prefix for consistency with Go version
    let (search_keyword, is_search_cmd) = parse_inline_query_keyword(&query.query);

    if search_keyword.is_empty() {
        if is_search_cmd {
            let help_article = InlineQueryResultArticle::new(
                "search_help",
                "请输入关键词",
                InputMessageContent::Text(InputMessageContentText::new(format!(
                    "使用方法：在 @{} 后面输入 search 关键词 搜索音乐",
                    state.bot_username
                ))),
            )
            .description("输入关键词开始搜索");

            bot.answer_inline_query(query.id, vec![InlineQueryResult::Article(help_article)])
                .await?;
        } else {
            let help_article = InlineQueryResultArticle::new(
                "usage_help",
                "如何使用此机器人？",
                InputMessageContent::Text(InputMessageContentText::new(
                    "使用方法：\n1. 直接输入关键词搜索音乐\n2. 输入 search 关键词 搜索音乐\n3. 粘贴网易云音乐链接\n4. 输入歌曲 ID".to_string()
                )),
             )
            .description("在输入框中输入关键词开始搜索音乐");

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
                sanitize_sensitive_text(&e.to_string())
            );
            let error_article = InlineQueryResultArticle::new(
                "search_error",
                "搜索失败",
                InputMessageContent::Text(InputMessageContentText::new(
                    "搜索失败，请稍后重试".to_string(),
                )),
            )
            .description("搜索失败，请稍后重试");

            bot.answer_inline_query(query.id, vec![InlineQueryResult::Article(error_article)])
                .await?;
        }
    }

    Ok(())
}

/// Build caption with exact format:
/// 「Title」- Artists
/// 专辑: Album
/// #网易云音乐 #ext {sizeMB}MB {kbps}kbps
/// via @`BotName`
pub(super) fn build_caption(
    title: &str,
    artists: &str,
    album: &str,
    file_ext: &str,
    size_bytes: i64,
    bitrate_bps: i64,
    bot_username: &str,
) -> String {
    let size_mb = (size_bytes as f64) / 1024.0 / 1024.0;
    let kbps = format_bitrate_kbps(bitrate_bps);
    format!(
        "「{title}」- {artists}\n专辑: {album}\n#网易云音乐 #{file_ext} {size_mb:.2}MB {kbps}kbps\nvia @{bot_username}",
    )
}

#[must_use]
pub(super) fn format_bitrate_kbps(bitrate_bps: i64) -> String {
    let bitrate_bps = bitrate_bps.max(0) as f64;
    format!("{:.2}", bitrate_bps / 1000.0)
}
