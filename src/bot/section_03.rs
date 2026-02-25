async fn handle_help_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let help_text = format!(
        "📖 <b>使用帮助</b>\n\n\
        1️⃣ <b>直接解析</b>\n\
        发送网易云音乐链接给机器人，例如：\n\
        <code>https://music.163.com/song?id=12345</code>\n\
        <code>https://music.163.com/playlist?id=12345</code>\n\
        <code>https://music.163.com/album?id=12345</code>\n\n\
        2️⃣ <b>搜索音乐</b>\n\
        使用 <code>/search &lt;关键词&gt;</code> 在私聊中搜索。\n\n\
        3️⃣ <b>Inline 搜索</b>\n\
        在任何对话框输入 <code>@{} &lt;关键词&gt;</code> 即可快速搜索并分享音乐。\n\n\
        4️⃣ <b>获取歌词</b>\n\
        使用 <code>/lyric &lt;关键词或ID&gt;</code> 获取歌词。\n\n\
        5️⃣ <b>更多命令</b>\n\
        • <code>/status</code> - 查看系统状态\n\
        • <code>/about</code> - 关于机器人\n\n\
        💬 <b>项目主页：</b> <a href=\"https://github.com/Lemonawa/music163bot-rust\">GitHub</a>",
        state.bot_username
    );

    bot.send_message(msg.chat.id, help_text)
        .parse_mode(ParseMode::Html)
        .disable_link_preview(true)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    Ok(())
}

async fn handle_music_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let args = args.unwrap_or_default();

    if args.is_empty() {
        send_reply_text(bot, msg, "请输入歌曲ID或歌曲关键词").await?;
        return Ok(());
    }

    // Try to parse as music ID first
    if let Some(music_id) = parse_music_id(&args) {
        return process_music(bot, msg, state, music_id).await;
    }

    if let Some(target) = parse_music_collection_target(&args) {
        return process_music_collection(bot, msg, state, target).await;
    }

    // If not a number, search for the song
    match state.music_api.search_songs(&args, 1).await {
        Ok(songs) => {
            if let Some(song) = songs.first() {
                process_music(bot, msg, state, song.id).await
            } else {
                send_reply_text(bot, msg, "未找到相关歌曲").await?;
                Ok(())
            }
        }
        Err(e) => {
            send_reply_text(bot, msg, format!("搜索失败: {e}")).await?;
            Ok(())
        }
    }
}

async fn try_send_cached_song(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
) -> ResponseResult<bool> {
    let music_id_i64 = music_id as i64;

    let Ok(Some(cached_song)) = state.database.get_song_by_music_id(music_id_i64).await else {
        return Ok(false);
    };

    let Some(file_id) = &cached_song.file_id else {
        return Ok(false);
    };

    if cached_song.music_size <= 1024 {
        tracing::warn!(
            "Removing invalid cached file for music_id {}: size {} bytes",
            music_id,
            cached_song.music_size
        );
        let _ = state.database.delete_song_by_music_id(music_id_i64).await;
        return Ok(false);
    }

    let bitrate = if cached_song.bit_rate > 0 {
        cached_song.bit_rate
    } else {
        let duration_sec = cached_song.duration.max(1) as f64;
        (8.0 * cached_song.music_size as f64 / duration_sec) as i64
    };

    let caption = build_caption(
        &cached_song.song_name,
        &cached_song.song_artists,
        &cached_song.song_album,
        &cached_song.file_ext,
        cached_song.music_size,
        bitrate,
        &state.bot_username,
    );

    let keyboard =
        create_music_keyboard(music_id, &cached_song.song_name, &cached_song.song_artists);

    match bot
        .send_audio(msg.chat.id, InputFile::file_id(FileId(file_id.clone())))
        .caption(caption)
        .reply_markup(keyboard)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("invalid remote file identifier") {
                tracing::warn!(
                    "Cached file_id invalid for music_id {}, deleting cache and re-downloading: {}",
                    music_id,
                    e
                );
                let _ = state.database.delete_song_by_music_id(music_id_i64).await;
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

async fn process_music(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
) -> ResponseResult<()> {
    let e2e_start = std::time::Instant::now();
    let mut perf_ctx = build_perf_trace_context(state, music_id, "initial");

    let cache_lookup_start = std::time::Instant::now();
    if try_send_cached_song(bot, msg, state, music_id).await? {
        perf_ctx = perf_ctx.with_cache_path("hit_pre_singleflight");
        perf_ctx.log_stage(PERF_STAGE_CACHE_LOOKUP, cache_lookup_start.elapsed());
        state.runtime_metrics.record_cache_hit();
        perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
        return Ok(());
    }
    perf_ctx.log_stage(PERF_STAGE_CACHE_LOOKUP, cache_lookup_start.elapsed());

    let singleflight_wait_start = std::time::Instant::now();
    let mut waited_for_existing_leader = false;
    let _singleflight_guard = loop {
        if let Some(leader_guard) =
            acquire_download_leader(&state.inflight_downloads, music_id).await
        {
            break leader_guard;
        }
        waited_for_existing_leader = true;

        if try_send_cached_song(bot, msg, state, music_id).await? {
            perf_ctx = perf_ctx.with_cache_path("hit_during_singleflight");
            state.runtime_metrics.record_cache_hit();
            perf_ctx.log_stage(
                PERF_STAGE_SINGLEFLIGHT_WAIT,
                singleflight_wait_start.elapsed(),
            );
            perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
            return Ok(());
        }
    };
    perf_ctx.log_stage(
        PERF_STAGE_SINGLEFLIGHT_WAIT,
        singleflight_wait_start.elapsed(),
    );

    if waited_for_existing_leader {
        let post_wait_cache_lookup_start = std::time::Instant::now();
        if try_send_cached_song(bot, msg, state, music_id).await? {
            perf_ctx = perf_ctx.with_cache_path("hit_post_singleflight");
            perf_ctx.log_stage(
                PERF_STAGE_CACHE_LOOKUP,
                post_wait_cache_lookup_start.elapsed(),
            );
            state.runtime_metrics.record_cache_hit();
            perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
            return Ok(());
        }
        perf_ctx.log_stage(
            PERF_STAGE_CACHE_LOOKUP,
            post_wait_cache_lookup_start.elapsed(),
        );
    }

    state.runtime_metrics.record_cache_miss();
    perf_ctx = perf_ctx.with_cache_path("miss_cold");

    // Send status message and fetch song detail+URL in parallel
    let status_init_start = std::time::Instant::now();
    let bitrate_candidates = url_bitrate_candidates(state.music_api.music_u.is_some());

    let status_fut = bot
        .send_message(msg.chat.id, "🔄 正在获取歌曲信息...")
        .reply_parameters(ReplyParameters::new(msg.id))
        .send();
    let fetch_fut = state
        .music_api
        .get_song_detail_and_best_url(music_id, bitrate_candidates);

    let (status_result, detail_and_url_result) = tokio::join!(status_fut, fetch_fut);
    let status_msg = status_result?;
    let select_url_duration = status_init_start.elapsed();
    log_perf(PERF_STAGE_SELECT_URL, select_url_duration);
    perf_ctx.log_stage(PERF_STAGE_SELECT_URL, select_url_duration);

    let (song_detail, song_url) = match detail_and_url_result {
        Ok(result) => result,
        Err(e) => {
            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                format!("❌ 获取歌曲信息或下载链接失败: {e}"),
            )
            .await?;
            perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
            return Ok(());
        }
    };

    if song_url.url.is_empty() {
        bot.edit_message_text(
            msg.chat.id,
            status_msg.id,
            "❌ 无法获取下载链接，可能需要VIP权限",
        )
        .await?;
        perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
        return Ok(());
    }

    let pre_upload_path_start = std::time::Instant::now();

    // Update status (fire-and-forget to overlap with download start)
    let artists = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
    {
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        let status_id = status_msg.id;
        let text = format!("📥 正在下载: {} - {}", song_detail.name, artists);
        tokio::spawn(async move {
            bot_clone
                .edit_message_text(chat_id, status_id, text)
                .await
                .ok();
        });
    }

    // Download and process the song
    match download_and_send_music(
        bot,
        msg,
        state,
        song_detail,
        &song_url,
        &status_msg,
        pre_upload_path_start,
        &perf_ctx,
        &artists,
    )
    .await
    {
        Ok(()) => {}
        Err(e) => {
            bot.edit_message_text(msg.chat.id, status_msg.id, format!("❌ 处理失败: {e}"))
                .await?;
        }
    }

    perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
    Ok(())
}

async fn process_music_collection(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    target: MusicCollectionTarget,
) -> ResponseResult<()> {
    let (collection_name, collection_id, song_ids_result) = match target {
        MusicCollectionTarget::Playlist(playlist_id) => (
            "歌单",
            playlist_id,
            state.music_api.get_playlist_song_ids(playlist_id).await,
        ),
        MusicCollectionTarget::Album(album_id) => (
            "专辑",
            album_id,
            state.music_api.get_album_song_ids(album_id).await,
        ),
    };

    let song_ids = match song_ids_result {
        Ok(song_ids) => song_ids,
        Err(e) => {
            send_reply_text(
                bot,
                msg,
                format!("❌ 获取{collection_name}歌曲列表失败: {e}"),
            )
            .await?;
            return Ok(());
        }
    };

    if song_ids.is_empty() {
        send_reply_text(bot, msg, format!("❌ 该{collection_name}中没有可下载歌曲")).await?;
        return Ok(());
    }

    let max_tracks = state.config.max_batch_download_tracks.max(1) as usize;
    if exceeds_batch_download_limit(song_ids.len(), state.config.max_batch_download_tracks) {
        send_reply_text(
            bot,
            msg,
            format!(
                "❌ 该{collection_name}包含 {} 首歌曲，超过单次下载上限 {} 首，已拒绝全部下载",
                song_ids.len(),
                max_tracks
            ),
        )
        .await?;
        return Ok(());
    }

    send_reply_text(
        bot,
        msg,
        format!(
            "📚 检测到{collection_name}（ID: {collection_id}），共 {} 首，开始下载",
            song_ids.len()
        ),
    )
    .await?;

    let mut failed_count = 0usize;
    for song_id in song_ids {
        if let Err(e) = process_music(bot, msg, state, song_id).await {
            failed_count += 1;
            tracing::error!(
                "Failed to process song {} from {} {}: {}",
                song_id,
                collection_name,
                collection_id,
                e
            );
        }
    }

    if failed_count > 0 {
        send_reply_text(
            bot,
            msg,
            format!("⚠️ {collection_name}下载完成，但有 {failed_count} 首歌曲处理失败"),
        )
        .await?;
    }

    Ok(())
}

async fn download_cover_assets(
    state: &Arc<BotState>,
    song_detail: &crate::music_api::SongDetail,
    song_id: u64,
    cover_mode: CoverMode,
    download_cover: bool,
    download_thumbnail: bool,
    perf_ctx: &PerfTraceContext,
) -> (Option<Bytes>, Option<ThumbnailBuffer>, bool) {
    let cover_download_start = std::time::Instant::now();
    let result = if let Some(ref al) = song_detail.al {
        tracing::debug!("Album info found: id={}, name={}", al.id, al.name);
        if let Some(ref pic_url) = al.pic_url {
            if pic_url.is_empty() {
                tracing::warn!("Album art URL is empty for music_id {}", song_id);
                (None, None, false)
            } else {
                tracing::debug!(
                    "Starting album art download for music_id {} (mode: {:?}), pic_url: {}",
                    song_id,
                    cover_mode,
                    pic_url
                );

                if download_cover {
                    match state.music_api.download_album_art_data(pic_url).await {
                        Ok(data) => {
                            tracing::debug!(
                                "Downloaded 320px album art for music_id {} ({} bytes)",
                                song_id,
                                data.len()
                            );

                            let data = Bytes::from(data);
                            let thumbnail_buffer = if download_thumbnail {
                                let thumb_filename =
                                    format!("thumb_{}_{}.jpg", song_id, chrono::Utc::now().timestamp());
                                ThumbnailBuffer::new(
                                    &state.config,
                                    data.clone(),
                                    &state.config.cache_dir,
                                    &thumb_filename,
                                )
                                .await
                                .ok()
                            } else {
                                None
                            };

                            (Some(data), thumbnail_buffer, false)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to download 320px album art for music_id {}: {}",
                                song_id,
                                e
                            );
                            (None, None, true)
                        }
                    }
                } else {
                    (None, None, false)
                }
            }
        } else {
            tracing::warn!("No pic_url found in album for music_id {}", song_id);
            (None, None, false)
        }
    } else {
        tracing::warn!("No album info found for music_id {}", song_id);
        (None, None, false)
    };
    perf_ctx.log_stage(PERF_STAGE_COVER_DOWNLOAD, cover_download_start.elapsed());
    result
}

