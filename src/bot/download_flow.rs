use super::collection_flow::{cover_download_failure_notice, download_cover_assets};
use super::core_flow::rate_limit_retry_delay_secs;
use super::maintenance::collect_maintenance_signals;
use super::music_ui::create_music_keyboard_for_target;
use super::permits::{acquire_download_permit, acquire_upload_permit_owned};
use super::replies::{
    delete_status_message_resilient, edit_status_message_resilient, send_reply_text,
};
use super::support::build_caption;
use super::tagging::{
    RawUploadParams, apply_tags_in_blocking, cleanup_audio_buffer, cleanup_thumbnail_buffer,
    log_perf,
};
use super::upload_client::{
    RawSendFileArgs, acquire_upload_client, extract_file_id_from_response, raw_send_file,
};
use super::wiring::{
    AudioFormat, BotState, MusicLinkTarget, PERF_STAGE_DB_SAVE, PERF_STAGE_DOWNLOAD_AUDIO,
    PERF_STAGE_PRE_UPLOAD_PATH, PERF_STAGE_TAG_PROCESS, PERF_STAGE_UPLOAD_CLIENT_ACQUIRE,
    PERF_STAGE_UPLOAD_PERMIT_WAIT, PERF_STAGE_UPLOAD_SEND, PerfTraceContext,
};
use crate::audio_buffer::AudioBuffer;
use crate::config::CoverMode;
use crate::database::SongInfo;
use crate::error::Result;
use crate::error::sanitized_error_chain;
use crate::i18n;
use crate::telegram::Message;
use crate::telegram::TelegramBot as Bot;
use crate::utils::{
    bytes_to_mb_f64, clean_filename, i64_to_u32_saturating, throughput_mbps, u64_to_i64_saturating,
    update_peak,
};
use anyhow::Context;
use futures_util::{StreamExt, TryStreamExt};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio_util::io::StreamReader;

/// Everything one download needs from its caller, carried unchanged through
/// every stage of the flow. Constructed once per attempt; the Chat Language
/// is resolved here and never re-resolved downstream.
pub(super) struct DownloadCtx<'a> {
    pub(super) bot: &'a Bot,
    pub(super) msg: &'a Message,
    pub(super) state: &'a Arc<BotState>,
    pub(super) lang: crate::i18n::ChatLanguage,
    pub(super) status_msg: &'a Message,
    pub(super) perf_ctx: &'a PerfTraceContext,
}

/// The song being delivered, independent of the conversation context.
pub(super) struct DownloadAndSendParams<'a> {
    pub(super) ctx: DownloadCtx<'a>,
    pub(super) song_detail: Arc<crate::music_api::SongDetail>,
    pub(super) song_url: &'a crate::music_api::SongUrl,
    pub(super) pre_upload_path_start: std::time::Instant,
    pub(super) artists: &'a str,
    pub(super) link_target: MusicLinkTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CoverPolicy {
    pub(super) download_original: bool,
    pub(super) download_thumbnail: bool,
    pub(super) embed_cover: bool,
}

pub(super) fn resolve_cover_policy(cover_mode: CoverMode) -> CoverPolicy {
    let download_original = matches!(cover_mode, CoverMode::Original | CoverMode::Both);
    let download_thumbnail = matches!(cover_mode, CoverMode::Thumbnail | CoverMode::Both);

    CoverPolicy {
        download_original,
        download_thumbnail,
        embed_cover: download_original || download_thumbnail,
    }
}

#[must_use]
pub(super) fn should_download_cover(policy: CoverPolicy) -> bool {
    policy.embed_cover || policy.download_thumbnail
}

pub(super) async fn download_and_send_music(p: &DownloadAndSendParams<'_>) -> Result<()> {
    let ctx = &p.ctx;
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

    let cover_mode = ctx.state.config.transfer.cover_mode;
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

    let cover_perf_ctx = ctx.perf_ctx.clone();
    let artwork_future = download_cover_assets(
        ctx.state,
        p.song_detail.as_ref(),
        song_id,
        cover_mode,
        download_cover,
        download_thumbnail,
        &cover_perf_ctx,
    );

    let audio_future = download_audio(ctx.state, &p.song_url.url, &filename, ctx.perf_ctx);

    let (downloaded_result, (cover_artwork_data, mut thumbnail_buffer, cover_retry_exhausted)) =
        tokio::join!(audio_future, artwork_future);
    let (audio_buffer, downloaded) = match downloaded_result {
        Ok(res) => res,
        Err(e) => {
            cleanup_thumbnail_buffer(thumbnail_buffer).await;
            return Err(e.into());
        }
    };

    let should_remove_song_cache = cover_retry_exhausted;

    if cover_retry_exhausted {
        tracing::warn!(
            "Cover fetch failed after retries for music_id {}. Audio will be sent without cover and cache will be removed",
            song_id
        );
    }

    if let Some(err_msg) = validate_downloaded_audio(&ctx.lang, downloaded) {
        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;
        edit_status_message_resilient(ctx.bot, ctx.msg.chat.id, ctx.status_msg.id, err_msg).await;
        return Ok(());
    }

    process_tag_and_upload(
        ctx,
        TagAndUploadParams {
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
            should_remove_song_cache,
            cover_retry_exhausted,
        },
    )
    .await
}

struct TagAndUploadParams<'a> {
    audio_buffer: AudioBuffer,
    thumbnail_buffer: &'a mut Option<crate::audio_buffer::ThumbnailBuffer>,
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
    should_remove_song_cache: bool,
    cover_retry_exhausted: bool,
}

#[allow(clippy::too_many_lines)]
async fn process_tag_and_upload(ctx: &DownloadCtx<'_>, p: TagAndUploadParams<'_>) -> Result<()> {
    let (mut audio_buffer, raw_client, api_base_url) = process_tags_and_acquire_client(
        ctx.state,
        ctx.perf_ctx,
        TagAndAcquireParams {
            audio_buffer: p.audio_buffer,
            thumbnail_buffer: p.thumbnail_buffer,
            audio_format: p.audio_format,
            song_detail: p.song_detail,
            cover_artwork_data: p.cover_artwork_data,
            embed_cover: p.embed_cover,
        },
    )
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
        msg: ctx.msg,
    });

    let caption = {
        build_caption(
            &ctx.lang,
            &song_info.song_name,
            &song_info.song_artists,
            &song_info.song_album,
            &song_info.file_ext,
            song_info.music_size,
            song_info.bit_rate,
            &ctx.state.bot_username,
        )
    };

    let reply_markup_json = serde_json::to_string(&create_music_keyboard_for_target(
        &ctx.lang,
        p.link_target,
        p.song_id,
        &song_info.song_name,
        &song_info.song_artists,
    ))
    .ok();

    let pre_upload_path_duration = p.pre_upload_path_start.elapsed();
    log_perf(PERF_STAGE_PRE_UPLOAD_PATH, pre_upload_path_duration);
    ctx.perf_ctx
        .log_stage(PERF_STAGE_PRE_UPLOAD_PATH, pre_upload_path_duration);

    let audio_bytes = audio_buffer.take_memory_bytes_for_upload();
    let file_id = acquire_permit_and_upload(
        ctx,
        &mut UploadFlowParams {
            raw_client: &raw_client,
            api_base_url: &api_base_url,
            audio_buffer: &mut audio_buffer,
            audio_bytes,
            thumbnail_buffer: p.thumbnail_buffer.as_ref(),
            caption: &caption,
            reply_markup_json,
            song_info: &song_info,
            audio_format: p.audio_format,
            file_size,
        },
    )
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
        ctx.state,
        &song_info,
        p.song_id,
        p.should_remove_song_cache,
        ctx.perf_ctx,
    )
    .await?;

    if p.cover_retry_exhausted {
        let notice = cover_download_failure_notice(&ctx.lang);
        if let Err(e) = send_reply_text(ctx.bot, ctx.msg, notice).await {
            tracing::warn!(
                "Failed to send cover fallback notice for music_id {}: {}",
                p.song_id,
                sanitized_error_chain(&e)
            );
        }
    }

    delete_status_message_resilient(ctx.bot, ctx.msg.chat.id, ctx.status_msg.id).await;
    Ok(())
}

struct UploadFlowParams<'a> {
    raw_client: &'a reqwest::Client,
    api_base_url: &'a str,
    audio_buffer: &'a mut AudioBuffer,
    audio_bytes: Option<bytes::Bytes>,
    thumbnail_buffer: Option<&'a crate::audio_buffer::ThumbnailBuffer>,
    caption: &'a str,
    reply_markup_json: Option<String>,
    song_info: &'a SongInfo,
    audio_format: AudioFormat,
    file_size: u64,
}

async fn acquire_permit_and_upload(
    ctx: &DownloadCtx<'_>,
    p: &mut UploadFlowParams<'_>,
) -> Result<Option<String>> {
    let mut attempt = 0;
    loop {
        let result = {
            let upload_permit_wait_start = std::time::Instant::now();
            let _upload_permit =
                acquire_upload_permit_owned(Arc::clone(&ctx.state.upload_semaphore)).await?;
            ctx.perf_ctx.log_stage(
                PERF_STAGE_UPLOAD_PERMIT_WAIT,
                upload_permit_wait_start.elapsed(),
            );
            execute_upload(ctx, p).await
        };
        match result {
            Err(error) => {
                let Some(delay) = rate_limit_retry_delay_secs(&error, attempt) else {
                    return Err(error);
                };
                attempt += 1;
                edit_status_message_resilient(
                    ctx.bot,
                    ctx.msg.chat.id,
                    ctx.status_msg.id,
                    i18n::tr_with(&ctx.lang, "rate_limited", "secs", &delay),
                )
                .await;
                // Keep the prepared buffer, but release the upload slot during flood wait.
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
            success => return success,
        }
    }
}

fn validate_downloaded_audio(lang: &crate::i18n::ChatLanguage, downloaded: u64) -> Option<String> {
    if downloaded == 0 {
        return Some(crate::i18n::tr(lang, "download_empty"));
    }
    if downloaded < 1024 {
        return Some(crate::i18n::tr_with(
            lang,
            "download_too_small",
            "bytes",
            &downloaded,
        ));
    }
    None
}

struct TagAndAcquireParams<'a> {
    audio_buffer: AudioBuffer,
    thumbnail_buffer: &'a mut Option<crate::audio_buffer::ThumbnailBuffer>,
    audio_format: AudioFormat,
    song_detail: Arc<crate::music_api::SongDetail>,
    cover_artwork_data: Option<bytes::Bytes>,
    embed_cover: bool,
}

async fn process_tags_and_acquire_client(
    state: &Arc<BotState>,
    perf_ctx: &PerfTraceContext,
    p: TagAndAcquireParams<'_>,
) -> Result<(AudioBuffer, reqwest::Client, String)> {
    tracing::debug!("Processing tags for {} format", p.audio_format);
    let tag_perf_ctx = perf_ctx.clone();
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

    let audio_buffer = match tag_result {
        Ok(buf) => buf,
        Err(e) => {
            cleanup_thumbnail_buffer(p.thumbnail_buffer.take()).await;
            return Err(e);
        }
    };

    let (raw_client, api_base_url) = match upload_client_result {
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

/// Copy a one-way download into a local file without flushing partial buffers
/// on read stalls. Flush the remainder after a complete, size-checked response.
/// Unlike a request/response proxy, the reader cannot depend on this file's
/// buffered writes, so a stalled reader needs no flush.
async fn write_audio_stream_to_disk<R>(
    reader: R,
    file: &mut tokio::fs::File,
    chunk_bytes: usize,
    max_download_size: u64,
) -> anyhow::Result<u64>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    let mut reader = reader.take(max_download_size.saturating_add(1));
    let mut writer = tokio::io::BufWriter::with_capacity(chunk_bytes, file);
    let mut downloaded = 0u64;
    loop {
        let chunk = reader
            .fill_buf()
            .await
            .context("Failed to read audio download")?;
        if chunk.is_empty() {
            break;
        }
        downloaded += chunk.len() as u64;
        if downloaded > max_download_size {
            anyhow::bail!("Download exceeds maximum allowed size ({max_download_size} bytes)");
        }
        writer
            .write_all(chunk)
            .await
            .context("Failed to write audio download to disk")?;
        let consumed = chunk.len();
        reader.consume(consumed);
        // Buffered reads and writes can both be immediately ready.
        tokio::task::consume_budget().await;
    }
    writer
        .flush()
        .await
        .context("Failed to flush disk writer")?;
    Ok(downloaded)
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
        max_download_size_bytes(state.config.storage.memory_max_file_mb)
    } else {
        max_download_size_bytes(state.config.storage.max_disk_download_mb)
    };

    let downloaded = if audio_buffer.is_disk() {
        let chunk_bytes = state.config.download_chunk_bytes();
        let stream = stream.map_err(std::io::Error::other);
        let reader = tokio::io::BufReader::with_capacity(chunk_bytes, StreamReader::new(stream));
        let file = audio_buffer
            .disk_file_mut()
            .ok_or_else(|| anyhow::anyhow!("Disk buffer missing file handle"))?;
        write_audio_stream_to_disk(reader, file, chunk_bytes, max_download_size)
            .await
            .context("Failed to stream download to disk")?
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

async fn execute_upload(
    ctx: &DownloadCtx<'_>,
    p: &mut UploadFlowParams<'_>,
) -> Result<Option<String>> {
    let is_flac = p.audio_format == AudioFormat::Flac;

    tracing::debug!(
        "Sending audio file: {} ({:.2} MB, {})",
        p.audio_buffer.filename(),
        bytes_to_mb_f64(p.file_size),
        if is_flac { "FLAC" } else { "MP3" }
    );

    let in_flight = ctx
        .state
        .upload_counters
        .in_flight
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    let peak_in_flight = update_peak(&ctx.state.upload_counters.peak_in_flight, in_flight);
    let upload_start = std::time::Instant::now();
    let duration_u32 = i64_to_u32_saturating(p.song_info.duration);
    let params = RawUploadParams {
        chat_id: ctx.msg.chat.id.0,
        caption: p.caption,
        reply_to_message_id: ctx.msg.id.0,
        reply_markup_json: p.reply_markup_json.clone(),
        title: Some(&p.song_info.song_name),
        performer: Some(&p.song_info.song_artists),
        duration: Some(duration_u32),
        thumbnail: p.thumbnail_buffer,
    };

    let upload_result = raw_send_file(&RawSendFileArgs {
        client: p.raw_client,
        api_base_url: p.api_base_url,
        config: &ctx.state.config,
        is_official_api: ctx.state.is_official_api,
        audio_buffer: p.audio_buffer,
        audio_bytes: p.audio_bytes.as_ref(),
        file_size: p.file_size,
        params: &params,
    })
    .await;

    let upload_duration = upload_start.elapsed();
    let in_flight_after = ctx
        .state
        .upload_counters
        .in_flight
        .fetch_sub(1, Ordering::Relaxed)
        - 1;
    log_perf("upload_audio", upload_duration);
    ctx.perf_ctx
        .log_stage(PERF_STAGE_UPLOAD_SEND, upload_duration);

    match upload_result {
        Ok(ref resp_json) => {
            let upload_mbps = throughput_mbps(p.file_size, upload_duration);
            ctx.state
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
                sanitized_error_chain(&e)
            );
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::{
        lang_ui::{test_bot_state, test_message},
        telegram::MockTelegramPollingServer,
    };

    async fn disk_buffer() -> AudioBuffer {
        AudioBuffer::new_disk(
            format!("download_{}.mp3", uuid::Uuid::new_v4()),
            std::env::temp_dir().to_str().unwrap(),
        )
        .await
        .unwrap()
    }

    fn stalled_audio_reader() -> (
        impl tokio::io::AsyncBufRead + Unpin,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let first = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"abcdefgh"))
        });
        let last = futures_util::stream::once(async move {
            waiting_tx.send(()).unwrap();
            resume_rx.await.unwrap();
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"ijkl"))
        });
        (
            StreamReader::new(first.chain(last).boxed()),
            waiting_rx,
            resume_tx,
        )
    }

    #[tokio::test]
    async fn disk_download_keeps_buffered_bytes_during_read_stall_and_flushes_at_eof() {
        let mut buffer = disk_buffer().await;
        let path = buffer.path().unwrap().to_path_buf();
        let (reader, waiting, resume) = stalled_audio_reader();
        let copy = write_audio_stream_to_disk(reader, buffer.disk_file_mut().unwrap(), 16, 12);
        tokio::pin!(copy);
        tokio::select! {
            result = &mut copy => panic!("copy completed before the reader resumed: {result:?}"),
            result = waiting => result.unwrap(),
        }
        assert_eq!(tokio::fs::metadata(&path).await.unwrap().len(), 0);
        resume.send(()).unwrap();
        assert_eq!(copy.await.unwrap(), 12);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"abcdefghijkl");
    }

    #[tokio::test]
    async fn disk_download_preserves_bytes_across_buffers_at_exact_size_limit() {
        let data: Vec<u8> = (0..65_537).map(|i| i as u8).collect();
        let mut buffer = disk_buffer().await;
        let reader = tokio::io::BufReader::with_capacity(73, std::io::Cursor::new(&data));
        let copied = write_audio_stream_to_disk(
            reader,
            buffer.disk_file_mut().unwrap(),
            257,
            data.len() as u64,
        )
        .await
        .unwrap();
        assert_eq!(copied, data.len() as u64);
        assert_eq!(tokio::fs::read(buffer.path().unwrap()).await.unwrap(), data);
    }

    #[tokio::test]
    async fn oversized_disk_download_fails_and_removes_partial_file() {
        let mut buffer = disk_buffer().await;
        let path = buffer.path().unwrap().to_path_buf();
        let reader = tokio::io::BufReader::with_capacity(4, &b"too much audio"[..]);
        let error = write_audio_stream_to_disk(reader, buffer.disk_file_mut().unwrap(), 4, 8)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("maximum allowed size (8 bytes)"));
        assert!(tokio::fs::metadata(&path).await.unwrap().len() <= 8);
        drop(buffer);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn disk_download_read_failure_is_reported_and_partial_file_is_removed() {
        let mut buffer = disk_buffer().await;
        let path = buffer.path().unwrap().to_path_buf();
        let reader = StreamReader::new(futures_util::stream::iter([
            Ok(bytes::Bytes::from_static(b"partial audio")),
            Err(std::io::Error::other("interrupted response")),
        ]));
        let error = write_audio_stream_to_disk(reader, buffer.disk_file_mut().unwrap(), 4, 100)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("interrupted response"));
        drop(buffer);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn disk_download_reports_file_io_errors_including_final_flush() {
        let buffer = disk_buffer().await;
        for capacity in [4, 16] {
            let mut file = tokio::fs::File::open(buffer.path().unwrap()).await.unwrap();
            let error = write_audio_stream_to_disk(&b"audio data"[..], &mut file, capacity, 100)
                .await
                .unwrap_err();
            // Tokio can report an asynchronous write failure at the next flush.
            assert!(error.downcast_ref::<std::io::Error>().is_some());
        }
    }

    #[tokio::test]
    async fn disk_download_yields_while_both_buffers_are_ready() {
        let mut buffer = disk_buffer().await;
        let chunks_read = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&chunks_read);
        let stream = futures_util::stream::iter((0..1000).map(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data"))
        }));
        let reader = StreamReader::new(stream);
        tokio::select! {
            biased;
            result = write_audio_stream_to_disk(reader, buffer.disk_file_mut().unwrap(), 4096, 4000) => {
                panic!("copy did not yield before completion: {result:?}");
            }
            _ = std::future::ready(()) => {}
        }
        let count = chunks_read.load(Ordering::Relaxed);
        assert!(
            count > 0 && count < 1000,
            "copied {count} chunks before yielding"
        );
    }

    #[tokio::test]
    async fn cancelling_stalled_disk_download_removes_partial_file() {
        let mut buffer = disk_buffer().await;
        let path = buffer.path().unwrap().to_path_buf();
        let (reader, waiting, _resume) = stalled_audio_reader();
        {
            let copy = write_audio_stream_to_disk(reader, buffer.disk_file_mut().unwrap(), 4, 100);
            tokio::pin!(copy);
            tokio::select! {
                result = &mut copy => panic!("copy completed before cancellation: {result:?}"),
                result = waiting => result.unwrap(),
            }
        }
        drop(buffer);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn rate_limited_upload_replays_prepared_audio_and_keyboard_in_memory_and_disk_modes() {
        for memory in [true, false] {
            let server = MockTelegramPollingServer::start().await;
            let state = test_bot_state("upload_replay").await;
            {
                let mut upload = state.upload_client_state.lock().await;
                upload.raw_client = Some(reqwest::Client::new());
                upload.upload_api_url = format!("{}botTEST/", server.base_url());
            }
            let bot = Bot::new("TEST").set_api_url(&server.base_url().parse().unwrap());
            let msg = test_message(-77, "group");
            let trace = super::super::wiring::build_perf_trace_context(&state, 71, "miss");
            let ctx = DownloadCtx {
                bot: &bot,
                msg: &msg,
                state: &state,
                lang: i18n::default_lang_zh(),
                status_msg: &msg,
                perf_ctx: &trace,
            };
            let mut buffer = if memory {
                AudioBuffer::Memory {
                    data: Vec::new(),
                    filename: "replay.mp3".into(),
                    reservation: None,
                }
            } else {
                AudioBuffer::new_disk(
                    format!("replay_{}.mp3", uuid::Uuid::new_v4()),
                    std::env::temp_dir().to_str().unwrap(),
                )
                .await
                .unwrap()
            };
            buffer
                .write_chunk(b"UNCHANGED_AUDIO_PAYLOAD")
                .await
                .unwrap();
            buffer.finish().await.unwrap();
            let disk_path = buffer.path().map(std::path::Path::to_path_buf);
            let mut thumbnail = None;
            process_tag_and_upload(
                &ctx,
                TagAndUploadParams {
                    audio_buffer: buffer,
                    thumbnail_buffer: &mut thumbnail,
                    audio_format: AudioFormat::Mp3,
                    song_detail: Arc::new(crate::music_api::SongDetail {
                        id: 71,
                        name: "Replay".into(),
                        dt: Some(1000),
                        ar: None,
                        al: None,
                    }),
                    cover_artwork_data: None,
                    embed_cover: false,
                    song_id: 71,
                    song_name: "Replay",
                    artists: "Artist",
                    song_album: "Album",
                    file_ext: "mp3",
                    duration_ms: Some(1000),
                    api_bitrate: 128000,
                    link_target: MusicLinkTarget::Song(71),
                    pre_upload_path_start: std::time::Instant::now(),
                    should_remove_song_cache: false,
                    cover_retry_exhausted: false,
                },
            )
            .await
            .unwrap();
            let bodies = server.upload_bodies();
            assert_eq!(bodies.len(), 2);
            let audio_part = |body: &str| {
                let start = body.find("filename=").unwrap();
                let data = body[start..].split_once("\r\n\r\n").unwrap().1;
                data.split("\r\n--").next().unwrap().to_string()
            };
            assert_eq!(audio_part(&bodies[0]), audio_part(&bodies[1]));
            for body in &bodies {
                assert!(body.contains("ID3"));
                assert!(body.contains("UNCHANGED_AUDIO_PAYLOAD"));
                assert!(body.contains("reply_markup"));
            }
            assert_eq!(
                state
                    .database
                    .get_song_by_music_id(71)
                    .await
                    .unwrap()
                    .unwrap()
                    .file_id
                    .as_deref(),
                Some("replayed")
            );
            if let Some(path) = disk_path {
                assert!(!path.exists());
            }
            assert_eq!(state.upload_semaphore.available_permits(), 1);
            assert_eq!(state.upload_counters.in_flight.load(Ordering::Relaxed), 0);
        }
    }
}
