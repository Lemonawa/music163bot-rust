fn build_about_text() -> String {
    format!(
        r"🎵 Music163bot-Rust v{} ({})

一个用来下载/分享/搜索网易云歌曲的 Telegram Bot

特性：
• 🔗 分享链接嗅探
• 🎵 歌曲搜索与下载
• 💾 智能缓存系统
• 🚀 智能存储 (v1.1.0+)
• 🎤 歌词获取
• 📊 使用统计

技术栈：
• 🦀 Rust + Teloxide
• 🔧 高并发处理
• 📦 轻量级部署

源码：GitHub | 原版：Music163bot-Go",
        env!("CARGO_PKG_VERSION"),
        BUILD_GIT_COMMIT
    )
}

async fn handle_about_command(
    bot: &Bot,
    msg: &Message,
    _state: &Arc<BotState>,
) -> ResponseResult<()> {
    let about_text = build_about_text();

    bot.send_message(msg.chat.id, about_text)
        .reply_parameters(ReplyParameters::new(msg.id))
        .disable_link_preview(true)
        .await?;

    Ok(())
}

async fn handle_lyric_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let args = args.unwrap_or_default();

    if args.is_empty() {
        send_reply_text(bot, msg, "请输入歌曲ID或关键词").await?;
        return Ok(());
    }

    let music_id = if let Some(id) = parse_music_id(&args) {
        id
    } else {
        match state.music_api.search_songs(&args, 1).await {
            Ok(songs) => {
                if let Some(song) = songs.first() {
                    song.id
                } else {
                    send_reply_text(bot, msg, "未找到相关歌曲").await?;
                    return Ok(());
                }
            }
            Err(e) => {
                send_reply_text(bot, msg, format!("搜索失败: {e}")).await?;
                return Ok(());
            }
        }
    };

    let status_msg = bot
        .send_message(msg.chat.id, "🎵 正在获取歌词...")
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

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

            // Get song detail for filename
            let song_detail = match detail_result {
                Ok(detail) => detail,
                Err(e) => {
                    bot.edit_message_text(
                        msg.chat.id,
                        status_msg.id,
                        format!("获取歌曲信息失败: {e}"),
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
                    bot.edit_message_text(
                        msg.chat.id,
                        status_msg.id,
                        format!("初始化上传客户端失败: {e}"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let _upload_permit = match permit_result {
                Ok(permit) => permit,
                Err(e) => {
                    bot.edit_message_text(
                        msg.chat.id,
                        status_msg.id,
                        format!("等待上传通道失败: {e}"),
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
                    bot.delete_message(msg.chat.id, status_msg.id).await.ok();
                }
                Err(e) => {
                    bot.edit_message_text(msg.chat.id, status_msg.id, format!("发送歌词失败: {e}"))
                        .await?;
                }
            }
        }
        (Err(e), _) => {
            bot.edit_message_text(msg.chat.id, status_msg.id, format!("获取歌词失败: {e}"))
                .await?;
        }
    }

    Ok(())
}

async fn handle_status_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
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

async fn handle_rmcache_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    // Check if user is admin
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);

    tracing::info!(
        "rmcache command from user_id: {}, configured admins: {:?}",
        user_id,
        state.config.bot_admin
    );

    if !ensure_admin(bot, msg, &state.config).await? {
        return Ok(());
    }

    let args = args.unwrap_or_default();

    if args.is_empty() {
        send_reply_html(bot, msg, rmcache_usage_prompt()).await?;
        return Ok(());
    }

    if let Some(music_id) = parse_music_id(&args) {
        let music_id_i64 = music_id as i64;

        // Get song info before deletion
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
                    send_reply_text(bot, msg, format!("删除缓存失败: {e}")).await?;
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

async fn handle_clearallcache_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    // Check if user is admin
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);

    tracing::info!(
        "clearallcache command from user_id: {}, configured admins: {:?}",
        user_id,
        state.config.bot_admin
    );

    if !ensure_admin(bot, msg, &state.config).await? {
        return Ok(());
    }

    // Send confirmation message
    send_reply_html(bot, msg, clearallcache_confirmation_prompt()).await?;

    Ok(())
}

async fn handle_clearallcache_confirm_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    // Check if user is admin
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);

    if !ensure_admin(bot, msg, &state.config).await? {
        return Ok(());
    }

    let status_msg = bot
        .send_message(msg.chat.id, "🗑️ 正在清除所有缓存...")
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    match state.database.clear_all_songs().await {
        Ok(count) => {
            // Optimize database after bulk deletion
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
            bot.edit_message_text(msg.chat.id, status_msg.id, format!("❌ 清除缓存失败: {e}"))
                .await?;

            tracing::error!("Failed to clear all cache: {}", e);
        }
    }

    Ok(())
}

async fn handle_callback(
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
                tracing::error!("Error processing music from callback: {}", e);
                bot.answer_callback_query(query.id)
                    .text(format!("❌ 失败: {e}"))
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

async fn handle_inline_query(
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
            tracing::error!("Inline search error: {}", e);
            let error_article = InlineQueryResultArticle::new(
                "search_error",
                "搜索失败",
                InputMessageContent::Text(InputMessageContentText::new(format!("搜索失败: {e}"))),
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
fn build_caption(
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
fn format_bitrate_kbps(bitrate_bps: i64) -> String {
    let bitrate_bps = bitrate_bps.max(0) as f64;
    format!("{:.2}", bitrate_bps / 1000.0)
}
