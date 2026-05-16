use super::{
    Arc, AudioBuffer, AudioFormat, Bot, BotState, Context, Message, MusicLinkTarget, Ordering,
    PERF_STAGE_DB_SAVE, PERF_STAGE_DOWNLOAD_AUDIO, PERF_STAGE_PRE_UPLOAD_PATH,
    PERF_STAGE_TAG_PROCESS, PERF_STAGE_UPLOAD_CLIENT_ACQUIRE, PERF_STAGE_UPLOAD_PERMIT_WAIT,
    PERF_STAGE_UPLOAD_SEND, PerfTraceContext, RawUploadParams, Result, SongInfo, StreamReader,
    acquire_download_permit, acquire_upload_client, acquire_upload_permit_owned,
    apply_tags_in_blocking, build_caption, clean_filename, cleanup_audio_buffer,
    cleanup_thumbnail_buffer, collect_maintenance_signals, cover_download_failure_notice,
    create_music_keyboard_for_target, delete_status_message_resilient, download_chunk_bytes,
    download_cover_assets, edit_status_message_resilient, extract_file_id_from_response, log_perf,
    raw_send_file, resolve_cover_policy, resource_availability_status, sanitize_sensitive_text,
    send_reply_text, should_download_cover, should_remove_song_cache_after_partial_failure,
    throughput_mbps, update_peak,
};
use futures_util::{StreamExt, TryStreamExt};

pub(super) async fn download_and_send_music(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    song_detail: Arc<crate::music_api::SongDetail>,
    song_url: &crate::music_api::SongUrl,
    status_msg: &Message,
    pre_upload_path_start: std::time::Instant,
    perf_ctx: &PerfTraceContext,
    artists: &str,
    link_target: MusicLinkTarget,
) -> Result<()> {
    let audio_format = if song_url.url.contains(".flac") {
        AudioFormat::Flac
    } else {
        AudioFormat::Mp3
    };
    let file_ext = audio_format.as_str();

    let filename = clean_filename(&format!(
        "{} - {}.{}",
        artists.replace('/', ","),
        song_detail.name,
        file_ext
    ));

    let cover_mode = state.config.cover_mode;
    let cover_policy = resolve_cover_policy(cover_mode);
    let download_thumbnail = cover_policy.download_thumbnail;
    let download_cover = should_download_cover(cover_policy);

    let song_id = song_detail.id;
    let song_name = song_detail.name.clone();
    let song_album = song_detail
        .al
        .as_ref()
        .map_or_else(|| "Unknown Album".to_string(), |al| al.name.clone());
    let duration_ms = song_detail.dt;

    let cover_perf_ctx = perf_ctx.clone();
    let artwork_future = download_cover_assets(
        state,
        song_detail.as_ref(),
        song_id,
        cover_mode,
        download_cover,
        download_thumbnail,
        &cover_perf_ctx,
    );

    let download_perf_ctx = perf_ctx.clone();
    let audio_future = async {
        let _download_permit = acquire_download_permit(&state.download_semaphore).await?;
        let download_start = std::time::Instant::now();
        let response = state.music_api.download_file(&song_url.url).await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP {}", response.status()));
        }

        let content_length = response.content_length();
        if content_length == Some(0) {
            return Err(anyhow::anyhow!("Empty file or unable to get file size"));
        }

        let mut audio_buffer = AudioBuffer::new(
            &state.config,
            content_length.unwrap_or(0),
            filename.clone(),
            &state.config.cache_dir,
        )
        .await?;

        let mut stream = response.bytes_stream();
        let max_download_size = if audio_buffer.is_memory() {
            state.config.memory_max_file_mb * 1024 * 1024
        } else {
            2000 * 1024 * 1024
        };

        let downloaded = if audio_buffer.is_disk() {
            let chunk_bytes = download_chunk_bytes(&state.config);
            let stream = stream.map_err(std::io::Error::other);
            let reader =
                tokio::io::BufReader::with_capacity(chunk_bytes, StreamReader::new(stream));
            let mut limited_reader = tokio::io::AsyncReadExt::take(reader, max_download_size + 1);
            let file = audio_buffer
                .disk_file_mut()
                .ok_or_else(|| anyhow::anyhow!("Disk buffer missing file handle"))?;
            let mut writer = tokio::io::BufWriter::with_capacity(chunk_bytes, file);
            let downloaded = tokio::io::copy_buf(&mut limited_reader, &mut writer)
                .await
                .context("Failed to stream download to disk")?;

            if downloaded > max_download_size {
                return Err(anyhow::anyhow!(
                    "Download exceeds maximum allowed size ({max_download_size} bytes)"
                ));
            }

            tokio::io::AsyncWriteExt::flush(&mut writer)
                .await
                .context("Failed to flush disk writer")?;
            downloaded
        } else {
            let mut downloaded = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                downloaded += chunk.len() as u64;

                if downloaded > max_download_size {
                    return Err(anyhow::anyhow!(
                        "Download exceeds maximum allowed size ({max_download_size} bytes)"
                    ));
                }

                audio_buffer.write_chunk(&chunk).await?;
            }
            downloaded
        };
        audio_buffer.finish().await?;
        let download_duration = download_start.elapsed();
        let download_mbps = throughput_mbps(downloaded, download_duration);
        state
            .runtime_metrics
            .record_download_speed(downloaded, download_duration);
        tracing::info!(
            "Audio download completed in {:.2}s ({:.2} MB/s)",
            download_duration.as_secs_f64(),
            download_mbps
        );
        log_perf(PERF_STAGE_DOWNLOAD_AUDIO, download_duration);
        download_perf_ctx.log_stage(PERF_STAGE_DOWNLOAD_AUDIO, download_duration);

        Ok::<(AudioBuffer, u64), anyhow::Error>((audio_buffer, downloaded))
    };

    let (downloaded_result, (cover_artwork_data, thumbnail_buffer, cover_retry_exhausted)) =
        tokio::join!(audio_future, artwork_future);
    let (mut audio_buffer, downloaded) = match downloaded_result {
        Ok(res) => res,
        Err(e) => {
            cleanup_thumbnail_buffer(thumbnail_buffer).await;
            return Err(e.into());
        }
    };

    let should_remove_song_cache =
        should_remove_song_cache_after_partial_failure(cover_retry_exhausted);

    if cover_retry_exhausted {
        tracing::warn!(
            "Cover fetch failed after retries for music_id {}. Audio will be sent without cover and cache will be removed",
            song_id
        );
    }

    tracing::debug!(
        "Audio download completed: {} bytes (mode: {})",
        downloaded,
        if audio_buffer.is_memory() {
            "memory"
        } else {
            "disk"
        }
    );
    let cover_status = resource_availability_status(download_cover, cover_artwork_data.is_some());
    let thumbnail_status =
        resource_availability_status(download_thumbnail, thumbnail_buffer.is_some());
    tracing::debug!(
        "Cover download result - Cover: {}, Thumbnail: {}",
        cover_status,
        thumbnail_status
    );

    if downloaded == 0 {
        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;
        edit_status_message_resilient(bot, msg.chat.id, status_msg.id, "下载失败: 文件为空").await;
        return Ok(());
    }

    if downloaded < 1024 {
        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;
        edit_status_message_resilient(
            bot,
            msg.chat.id,
            status_msg.id,
            format!("下载失败: 文件太小({downloaded} bytes)"),
        )
        .await;
        return Ok(());
    }

    tracing::debug!("File validation passed: {} bytes", downloaded);

    tracing::debug!("Processing tags for {} format", audio_format);
    let tag_perf_ctx = perf_ctx.clone();
    let tag_future = async {
        let tags_start = std::time::Instant::now();
        let result = apply_tags_in_blocking(
            audio_buffer,
            audio_format,
            song_detail,
            cover_artwork_data,
            cover_policy.embed_cover,
        )
        .await;
        let tags_duration = tags_start.elapsed();
        log_perf("process_tags", tags_duration);
        tag_perf_ctx.log_stage(PERF_STAGE_TAG_PROCESS, tags_duration);
        result
    };

    let upload_client_perf_ctx = perf_ctx.clone();
    let upload_client_future = async {
        let upload_client_start = std::time::Instant::now();
        let result = acquire_upload_client(state).await;
        upload_client_perf_ctx.log_stage(
            PERF_STAGE_UPLOAD_CLIENT_ACQUIRE,
            upload_client_start.elapsed(),
        );
        result
    };

    let (tag_result, upload_client_result) = tokio::join!(tag_future, upload_client_future);

    audio_buffer = match tag_result {
        Ok(buf) => buf,
        Err(e) => {
            cleanup_thumbnail_buffer(thumbnail_buffer).await;
            return Err(e);
        }
    };

    let (_upload_bot, raw_client, api_base_url) = match upload_client_result {
        Ok(res) => res,
        Err(e) => {
            cleanup_audio_buffer(audio_buffer).await;
            cleanup_thumbnail_buffer(thumbnail_buffer).await;
            return Err(e);
        }
    };

    let file_size = audio_buffer.size().await;
    let audio_file_size = file_size as i64;
    let duration_sec = (duration_ms.unwrap_or(0) / 1000) as i64;

    let actual_bitrate_bps = if duration_sec > 0 {
        (8 * audio_file_size) / duration_sec
    } else {
        song_url.br as i64
    };

    tracing::debug!(
        "Bitrate - API: {} bps, Calculated from file: {} bps (duration: {}s)",
        song_url.br,
        actual_bitrate_bps,
        duration_sec
    );

    let now = chrono::Utc::now();
    let program_id = match link_target {
        MusicLinkTarget::Program(program_id) => Some(program_id as i64),
        MusicLinkTarget::Song(_) => None,
    };
    let mut song_info = SongInfo {
        music_id: song_id as i64,
        program_id,
        song_name,
        song_artists: artists.to_string(),
        song_album,
        file_ext: file_ext.to_string(),
        music_size: audio_file_size,
        pic_size: 0,
        emb_pic_size: 0,
        bit_rate: actual_bitrate_bps,
        duration: duration_sec,
        file_id: None,
        thumb_file_id: None,
        from_user_id: msg.from.as_ref().map_or(0, |u| u.id),
        from_user_name: msg
            .from
            .as_ref()
            .and_then(|u| u.username.clone())
            .unwrap_or_default(),
        from_chat_id: msg.chat.id.0,
        from_chat_name: msg.chat.username.as_deref().unwrap_or("").to_string(),
        created_at: now,
        updated_at: now,
        ..Default::default()
    };

    tracing::debug!("Final thumbnail status: {}", thumbnail_status);

    let caption = build_caption(
        &song_info.song_name,
        &song_info.song_artists,
        &song_info.song_album,
        &song_info.file_ext,
        song_info.music_size,
        song_info.bit_rate,
        &state.bot_username,
    );

    let keyboard = create_music_keyboard_for_target(
        link_target,
        song_id,
        &song_info.song_name,
        &song_info.song_artists,
    );

    if file_size == 0 {
        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;
        return Err(anyhow::anyhow!("Audio file is empty after processing").into());
    }

    tracing::debug!(
        "Prepared audio: {} ({:.2} MB, mode: {})",
        audio_buffer.filename(),
        file_size as f64 / 1024.0 / 1024.0,
        if audio_buffer.is_memory() {
            "memory"
        } else {
            "disk"
        }
    );

    let pre_upload_path_duration = pre_upload_path_start.elapsed();
    log_perf(PERF_STAGE_PRE_UPLOAD_PATH, pre_upload_path_duration);
    perf_ctx.log_stage(PERF_STAGE_PRE_UPLOAD_PATH, pre_upload_path_duration);

    let upload_permit_wait_start = std::time::Instant::now();
    let _upload_permit =
        match acquire_upload_permit_owned(Arc::clone(&state.upload_semaphore)).await {
            Ok(permit) => permit,
            Err(e) => {
                cleanup_audio_buffer(audio_buffer).await;
                cleanup_thumbnail_buffer(thumbnail_buffer).await;
                return Err(e);
            }
        };
    perf_ctx.log_stage(
        PERF_STAGE_UPLOAD_PERMIT_WAIT,
        upload_permit_wait_start.elapsed(),
    );

    tracing::debug!(
        "Sending audio file: {} ({:.2} MB)",
        audio_buffer.filename(),
        file_size as f64 / 1024.0 / 1024.0
    );

    let is_flac = audio_format == AudioFormat::Flac;

    tracing::debug!("File format: {}", if is_flac { "FLAC" } else { "MP3" });

    let reply_markup_json = match serde_json::to_string(&keyboard) {
        Ok(json) => Some(json),
        Err(e) => {
            tracing::warn!("Failed to serialize reply keyboard: {}", e);
            None
        }
    };

    let audio_bytes = audio_buffer.take_memory_bytes_for_upload();

    let in_flight = state
        .upload_counters
        .in_flight
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    let peak_in_flight = update_peak(&state.upload_counters.peak_in_flight, in_flight);
    let upload_start = std::time::Instant::now();
    let params = RawUploadParams {
        chat_id: msg.chat.id.0,
        caption: &caption,
        reply_to_message_id: msg.id.0,
        reply_markup_json: reply_markup_json.clone(),
        title: Some(&song_info.song_name),
        performer: Some(&song_info.song_artists),
        duration: Some(song_info.duration as u32),
        thumbnail: thumbnail_buffer.as_ref(),
    };

    let upload_result = raw_send_file(
        &raw_client,
        &api_base_url,
        &state.config,
        state.is_official_api,
        &audio_buffer,
        audio_bytes.as_ref(),
        file_size,
        &params,
    )
    .await;

    let upload_duration = upload_start.elapsed();
    let in_flight_after = state
        .upload_counters
        .in_flight
        .fetch_sub(1, Ordering::Relaxed)
        - 1;
    log_perf("upload_audio", upload_duration);
    perf_ctx.log_stage(PERF_STAGE_UPLOAD_SEND, upload_duration);

    if let Ok(ref resp_json) = upload_result {
        let upload_mbps = throughput_mbps(file_size, upload_duration);
        state
            .runtime_metrics
            .record_upload_speed(file_size, upload_duration);
        tracing::info!(
            "Upload completed in {:.2}s ({:.2} MB/s, inflight: {}, peak: {})",
            upload_duration.as_secs_f64(),
            upload_mbps,
            in_flight_after,
            peak_in_flight
        );
        tracing::info!(
            "Successfully sent as audio: {}",
            if is_flac { "FLAC" } else { "MP3" }
        );

        if let Some(file_id) = extract_file_id_from_response(resp_json) {
            song_info.file_id = Some(file_id);
        }
    } else if let Err(e) = upload_result {
        let upload_mbps = throughput_mbps(file_size, upload_duration);
        tracing::warn!(
            "Upload failed after {:.2}s ({:.2} MB/s, inflight: {}, peak: {})",
            upload_duration.as_secs_f64(),
            upload_mbps,
            in_flight_after,
            peak_in_flight
        );
        tracing::warn!(
            "Upload failed: {}",
            sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
        );

        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;

        return Err(e);
    }

    cleanup_audio_buffer(audio_buffer).await;
    cleanup_thumbnail_buffer(thumbnail_buffer).await;

    let db_save_start = std::time::Instant::now();
    let db_save_result = if should_remove_song_cache {
        if let Err(e) = state.database.delete_song_by_music_id(song_id as i64).await {
            tracing::warn!(
                "Failed to remove partial cache for music_id {} after upload: {}",
                song_id,
                e
            );
        }
        Ok(())
    } else {
        match state.database.save_song_info(&song_info).await {
            Ok(_) => {
                for signal in
                    collect_maintenance_signals(&state.maintenance_counters, &state.config)
                {
                    match state.maintenance_tx.try_send(signal) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            tracing::debug!("Maintenance queue full; dropping signal {:?}", signal);
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            tracing::warn!("Maintenance worker unavailable; skipping signal");
                        }
                    }
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    };
    perf_ctx.log_stage(PERF_STAGE_DB_SAVE, db_save_start.elapsed());
    db_save_result?;

    if cover_retry_exhausted {
        let notice = cover_download_failure_notice();
        if let Err(e) = send_reply_text(bot, msg, notice).await {
            tracing::warn!(
                "Failed to send cover fallback notice for music_id {}: {}",
                song_id,
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
        }
    }

    delete_status_message_resilient(bot, msg.chat.id, status_msg.id).await;

    Ok(())
}
