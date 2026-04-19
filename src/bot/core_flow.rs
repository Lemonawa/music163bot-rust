use super::*;

pub(super) async fn try_send_cached_song(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
    preferred_program_id: Option<u64>,
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

    let preferred_program_id = preferred_program_id.and_then(|id| i64::try_from(id).ok());
    let link_target =
        cached_music_link_target(preferred_program_id.or(cached_song.program_id), music_id);
    let keyboard = create_music_keyboard_for_target(
        link_target,
        music_id,
        &cached_song.song_name,
        &cached_song.song_artists,
    );

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

pub(super) async fn process_program(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    program_id: u64,
) -> ResponseResult<()> {
    let program = match state.music_api.get_program_main_track(program_id).await {
        Ok(program) => program,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch program detail for {program_id}: {}",
                sanitize_sensitive_text(&e.to_string())
            );
            send_reply_text(bot, msg, "❌ 获取声音详情失败，请稍后重试").await?;
            return Ok(());
        }
    };

    process_music_with_context(bot, msg, state, program.main_track_id, Some(program)).await
}

pub(super) fn apply_program_metadata(
    song_detail: Arc<crate::music_api::SongDetail>,
    program: &ProgramMainTrack,
) -> Arc<crate::music_api::SongDetail> {
    let mut detail = (*song_detail).clone();

    if !program.program_name.trim().is_empty() {
        detail.name.clone_from(&program.program_name);
    }

    let author_name = if program.author_name.trim().is_empty() {
        detail
            .ar
            .as_ref()
            .and_then(|artists| artists.first())
            .and_then(|artist| {
                let name = artist.name.trim();
                if name.is_empty() {
                    None
                } else {
                    Some(artist.name.clone())
                }
            })
            .unwrap_or_else(|| "Unknown Artist".to_string())
    } else {
        program.author_name.clone()
    };
    detail.ar = Some(vec![crate::music_api::Artist {
        id: 0,
        name: author_name,
    }]);

    let previous_album = detail.al.take();
    let fallback_cover = previous_album
        .as_ref()
        .and_then(|album| album.pic_url.clone())
        .filter(|url| !url.is_empty());
    let cover_url = program
        .cover_url
        .clone()
        .filter(|url| !url.is_empty())
        .or(fallback_cover);
    let album_name = if program.radio_name.trim().is_empty() {
        previous_album
            .as_ref()
            .and_then(|album| {
                let name = album.name.trim();
                if name.is_empty() {
                    None
                } else {
                    Some(album.name.clone())
                }
            })
            .unwrap_or_else(|| "Unknown Album".to_string())
    } else {
        program.radio_name.clone()
    };
    let album_id = previous_album.as_ref().map_or(0, |album| album.id);
    detail.al = Some(crate::music_api::Album {
        id: album_id,
        name: album_name,
        pic_url: cover_url,
    });

    if detail.name.trim().is_empty() {
        detail.name = format!("声音 {}", program.program_id);
    }

    Arc::new(detail)
}

pub(super) async fn process_music(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
) -> ResponseResult<()> {
    process_music_with_context(bot, msg, state, music_id, None).await
}

pub(super) async fn process_music_with_context(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
    program_context: Option<ProgramMainTrack>,
) -> ResponseResult<()> {
    let e2e_start = std::time::Instant::now();
    let mut perf_ctx = build_perf_trace_context(state, music_id, "initial");
    let preferred_program_id = program_context.as_ref().map(|program| program.program_id);
    let media_label = if program_context.is_some() {
        "声音"
    } else {
        "歌曲"
    };
    let loading_text = if program_context.is_some() {
        "🔄 正在获取声音信息..."
    } else {
        "🔄 正在获取歌曲信息..."
    };

    let cache_lookup_start = std::time::Instant::now();
    if try_send_cached_song(bot, msg, state, music_id, preferred_program_id).await? {
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

        if try_send_cached_song(bot, msg, state, music_id, preferred_program_id).await? {
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
        if try_send_cached_song(bot, msg, state, music_id, preferred_program_id).await? {
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

    let status_init_start = std::time::Instant::now();
    let bitrate_candidates = url_bitrate_candidates(state.music_api.music_u.is_some());

    let status_fut = bot
        .send_message(msg.chat.id, loading_text)
        .reply_parameters(ReplyParameters::new(msg.id))
        .send();
    let fetch_fut = state
        .music_api
        .get_song_detail_and_best_url(music_id, bitrate_candidates);

    let (status_result, detail_and_url_result) = tokio::join!(status_fut, fetch_fut);
    let status_msg = match status_result {
        Ok(status_msg) => status_msg,
        Err(e) => {
            let sanitized = sanitize_sensitive_text(&e.to_string());
            if let Some(delay_secs) = extract_retry_after_seconds(&sanitized) {
                let retry_delay_secs = delay_secs.saturating_add(1);
                tracing::warn!(
                    "Status message rate limited for music_id {}. Waiting {}s before retry",
                    music_id,
                    retry_delay_secs
                );
                tokio::time::sleep(std::time::Duration::from_secs(retry_delay_secs)).await;
                bot.send_message(msg.chat.id, loading_text)
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .send()
                    .await?
            } else {
                return Err(e);
            }
        }
    };
    let select_url_duration = status_init_start.elapsed();
    log_perf(PERF_STAGE_SELECT_URL, select_url_duration);
    perf_ctx.log_stage(PERF_STAGE_SELECT_URL, select_url_duration);

    let (song_detail, song_url) = match detail_and_url_result {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch {media_label} detail/url for {music_id}: {}",
                sanitize_sensitive_text(&e.to_string())
            );
            edit_status_message_resilient(
                bot,
                msg.chat.id,
                status_msg.id,
                format!("❌ 获取{media_label}信息或下载链接失败，请稍后重试"),
            )
            .await;
            perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
            return Ok(());
        }
    };

    let (song_detail, link_target) = if let Some(program) = program_context.as_ref() {
        (
            apply_program_metadata(song_detail, program),
            MusicLinkTarget::Program(program.program_id),
        )
    } else {
        (song_detail, MusicLinkTarget::Song(music_id))
    };

    if song_url.url.is_empty() {
        edit_status_message_resilient(
            bot,
            msg.chat.id,
            status_msg.id,
            "❌ 无法获取下载链接，可能需要VIP权限",
        )
        .await;
        perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
        return Ok(());
    }

    let artists = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
    {
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        let status_id = status_msg.id;
        let text = format!("📥 正在下载: {} - {}", song_detail.name, artists);
        tokio::spawn(async move {
            edit_status_message_resilient(&bot_clone, chat_id, status_id, text).await;
        });
    }

    let mut process_attempt = 0u32;
    loop {
        let pre_upload_path_start = std::time::Instant::now();
        match download_and_send_music(
            bot,
            msg,
            state,
            Arc::clone(&song_detail),
            &song_url,
            &status_msg,
            pre_upload_path_start,
            &perf_ctx,
            &artists,
            link_target,
        )
        .await
        {
            Ok(()) => break,
            Err(e) => {
                let sanitized = sanitize_sensitive_text(&e.to_string());
                if process_attempt == 0
                    && let Some(delay_secs) = extract_retry_after_seconds(&sanitized)
                {
                    let retry_delay_secs = delay_secs.saturating_add(1);
                    process_attempt = process_attempt.saturating_add(1);
                    tracing::warn!(
                        "Upload rate limited for music_id {}. Waiting {}s before retry",
                        music_id,
                        retry_delay_secs
                    );
                    edit_status_message_resilient(
                        bot,
                        msg.chat.id,
                        status_msg.id,
                        format!("⚠️ Telegram 限流，等待 {retry_delay_secs} 秒后重试"),
                    )
                    .await;
                    tokio::time::sleep(std::time::Duration::from_secs(retry_delay_secs)).await;
                    edit_status_message_resilient(
                        bot,
                        msg.chat.id,
                        status_msg.id,
                        format!("📥 正在下载: {} - {}", song_detail.name, artists),
                    )
                    .await;
                    continue;
                }

                tracing::warn!("Failed to process music {music_id}: {}", sanitized);
                if extract_retry_after_seconds(&sanitized).is_some() {
                    delete_status_message_resilient(bot, msg.chat.id, status_msg.id).await;
                } else {
                    edit_status_message_resilient(
                        bot,
                        msg.chat.id,
                        status_msg.id,
                        "❌ 处理失败，请稍后重试",
                    )
                    .await;
                }
                break;
            }
        }
    }

    perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
    Ok(())
}
