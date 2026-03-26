use super::*;

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
    // Determine file extension
    let file_ext = if song_url.url.contains(".flac") {
        "flac"
    } else {
        "mp3"
    };

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

    // Extract fields needed for SongInfo before moving song_detail into blocking task
    let song_id = song_detail.id;
    let song_name = song_detail.name.clone();
    let song_album = song_detail
        .al
        .as_ref()
        .map_or_else(|| "Unknown Album".to_string(), |al| al.name.clone());
    let duration_ms = song_detail.dt;

    // Start parallel downloads: audio file and album art
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

    // Download audio file using smart storage
    let download_perf_ctx = perf_ctx.clone();
    let audio_future = async {
        let _download_permit = acquire_download_permit(&state.download_semaphore).await?;
        let download_start = std::time::Instant::now();
        let response = state.music_api.download_file(&song_url.url).await?;

        // Check response status
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP {}", response.status()));
        }

        // Check content length. None means unknown/chunked transfer and is allowed.
        let content_length = response.content_length();
        if content_length == Some(0) {
            return Err(anyhow::anyhow!("Empty file or unable to get file size"));
        }

        // Enforce max download size limit when Content-Length is known
        let max_size_bytes = if state.config.max_download_size_mb > 0 {
            state.config.max_download_size_mb * 1024 * 1024
        } else {
            u64::MAX // No limit
        };

        if let Some(size) = content_length
            && size > max_size_bytes
        {
            return Err(anyhow::anyhow!(
                "File size ({} MB) exceeds maximum allowed size ({} MB)",
                size / (1024 * 1024),
                max_size_bytes / (1024 * 1024)
            ));
        }

        // Create audio buffer based on storage mode configuration
        let mut audio_buffer = AudioBuffer::new(
            &state.config,
            content_length.unwrap_or(0),
            filename.clone(),
            &state.config.cache_dir,
        )
        .await?;

        let mut stream = response.bytes_stream();
        let downloaded = if audio_buffer.is_disk() {
            let chunk_bytes = download_chunk_bytes(&state.config);
            let stream = stream.map_err(std::io::Error::other);
            let mut reader =
                tokio::io::BufReader::with_capacity(chunk_bytes, StreamReader::new(stream));
            let file = audio_buffer
                .disk_file_mut()
                .ok_or_else(|| anyhow::anyhow!("Disk buffer missing file handle"))?;
            let mut writer = tokio::io::BufWriter::with_capacity(chunk_bytes, file);
            let downloaded = tokio::io::copy_buf(&mut reader, &mut writer)
                .await
                .context("Failed to stream download to disk")?;
            tokio::io::AsyncWriteExt::flush(&mut writer)
                .await
                .context("Failed to flush disk writer")?;
            // Enforce max size even for disk mode (in case Content-Length was missing)
            if downloaded > max_size_bytes {
                return Err(anyhow::anyhow!(
                    "Downloaded size ({} MB) exceeds maximum allowed size ({} MB)",
                    downloaded / (1024 * 1024),
                    max_size_bytes / (1024 * 1024)
                ));
            }
            downloaded
        } else {
            let mut downloaded = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                downloaded += chunk.len() as u64;

                audio_buffer.write_chunk(&chunk).await?;

                // Enforce max size during streaming for chunked transfers
                if downloaded > max_size_bytes {
                    return Err(anyhow::anyhow!(
                        "Downloaded size ({} MB) exceeds maximum allowed size ({} MB)",
                        downloaded / (1024 * 1024),
                        max_size_bytes / (1024 * 1024)
                    ));
                }
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

    // Clean up thumbnail on audio download failure
    let (mut audio_buffer, downloaded) = match downloaded_result {
        Ok(result) => result,
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

    // Validate file size using downloaded byte count
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

    // 封面处理：使用320x320图片嵌入文件，缩略图用于Telegram显示
    // Overlap tag processing with upload client acquisition — they are independent.
    tracing::debug!("Processing tags for {} format", file_ext);
    let tag_perf_ctx = perf_ctx.clone();
    let tag_future = async {
        let tags_start = std::time::Instant::now();
        let result = apply_tags_in_blocking(
            audio_buffer,
            file_ext,
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
        Ok(buffer) => buffer,
        Err(e) => {
            cleanup_thumbnail_buffer(thumbnail_buffer).await;
            return Err(e);
        }
    };
    let (_upload_bot, raw_client, api_base_url) = match upload_client_result {
        Ok(client) => client,
        Err(e) => {
            cleanup_audio_buffer(audio_buffer).await;
            cleanup_thumbnail_buffer(thumbnail_buffer).await;
            return Err(e);
        }
    };

    // Get file size for database and logging
    let file_size = audio_buffer.size().await;
    let audio_file_size = file_size as i64;
    let duration_sec = (duration_ms.unwrap_or(0) / 1000) as i64;

    // Calculate actual bitrate from file size and duration
    // API's song_url.br is often theoretical (e.g., 1411kbps for FLAC) but
    // actual file may be compressed (e.g., 960kbps). Use real calculated value.
    let actual_bitrate_bps = if duration_sec > 0 {
        (8 * audio_file_size) / duration_sec
    } else {
        // Fallback to API value if duration is missing
        song_url.br as i64
    };

    tracing::debug!(
        "Bitrate - API: {} bps, Calculated from file: {} bps (duration: {}s)",
        song_url.br,
        actual_bitrate_bps,
        duration_sec
    );

    // Create song info for database
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
        from_user_id: msg.from.as_ref().map_or(0, |u| u.id.0 as i64),
        from_user_name: msg
            .from
            .as_ref()
            .and_then(|u| u.username.clone())
            .unwrap_or_default(),
        from_chat_id: msg.chat.id.0,
        from_chat_name: msg.chat.username().unwrap_or("").to_string(),
        created_at: now,
        updated_at: now,
        ..Default::default()
    };

    // Log final thumbnail status
    tracing::debug!("Final thumbnail status: {}", thumbnail_status);

    // Send the audio file
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

    // Acquire the upload permit only when we are ready to send bytes to Telegram.
    // This keeps slow downloads/tagging from occupying the upload lane.
    let upload_permit_wait_start = std::time::Instant::now();
    let _upload_permit = acquire_upload_permit_owned(Arc::clone(&state.upload_semaphore)).await?;
    perf_ctx.log_stage(
        PERF_STAGE_UPLOAD_PERMIT_WAIT,
        upload_permit_wait_start.elapsed(),
    );

    // Send audio file with enhanced error handling and proper MIME type
    tracing::debug!(
        "Sending audio file: {} ({:.2} MB)",
        audio_buffer.filename(),
        file_size as f64 / 1024.0 / 1024.0
    );

    // Simple approach: send as audio only
    let is_flac = file_ext == "flac";

    tracing::debug!("File format: {}", if is_flac { "FLAC" } else { "MP3" });

    // Serialize reply_markup once for reuse across attempts
    let reply_markup_json = serde_json::to_string(&keyboard).ok();

    // Move memory audio data to Bytes for upload without copying the full buffer.
    let audio_bytes = audio_buffer.take_memory_bytes_for_upload();

    // Send as audio only.
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

        // Extract file_id from raw API response
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
        tracing::warn!("Upload failed: {}", sanitize_sensitive_text(&e.to_string()));

        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;

        return Err(e);
    }

    cleanup_audio_buffer(audio_buffer).await;
    cleanup_thumbnail_buffer(thumbnail_buffer).await;

    // Save to database unless this upload is partially degraded.
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
                sanitize_sensitive_text(&e.to_string())
            );
        }
    }

    // Delete status message
    delete_status_message_resilient(bot, msg.chat.id, status_msg.id).await;

    Ok(())
}
