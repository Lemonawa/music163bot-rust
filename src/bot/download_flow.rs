use super::{
    Arc, AudioBuffer, AudioFormat, Bot, BotState, Context, Message, MusicLinkTarget, Ordering,
    PERF_STAGE_DB_SAVE, PERF_STAGE_DOWNLOAD_AUDIO, PERF_STAGE_PRE_UPLOAD_PATH,
    PERF_STAGE_TAG_PROCESS, PERF_STAGE_UPLOAD_CLIENT_ACQUIRE, PERF_STAGE_UPLOAD_PERMIT_WAIT,
    PERF_STAGE_UPLOAD_SEND, PerfTraceContext, RawSendFileArgs, RawUploadParams, Result, SongInfo,
    StreamReader, acquire_download_permit, acquire_upload_client, acquire_upload_permit_owned,
    apply_tags_in_blocking, build_caption, bytes_to_mb_f64, clean_filename, cleanup_audio_buffer,
    cleanup_thumbnail_buffer, collect_maintenance_signals, cover_download_failure_notice,
    create_music_keyboard_for_target, delete_status_message_resilient, download_cover_assets,
    edit_status_message_resilient, extract_file_id_from_response, i64_to_u32_saturating, log_perf,
    raw_send_file, resolve_cover_policy, resolve_message, sanitize_sensitive_text, send_reply_text,
    should_download_cover, should_remove_song_cache_after_partial_failure, throughput_mbps,
    u64_to_i64_saturating, update_peak,
};
use futures_util::{StreamExt, TryStreamExt};

pub(super) struct DownloadAndSendParams<'a> {
    pub(super) bot: &'a Bot,
    pub(super) msg: &'a Message,
    pub(super) state: &'a Arc<BotState>,
    pub(super) song_detail: Arc<crate::music_api::SongDetail>,
    pub(super) song_url: &'a crate::music_api::SongUrl,
    pub(super) status_msg: &'a Message,
    pub(super) pre_upload_path_start: std::time::Instant,
    pub(super) perf_ctx: &'a PerfTraceContext,
    pub(super) artists: &'a str,
    pub(super) link_target: MusicLinkTarget,
}

pub(super) async fn download_and_send_music(p: &DownloadAndSendParams<'_>) -> Result<()> {
    let audio_format = if p.song_url.url.contains(".flac") {
        AudioFormat::Flac
    } else {
        AudioFormat::Mp3
    };
    let file_ext = audio_format.as_str();

    let filename = clean_filename(&format!(
        "{} - {}.{}",
        p.artists.replace('/', ","),
        p.song_detail.name,
        file_ext
    ));

    let cover_mode = p.state.config.cover_mode;
    let cover_policy = resolve_cover_policy(cover_mode);
    let download_thumbnail = cover_policy.download_thumbnail;
    let download_cover = should_download_cover(cover_policy);

    let song_id = p.song_detail.id;
    let song_name = p.song_detail.name.clone();
    let song_album = p
        .song_detail
        .al
        .as_ref()
        .map_or_else(|| "Unknown Album".to_string(), |al| al.name.clone());
    let duration_ms = p.song_detail.dt;

    let cover_perf_ctx = p.perf_ctx.clone();
    let artwork_future = download_cover_assets(
        p.state,
        p.song_detail.as_ref(),
        song_id,
        cover_mode,
        download_cover,
        download_thumbnail,
        &cover_perf_ctx,
    );

    let audio_future = download_audio(p.state, &p.song_url.url, &filename, p.perf_ctx);

    let (downloaded_result, (cover_artwork_data, mut thumbnail_buffer, cover_retry_exhausted)) =
        tokio::join!(audio_future, artwork_future);
    let (audio_buffer, downloaded) = match downloaded_result {
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

    if let Some(err_msg) = validate_downloaded_audio(downloaded) {
        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;
        edit_status_message_resilient(p.bot, p.msg.chat.id, p.status_msg.id, err_msg).await;
        return Ok(());
    }

    process_tag_and_upload(TagAndUploadParams {
        bot: p.bot,
        msg: p.msg,
        state: p.state,
        audio_buffer,
        thumbnail_buffer: &mut thumbnail_buffer,
        audio_format,
        song_detail: Arc::clone(&p.song_detail),
        cover_artwork_data,
        embed_cover: cover_policy.embed_cover,
        song_id,
        song_name: &song_name,
        artists: p.artists,
        song_album: &song_album,
        file_ext,
        duration_ms,
        api_bitrate: p.song_url.br,
        link_target: p.link_target,
        pre_upload_path_start: p.pre_upload_path_start,
        perf_ctx: p.perf_ctx,
        status_msg: p.status_msg,
        should_remove_song_cache,
        cover_retry_exhausted,
    })
    .await
}

struct TagAndUploadParams<'a> {
    bot: &'a Bot,
    msg: &'a Message,
    state: &'a Arc<BotState>,
    audio_buffer: AudioBuffer,
    thumbnail_buffer: &'a mut Option<crate::bot::ThumbnailBuffer>,
    audio_format: AudioFormat,
    song_detail: Arc<crate::music_api::SongDetail>,
    cover_artwork_data: Option<bytes::Bytes>,
    embed_cover: bool,
    song_id: u64,
    song_name: &'a str,
    artists: &'a str,
    song_album: &'a str,
    file_ext: &'a str,
    duration_ms: Option<u64>,
    api_bitrate: u64,
    link_target: MusicLinkTarget,
    pre_upload_path_start: std::time::Instant,
    perf_ctx: &'a PerfTraceContext,
    status_msg: &'a Message,
    should_remove_song_cache: bool,
    cover_retry_exhausted: bool,
}

#[allow(clippy::too_many_lines)]
async fn process_tag_and_upload(p: TagAndUploadParams<'_>) -> Result<()> {
    let (mut audio_buffer, raw_client, api_base_url) =
        process_tags_and_acquire_client(TagAndAcquireParams {
            state: p.state,
            audio_buffer: p.audio_buffer,
            thumbnail_buffer: p.thumbnail_buffer,
            audio_format: p.audio_format,
            song_detail: p.song_detail,
            cover_artwork_data: p.cover_artwork_data,
            embed_cover: p.embed_cover,
            perf_ctx: p.perf_ctx,
        })
        .await?;

    let file_size = audio_buffer.size().await;
    if file_size == 0 {
        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(p.thumbnail_buffer.take()).await;
        return Err(anyhow::anyhow!("Audio file is empty after processing").into());
    }

    let mut song_info = build_upload_metadata(&UploadMetadataParams {
        song_id: p.song_id,
        song_name: p.song_name,
        artists: p.artists,
        song_album: p.song_album,
        file_ext: p.file_ext,
        file_size,
        duration_ms: p.duration_ms,
        api_bitrate: p.api_bitrate,
        link_target: p.link_target,
        msg: p.msg,
    });

    let lang = resolve_message(
        &p.state.database,
        &p.state.chat_languages,
        &p.state.config.default_language,
        p.msg,
    )
    .await;
    let caption = {
        build_caption(
            &lang,
            &song_info.song_name,
            &song_info.song_artists,
            &song_info.song_album,
            &song_info.file_ext,
            song_info.music_size,
            song_info.bit_rate,
            &p.state.bot_username,
        )
    };

    let reply_markup_json = serde_json::to_string(&create_music_keyboard_for_target(
        &lang,
        p.link_target,
        p.song_id,
        &song_info.song_name,
        &song_info.song_artists,
    ))
    .ok();

    let pre_upload_path_duration = p.pre_upload_path_start.elapsed();
    log_perf(PERF_STAGE_PRE_UPLOAD_PATH, pre_upload_path_duration);
    p.perf_ctx
        .log_stage(PERF_STAGE_PRE_UPLOAD_PATH, pre_upload_path_duration);

    let file_id = acquire_permit_and_upload(UploadFlowParams {
        state: p.state,
        msg: p.msg,
        raw_client: &raw_client,
        api_base_url: &api_base_url,
        audio_buffer: &mut audio_buffer,
        thumbnail_buffer: p.thumbnail_buffer.as_ref(),
        caption: &caption,
        reply_markup_json,
        song_info: &song_info,
        audio_format: p.audio_format,
        file_size,
        perf_ctx: p.perf_ctx,
    })
    .await;

    match file_id {
        Ok(fid) => song_info.file_id = fid,
        Err(e) => {
            cleanup_audio_buffer(audio_buffer).await;
            cleanup_thumbnail_buffer(p.thumbnail_buffer.take()).await;
            return Err(e);
        }
    }

    cleanup_audio_buffer(audio_buffer).await;
    cleanup_thumbnail_buffer(p.thumbnail_buffer.take()).await;
    save_song_and_notify(
        p.state,
        &song_info,
        p.song_id,
        p.should_remove_song_cache,
        p.perf_ctx,
    )
    .await?;

    if p.cover_retry_exhausted {
        let notice = {
            let lang = resolve_message(
                &p.state.database,
                &p.state.chat_languages,
                &p.state.config.default_language,
                p.msg,
            )
            .await;
            cover_download_failure_notice(&lang)
        };
        if let Err(e) = send_reply_text(p.bot, p.msg, notice).await {
            tracing::warn!(
                "Failed to send cover fallback notice for music_id {}: {}",
                p.song_id,
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
        }
    }

    delete_status_message_resilient(p.bot, p.msg.chat.id, p.status_msg.id).await;
    Ok(())
}

struct UploadFlowParams<'a> {
    state: &'a Arc<BotState>,
    msg: &'a Message,
    raw_client: &'a reqwest::Client,
    api_base_url: &'a str,
    audio_buffer: &'a mut AudioBuffer,
    thumbnail_buffer: Option<&'a crate::bot::ThumbnailBuffer>,
    caption: &'a str,
    reply_markup_json: Option<String>,
    song_info: &'a SongInfo,
    audio_format: AudioFormat,
    file_size: u64,
    perf_ctx: &'a PerfTraceContext,
}

async fn acquire_permit_and_upload(mut p: UploadFlowParams<'_>) -> Result<Option<String>> {
    let upload_permit_wait_start = std::time::Instant::now();
    let _upload_permit = acquire_upload_permit_owned(Arc::clone(&p.state.upload_semaphore)).await?;
    p.perf_ctx.log_stage(
        PERF_STAGE_UPLOAD_PERMIT_WAIT,
        upload_permit_wait_start.elapsed(),
    );

    execute_upload(&mut p).await
}

fn validate_downloaded_audio(downloaded: u64) -> Option<String> {
    let lang = crate::i18n::ChatLanguage::new("zh");
    if downloaded == 0 {
        return Some(crate::i18n::tr(&lang, "download_empty"));
    }
    if downloaded < 1024 {
        return Some(crate::i18n::tr_with(
            &lang,
            "download_too_small",
            "bytes",
            &downloaded,
        ));
    }
    None
}

struct TagAndAcquireParams<'a> {
    state: &'a Arc<BotState>,
    audio_buffer: AudioBuffer,
    thumbnail_buffer: &'a mut Option<crate::bot::ThumbnailBuffer>,
    audio_format: AudioFormat,
    song_detail: Arc<crate::music_api::SongDetail>,
    cover_artwork_data: Option<bytes::Bytes>,
    embed_cover: bool,
    perf_ctx: &'a PerfTraceContext,
}

async fn process_tags_and_acquire_client(
    p: TagAndAcquireParams<'_>,
) -> Result<(AudioBuffer, reqwest::Client, String)> {
    tracing::debug!("Processing tags for {} format", p.audio_format);
    let tag_perf_ctx = p.perf_ctx.clone();
    let tag_future = async {
        let tags_start = std::time::Instant::now();
        let result = apply_tags_in_blocking(
            p.audio_buffer,
            p.audio_format,
            p.song_detail,
            p.cover_artwork_data,
            p.embed_cover,
        )
        .await;
        let tags_duration = tags_start.elapsed();
        log_perf("process_tags", tags_duration);
        tag_perf_ctx.log_stage(PERF_STAGE_TAG_PROCESS, tags_duration);
        result
    };

    let upload_client_perf_ctx = p.perf_ctx.clone();
    let upload_client_future = async {
        let upload_client_start = std::time::Instant::now();
        let result = acquire_upload_client(p.state).await;
        upload_client_perf_ctx.log_stage(
            PERF_STAGE_UPLOAD_CLIENT_ACQUIRE,
            upload_client_start.elapsed(),
        );
        result
    };

    let (tag_result, upload_client_result) = tokio::join!(tag_future, upload_client_future);

    let audio_buffer = match tag_result {
        Ok(buf) => buf,
        Err(e) => {
            cleanup_thumbnail_buffer(p.thumbnail_buffer.take()).await;
            return Err(e);
        }
    };

    let (_upload_bot, raw_client, api_base_url) = match upload_client_result {
        Ok(res) => res,
        Err(e) => {
            cleanup_audio_buffer(audio_buffer).await;
            cleanup_thumbnail_buffer(p.thumbnail_buffer.take()).await;
            return Err(e);
        }
    };

    Ok((audio_buffer, raw_client, api_base_url))
}

struct UploadMetadataParams<'a> {
    song_id: u64,
    song_name: &'a str,
    artists: &'a str,
    song_album: &'a str,
    file_ext: &'a str,
    file_size: u64,
    duration_ms: Option<u64>,
    api_bitrate: u64,
    link_target: MusicLinkTarget,
    msg: &'a Message,
}

fn build_upload_metadata(p: &UploadMetadataParams<'_>) -> SongInfo {
    let audio_file_size = u64_to_i64_saturating(p.file_size);
    let duration_ms_val = p.duration_ms.unwrap_or(0);
    let duration_sec = u64_to_i64_saturating(duration_ms_val / 1000);
    let actual_bitrate_bps = if duration_sec > 0 {
        (8 * audio_file_size) / duration_sec
    } else {
        u64_to_i64_saturating(p.api_bitrate)
    };

    let now = chrono::Utc::now();
    let program_id = match p.link_target {
        MusicLinkTarget::Program(pid) => Some(u64_to_i64_saturating(pid)),
        MusicLinkTarget::Song(_) => None,
    };
    SongInfo {
        music_id: u64_to_i64_saturating(p.song_id),
        program_id,
        song_name: p.song_name.to_string(),
        song_artists: p.artists.to_string(),
        song_album: p.song_album.to_string(),
        file_ext: p.file_ext.to_string(),
        music_size: audio_file_size,
        pic_size: 0,
        emb_pic_size: 0,
        bit_rate: actual_bitrate_bps,
        duration: duration_sec,
        file_id: None,
        thumb_file_id: None,
        from_user_id: p.msg.from.as_ref().map_or(0, |u| u.id),
        from_user_name: p
            .msg
            .from
            .as_ref()
            .and_then(|u| u.username.clone())
            .unwrap_or_default(),
        from_chat_id: p.msg.chat.id.0,
        from_chat_name: p.msg.chat.username.as_deref().unwrap_or("").to_string(),
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
}

async fn save_song_and_notify(
    state: &Arc<BotState>,
    song_info: &SongInfo,
    song_id: u64,
    should_remove: bool,
    perf_ctx: &PerfTraceContext,
) -> Result<()> {
    let db_save_start = std::time::Instant::now();
    let result = if should_remove {
        let song_id_i64 = u64_to_i64_saturating(song_id);
        if let Err(e) = state.database.delete_song_by_music_id(song_id_i64).await {
            tracing::warn!(
                "Failed to remove partial cache for music_id {} after upload: {}",
                song_id,
                e
            );
        }
        Ok(())
    } else {
        let save_result = state.database.save_song_info(song_info).await;
        match classify_post_upload_db_result(save_result.is_ok()) {
            PostUploadDbAction::Persisted => {
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
            }
            PostUploadDbAction::LogAndContinue => {
                if let Err(e) = save_result {
                    // The audio was already uploaded and delivered to the user at this point, so a
                    // cache-persistence failure must not surface as a user-facing "处理失败". Log it
                    // and continue; the only consequence is that this track is re-fetched next time.
                    tracing::warn!(
                        "Failed to persist song cache for music_id {} after successful upload: {}",
                        song_id,
                        e
                    );
                }
            }
        }
        Ok(())
    };
    perf_ctx.log_stage(PERF_STAGE_DB_SAVE, db_save_start.elapsed());
    result
}

/// Outcome of the post-upload cache write, deciding how it affects the user-facing result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PostUploadDbAction {
    /// Cache persisted successfully; safe to emit maintenance signals.
    Persisted,
    /// Persistence failed, but the audio was already delivered to the user, so the failure is
    /// logged and the overall operation still reports success.
    LogAndContinue,
}

/// Classify a post-upload cache-write result. A failure is never fatal because the upload
/// already succeeded by the time this runs.
pub(super) fn classify_post_upload_db_result(save_succeeded: bool) -> PostUploadDbAction {
    if save_succeeded {
        PostUploadDbAction::Persisted
    } else {
        PostUploadDbAction::LogAndContinue
    }
}

/// Convert a megabyte cap into a byte cap. Uses saturating multiplication so an absurdly large
/// configured value clamps to `u64::MAX` (effectively unlimited) instead of wrapping in release
/// builds (which would silently shrink the cap) or panicking in debug builds.
pub(super) fn max_download_size_bytes(mb: u64) -> u64 {
    mb.saturating_mul(1024).saturating_mul(1024)
}

async fn download_audio(
    state: &Arc<BotState>,
    url: &str,
    filename: &str,
    perf_ctx: &PerfTraceContext,
) -> anyhow::Result<(AudioBuffer, u64)> {
    let _download_permit = acquire_download_permit(&state.download_semaphore).await?;
    let download_start = std::time::Instant::now();
    let response = state.music_api.download_file(url).await?;

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
        filename.to_string(),
        &state.config.cache_dir,
    )
    .await?;

    let mut stream = response.bytes_stream();
    let max_download_size = if audio_buffer.is_memory() {
        max_download_size_bytes(state.config.memory_max_file_mb)
    } else {
        max_download_size_bytes(state.config.max_disk_download_mb)
    };

    let downloaded = if audio_buffer.is_disk() {
        let chunk_bytes = state.config.download_chunk_bytes();
        let stream = stream.map_err(std::io::Error::other);
        let reader = tokio::io::BufReader::with_capacity(chunk_bytes, StreamReader::new(stream));
        let mut limited_reader =
            tokio::io::AsyncReadExt::take(reader, max_download_size.saturating_add(1));
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
    perf_ctx.log_stage(PERF_STAGE_DOWNLOAD_AUDIO, download_duration);

    Ok((audio_buffer, downloaded))
}

async fn execute_upload(p: &mut UploadFlowParams<'_>) -> Result<Option<String>> {
    let is_flac = p.audio_format == AudioFormat::Flac;

    tracing::debug!(
        "Sending audio file: {} ({:.2} MB, {})",
        p.audio_buffer.filename(),
        bytes_to_mb_f64(p.file_size),
        if is_flac { "FLAC" } else { "MP3" }
    );

    let audio_bytes = p.audio_buffer.take_memory_bytes_for_upload();

    let in_flight = p
        .state
        .upload_counters
        .in_flight
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    let peak_in_flight = update_peak(&p.state.upload_counters.peak_in_flight, in_flight);
    let upload_start = std::time::Instant::now();
    let duration_u32 = i64_to_u32_saturating(p.song_info.duration);
    let params = RawUploadParams {
        chat_id: p.msg.chat.id.0,
        caption: p.caption,
        reply_to_message_id: p.msg.id.0,
        reply_markup_json: p.reply_markup_json.take(),
        title: Some(&p.song_info.song_name),
        performer: Some(&p.song_info.song_artists),
        duration: Some(duration_u32),
        thumbnail: p.thumbnail_buffer,
    };

    let upload_result = raw_send_file(&RawSendFileArgs {
        client: p.raw_client,
        api_base_url: p.api_base_url,
        config: &p.state.config,
        is_official_api: p.state.is_official_api,
        audio_buffer: p.audio_buffer,
        audio_bytes: audio_bytes.as_ref(),
        file_size: p.file_size,
        params: &params,
    })
    .await;

    let upload_duration = upload_start.elapsed();
    let in_flight_after = p
        .state
        .upload_counters
        .in_flight
        .fetch_sub(1, Ordering::Relaxed)
        - 1;
    log_perf("upload_audio", upload_duration);
    p.perf_ctx
        .log_stage(PERF_STAGE_UPLOAD_SEND, upload_duration);

    match upload_result {
        Ok(ref resp_json) => {
            let upload_mbps = throughput_mbps(p.file_size, upload_duration);
            p.state
                .runtime_metrics
                .record_upload_speed(p.file_size, upload_duration);
            tracing::info!(
                "Upload completed ({}) in {:.2}s ({:.2} MB/s, inflight: {}, peak: {})",
                if is_flac { "FLAC" } else { "MP3" },
                upload_duration.as_secs_f64(),
                upload_mbps,
                in_flight_after,
                peak_in_flight
            );
            Ok(extract_file_id_from_response(resp_json))
        }
        Err(e) => {
            tracing::warn!(
                "Upload failed after {:.2}s ({:.2} MB/s, inflight: {}, peak: {}): {}",
                upload_duration.as_secs_f64(),
                throughput_mbps(p.file_size, upload_duration),
                in_flight_after,
                peak_in_flight,
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            Err(e)
        }
    }
}
