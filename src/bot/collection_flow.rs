use super::{extract_retry_after_seconds, Bot, Message, Arc, BotState, MusicCollectionTarget, ResponseResult, sanitize_sensitive_text, send_reply_text, exceeds_batch_download_limit, process_music, process_music_with_context, CoverMode, PerfTraceContext, Bytes, ThumbnailBuffer, PERF_STAGE_COVER_DOWNLOAD};

pub(super) fn collection_retry_delay_seconds(
    error: &impl std::fmt::Display,
    attempt: u32,
) -> Option<u64> {
    if attempt > 0 {
        return None;
    }

    extract_retry_after_seconds(&error.to_string()).map(|seconds| seconds.saturating_add(1))
}

pub(super) async fn process_music_collection(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    target: MusicCollectionTarget,
) -> ResponseResult<()> {
    if let MusicCollectionTarget::DjRadio(radio_id) = target {
        return process_djradio_collection(bot, msg, state, radio_id).await;
    }

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
        MusicCollectionTarget::DjRadio(_) => unreachable!("djradio handled above"),
    };

    let song_ids = match song_ids_result {
        Ok(song_ids) => song_ids,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch {collection_name} songs: {}",
                sanitize_sensitive_text(&e.to_string())
            );
            send_reply_text(
                bot,
                msg,
                format!("❌ 获取{collection_name}歌曲列表失败，请稍后重试"),
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
        let mut attempt = 0u32;
        loop {
            match process_music(bot, msg, state, song_id).await {
                Ok(()) => break,
                Err(e) => {
                    if let Some(delay_secs) = collection_retry_delay_seconds(&e, attempt) {
                        attempt = attempt.saturating_add(1);
                        tracing::warn!(
                            "Rate limited while processing song {} from {} {}. Waiting {}s before retry",
                            song_id,
                            collection_name,
                            collection_id,
                            delay_secs
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        continue;
                    }

                    failed_count += 1;
                    tracing::error!(
                        "Failed to process song {} from {} {}: {}",
                        song_id,
                        collection_name,
                        collection_id,
                        sanitize_sensitive_text(&e.to_string())
                    );
                    break;
                }
            }
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

pub(super) async fn process_djradio_collection(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    radio_id: u64,
) -> ResponseResult<()> {
    let max_tracks = state.config.max_batch_download_tracks.max(1) as usize;
    let fetch_limit = max_tracks.saturating_add(1);
    let (total_programs, program_tracks) = match state
        .music_api
        .get_djradio_program_main_tracks(radio_id, fetch_limit)
        .await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch djradio program list for {radio_id}: {}",
                sanitize_sensitive_text(&e.to_string())
            );
            send_reply_text(bot, msg, "❌ 获取播客声音列表失败，请稍后重试").await?;
            return Ok(());
        }
    };

    if total_programs == 0 || program_tracks.is_empty() {
        send_reply_text(bot, msg, "❌ 该播客中没有可下载声音").await?;
        return Ok(());
    }

    if exceeds_batch_download_limit(total_programs, state.config.max_batch_download_tracks) {
        send_reply_text(
            bot,
            msg,
            format!(
                "❌ 该播客包含 {total_programs} 条声音，超过单次下载上限 {max_tracks} 条，已拒绝全部下载",
            ),
        )
        .await?;
        return Ok(());
    }

    let mut seen_track_ids = std::collections::HashSet::new();
    let mut unique_tracks = Vec::with_capacity(program_tracks.len());
    for program in program_tracks {
        if seen_track_ids.insert(program.main_track_id) {
            unique_tracks.push(program);
        }
    }

    if unique_tracks.is_empty() {
        send_reply_text(bot, msg, "❌ 该播客中没有可下载声音").await?;
        return Ok(());
    }

    send_reply_text(
        bot,
        msg,
        format!(
            "📻 检测到播客（ID: {radio_id}），共 {} 条声音，开始下载",
            unique_tracks.len()
        ),
    )
    .await?;

    let mut failed_count = 0usize;
    for program in unique_tracks {
        let program_id = program.program_id;
        let main_track_id = program.main_track_id;
        let mut attempt = 0u32;
        loop {
            match process_music_with_context(bot, msg, state, main_track_id, Some(program.clone()))
                .await
            {
                Ok(()) => break,
                Err(e) => {
                    if let Some(delay_secs) = collection_retry_delay_seconds(&e, attempt) {
                        attempt = attempt.saturating_add(1);
                        tracing::warn!(
                            "Rate limited while processing program {} from radio {}. Waiting {}s before retry",
                            program_id,
                            radio_id,
                            delay_secs
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        continue;
                    }

                    failed_count += 1;
                    tracing::error!(
                        "Failed to process program {} from radio {}: {}",
                        program_id,
                        radio_id,
                        e
                    );
                    break;
                }
            }
        }
    }

    if failed_count > 0 {
        send_reply_text(
            bot,
            msg,
            format!("⚠️ 播客下载完成，但有 {failed_count} 条声音处理失败"),
        )
        .await?;
    }

    Ok(())
}

pub(super) async fn download_cover_assets(
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
                    let resize = !matches!(cover_mode, CoverMode::Original | CoverMode::Both);
                    match state
                        .music_api
                        .download_album_art_data(pic_url, resize)
                        .await
                    {
                        Err(e) => {
                            tracing::warn!(
                                "Failed to download album art for music_id {} after {} attempts: {}",
                                song_id,
                                crate::music_api::ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS,
                                e
                            );
                            (None, None, true)
                        }
                        Ok(data) => {
                            tracing::debug!(
                                "Downloaded album art for music_id {} ({} bytes, resize: {})",
                                song_id,
                                data.len(),
                                resize
                            );

                            let thumbnail_buffer = if download_thumbnail {
                                let thumb_data_bytes = if resize {
                                    Bytes::from(data.clone())
                                } else {
                                    let raw_data = data.clone();
                                    if let Ok(Ok(resized)) =
                                        tokio::task::spawn_blocking(move || {
                                            crate::music_api::resize_album_art_to_thumbnail(
                                                &raw_data,
                                            )
                                        })
                                        .await
                                    {
                                        Bytes::from(resized)
                                    } else {
                                        tracing::warn!(
                                            "Failed to resize album art to thumbnail for Both mode"
                                        );
                                        Bytes::from(data.clone())
                                    }
                                };

                                let thumb_filename = format!(
                                    "thumb_{}_{}.jpg",
                                    song_id,
                                    chrono::Utc::now().timestamp()
                                );
                                ThumbnailBuffer::new(
                                    &state.config,
                                    thumb_data_bytes,
                                    &state.config.cache_dir,
                                    &thumb_filename,
                                )
                                .await
                                .ok()
                            } else {
                                None
                            };

                            let embed_data = Bytes::from(data);
                            (Some(embed_data), thumbnail_buffer, false)
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

pub(super) fn cover_download_failure_notice() -> String {
    format!(
        "⚠️ 封面下载连续失败 {} 次，已发送无封面版本",
        crate::music_api::ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS
    )
}
