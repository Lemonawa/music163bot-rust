use super::*;

pub(super) async fn apply_tags_in_blocking(
    mut audio_buffer: AudioBuffer,
    file_ext: &str,
    song_detail: Arc<crate::music_api::SongDetail>,
    artwork_data: Option<Bytes>,
    embed_cover: bool,
) -> Result<AudioBuffer> {
    let file_ext = file_ext.to_string(); // move into blocking task
    tokio::task::spawn_blocking(move || {
        let embed_artwork = if embed_cover {
            artwork_data.as_ref().map(std::convert::AsRef::as_ref)
        } else {
            None
        };

        match file_ext.as_str() {
            "mp3" => {
                let cover_label = if embed_cover { "320" } else { "none" };
                tracing::debug!("Adding ID3 tags to MP3 (cover: {})", cover_label);
                match audio_buffer.add_id3_tags(&song_detail, embed_artwork) {
                    Ok(()) => tracing::debug!("MP3 tags added successfully"),
                    Err(e) => tracing::warn!("Failed to add MP3 tags: {}", e),
                }
            }
            "flac" => {
                let cover_label = if embed_cover { "320" } else { "none" };
                tracing::debug!("Adding FLAC metadata (cover: {})", cover_label);
                match audio_buffer.add_flac_metadata(&song_detail, embed_artwork) {
                    Ok(()) => tracing::debug!("FLAC metadata added successfully"),
                    Err(e) => tracing::warn!("Failed to add FLAC metadata: {}", e),
                }
            }
            _ => {
                tracing::debug!("Unknown format {}, skipping tag embedding", file_ext);
            }
        }

        audio_buffer
    })
    .await
    .map_err(|e| BotError::Other(anyhow::anyhow!("metadata task join failed: {e}")))
}

pub(super) fn cached_music_link_target(program_id: Option<i64>, music_id: u64) -> MusicLinkTarget {
    if let Some(program_id) = program_id.and_then(|id| u64::try_from(id).ok()) {
        MusicLinkTarget::Program(program_id)
    } else {
        MusicLinkTarget::Song(music_id)
    }
}

pub(super) fn create_music_keyboard_for_target(
    link_target: MusicLinkTarget,
    music_id: u64,
    song_name: &str,
    artists: &str,
) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    let primary_url_result = match link_target {
        MusicLinkTarget::Song(link_music_id) => {
            build_music_url("https://music.163.com", link_music_id)
        }
        MusicLinkTarget::Program(program_id) => {
            build_program_url("https://music.163.com", program_id)
        }
    };

    match primary_url_result {
        Ok(url) => {
            rows.push(vec![InlineKeyboardButton::url(
                format!("{song_name} - {artists}"),
                url,
            )]);
        }
        Err(e) => {
            tracing::warn!("Failed to build music URL for music_id {}: {}", music_id, e);
        }
    }

    let switch_inline_query_url = match link_target {
        MusicLinkTarget::Song(link_music_id) => {
            format!("https://music.163.com/song?id={link_music_id}")
        }
        MusicLinkTarget::Program(program_id) => {
            format!("https://music.163.com/program?id={program_id}")
        }
    };
    rows.push(vec![InlineKeyboardButton::switch_inline_query(
        "分享给朋友",
        switch_inline_query_url,
    )]);

    InlineKeyboardMarkup::new(rows)
}

pub(super) fn build_music_url(
    base_url: &str,
    music_id: u64,
) -> std::result::Result<reqwest::Url, url::ParseError> {
    let mut url = reqwest::Url::parse(base_url)?;
    url.set_path("song");
    url.set_query(Some(&format!("id={music_id}")));
    Ok(url)
}

pub(super) fn build_program_url(
    base_url: &str,
    program_id: u64,
) -> std::result::Result<reqwest::Url, url::ParseError> {
    let mut url = reqwest::Url::parse(base_url)?;
    url.set_path("program");
    url.set_query(Some(&format!("id={program_id}")));
    Ok(url)
}

pub(super) fn parse_api_url(api_url: &str) -> std::result::Result<reqwest::Url, url::ParseError> {
    reqwest::Url::parse(api_url)
}

pub(super) fn is_admin(msg: &Message, config: &Config) -> bool {
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);
    config.bot_admin.contains(&user_id)
}

pub(super) async fn ensure_admin(
    bot: &Bot,
    msg: &Message,
    config: &Config,
) -> ResponseResult<bool> {
    if is_admin(msg, config) {
        Ok(true)
    } else {
        send_reply_text(bot, msg, "❌ 该命令仅限管理员使用").await?;
        Ok(false)
    }
}

pub(super) fn is_official_telegram_api(api_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(api_url) else {
        return false;
    };

    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.telegram.org"))
}

pub(super) async fn local_file_uri_from_path(path: &std::path::Path) -> Option<String> {
    let canonical = tokio::fs::canonicalize(path).await.ok()?;
    url::Url::from_file_path(canonical)
        .ok()
        .map(|url| url.to_string())
}

pub(super) async fn maybe_local_file_uri(
    config: &Config,
    is_official_api: bool,
    path: &std::path::Path,
) -> Option<String> {
    if !config.upload_local_file_uri {
        return None;
    }

    if is_official_api {
        return None;
    }

    local_file_uri_from_path(path).await
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum UploadFileTarget {
    LocalUri(String),
    Multipart,
}

pub(super) async fn select_local_upload_target(
    config: &Config,
    is_official_api: bool,
    path: &std::path::Path,
) -> UploadFileTarget {
    maybe_local_file_uri(config, is_official_api, path)
        .await
        .map_or(UploadFileTarget::Multipart, UploadFileTarget::LocalUri)
}

pub(super) fn url_bitrate_candidates(has_music_u: bool) -> &'static [u64] {
    if has_music_u {
        &[999_000, 320_000, 128_000]
    } else {
        &[320_000, 128_000]
    }
}

pub(super) fn should_remove_song_cache_after_partial_failure(cover_retry_exhausted: bool) -> bool {
    cover_retry_exhausted
}

pub(super) const MESSAGE_TASK_LINK_HINTS: [&str; 3] = ["music.163.com", "163cn.tv", "163cn.link"];
pub(super) const MUSIC_ID_EXTRACT_FAILED_TEXT: &str = "无法从链接中提取音乐ID";

pub(super) fn contains_music_link_hint(text: &str) -> bool {
    MESSAGE_TASK_LINK_HINTS
        .iter()
        .any(|hint| text.contains(hint))
}

pub(super) fn is_spawnable_command_text(text: &str) -> bool {
    text.starts_with('/')
}

pub(super) fn is_command_text(text: &str) -> bool {
    text.starts_with('/')
}

pub(super) fn should_spawn_message_task(text: &str) -> bool {
    is_spawnable_command_text(text) || contains_music_link_hint(text)
}

pub(super) fn should_log_command(command: &str) -> bool {
    matches!(
        command,
        "music" | "netease" | "search" | "rmcache" | "clearallcache"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageTaskRoute {
    Command,
    MusicLink,
}

pub(super) fn classify_message_task(text: &str) -> Option<MessageTaskRoute> {
    if is_command_text(text) {
        Some(MessageTaskRoute::Command)
    } else if contains_music_link_hint(text) {
        Some(MessageTaskRoute::MusicLink)
    } else {
        None
    }
}

pub(super) fn is_clearallcache_confirm(args: Option<&str>) -> bool {
    matches!(args.map(str::trim), Some("confirm"))
}

pub(super) fn rmcache_usage_prompt() -> &'static str {
    "请输入要删除缓存的歌曲ID\n\n用法: <code>/rmcache &lt;音乐ID&gt;</code>"
}

pub(super) fn clearallcache_confirmation_prompt() -> &'static str {
    "⚠️ 确认要清除所有缓存吗？\n\n这将删除数据库中的所有歌曲缓存记录。\n\n请在30秒内再次发送 <code>/clearallcache confirm</code> 确认操作。"
}

pub(super) async fn send_reply_message(
    bot: &Bot,
    msg: &Message,
    text: impl Into<String>,
) -> ResponseResult<Message> {
    bot.send_message(msg.chat.id, text)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await
}

pub(super) async fn send_reply_text(
    bot: &Bot,
    msg: &Message,
    text: impl Into<String>,
) -> ResponseResult<()> {
    send_reply_message(bot, msg, text).await?;
    Ok(())
}

pub(super) async fn require_command_args_or_reply(
    bot: &Bot,
    msg: &Message,
    args: Option<String>,
    prompt: &str,
) -> ResponseResult<Option<String>> {
    let args = args.unwrap_or_default();
    if args.is_empty() {
        send_reply_text(bot, msg, prompt).await?;
        Ok(None)
    } else {
        Ok(Some(args))
    }
}

pub(super) async fn send_reply_html(
    bot: &Bot,
    msg: &Message,
    text: impl Into<String>,
) -> ResponseResult<()> {
    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;
    Ok(())
}

pub(super) async fn edit_status_message_resilient(
    bot: &Bot,
    chat_id: teloxide::types::ChatId,
    message_id: teloxide::types::MessageId,
    text: impl Into<String>,
) {
    let text = text.into();
    if let Err(e) = bot
        .edit_message_text(chat_id, message_id, text.clone())
        .await
    {
        let sanitized = sanitize_sensitive_text(&e.to_string());
        if let Some(delay_secs) = extract_retry_after_seconds(&sanitized) {
            let bot = bot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs.saturating_add(1)))
                    .await;
                if let Err(retry_err) = bot.edit_message_text(chat_id, message_id, text).await {
                    tracing::debug!(
                        "Status message edit retry failed: {}",
                        sanitize_sensitive_text(&retry_err.to_string())
                    );
                }
            });
        } else {
            tracing::debug!("Status message edit failed: {}", sanitized);
        }
    }
}

pub(super) async fn delete_status_message_resilient(
    bot: &Bot,
    chat_id: teloxide::types::ChatId,
    message_id: teloxide::types::MessageId,
) {
    if let Err(e) = bot.delete_message(chat_id, message_id).await {
        let sanitized = sanitize_sensitive_text(&e.to_string());
        if let Some(delay_secs) = extract_retry_after_seconds(&sanitized) {
            let bot = bot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs.saturating_add(1)))
                    .await;
                if let Err(retry_err) = bot.delete_message(chat_id, message_id).await {
                    tracing::debug!(
                        "Status message delete retry failed: {}",
                        sanitize_sensitive_text(&retry_err.to_string())
                    );
                }
            });
        } else {
            tracing::debug!("Status message delete failed: {}", sanitized);
        }
    }
}

pub(super) fn message_task_limit(max_concurrent_downloads: u32) -> usize {
    (max_concurrent_downloads as usize)
        .saturating_mul(4)
        .clamp(8, 256)
}

pub(super) fn exceeds_batch_download_limit(
    track_count: usize,
    max_batch_download_tracks: u32,
) -> bool {
    track_count > max_batch_download_tracks.max(1) as usize
}

pub(super) fn upload_task_limit(max_concurrent_uploads: u32) -> usize {
    (max_concurrent_uploads as usize).clamp(1, 64)
}

pub(super) fn should_refresh_upload_client(
    upload_state: &UploadClientState,
    reuse_limit: u32,
) -> bool {
    upload_state.bot.is_none() || (reuse_limit != 0 && upload_state.reuse_count >= reuse_limit)
}

pub(super) fn collect_maintenance_signals(
    counters: &MaintenanceCounters,
    config: &Config,
) -> Vec<MaintenanceSignal> {
    let mut signals = Vec::with_capacity(3);
    for (counter, interval, signal) in [
        (
            &counters.db_analyze,
            config.db_analyze_interval_requests,
            MaintenanceSignal::AnalyzeDb,
        ),
        (
            &counters.memory_release,
            config.memory_release_interval_requests,
            MaintenanceSignal::ReleaseMemory,
        ),
        (
            &counters.api_cache_prune,
            CACHE_PRUNE_INTERVAL_REQUESTS,
            MaintenanceSignal::PruneApiCache,
        ),
    ] {
        if MaintenanceCounters::should_run(counter, interval) {
            signals.push(signal);
        }
    }

    signals
}

pub(super) async fn join_futures<F1, F2, T1, T2, E>(
    f1: F1,
    f2: F2,
) -> (std::result::Result<T1, E>, std::result::Result<T2, E>)
where
    F1: std::future::Future<Output = std::result::Result<T1, E>>,
    F2: std::future::Future<Output = std::result::Result<T2, E>>,
{
    tokio::join!(f1, f2)
}

pub(super) async fn acquire_download_leader(
    inflight: &Arc<InflightDownloads>,
    music_id: u64,
) -> Option<InflightLeaderGuard> {
    match inflight.begin(music_id) {
        InflightClaim::Leader(guard) => Some(guard),
        InflightClaim::Follower(entry) => {
            entry.wait().await;
            None
        }
    }
}

pub(super) async fn maintenance_worker(
    mut rx: tokio::sync::mpsc::Receiver<MaintenanceSignal>,
    database: Database,
    music_api: Arc<MusicApi>,
) {
    while let Some(signal) = rx.recv().await {
        match signal {
            MaintenanceSignal::AnalyzeDb => {
                if let Err(e) = database.optimize_planner().await {
                    tracing::warn!("Database planner optimize failed: {}", e);
                }
            }
            MaintenanceSignal::ReleaseMemory => {
                if let Err(e) = tokio::task::spawn_blocking(|| {
                    crate::memory::force_memory_release();
                    crate::memory::log_memory_stats();
                })
                .await
                {
                    tracing::warn!("Memory release background task failed: {}", e);
                }
            }
            MaintenanceSignal::PruneApiCache => {
                let stats = music_api.prune_expired_cache_entries();
                if stats.total_removed() > 0 {
                    tracing::debug!(
                        "Pruned API cache entries: detail={}, url={}, lyric={}",
                        stats.song_detail_removed,
                        stats.song_url_removed,
                        stats.song_lyric_removed
                    );
                }
            }
        }
    }
}

pub(super) const PERF_STAGE_SELECT_URL: &str = "select_url";
pub(super) const PERF_STAGE_PRE_UPLOAD_PATH: &str = "pre_upload_path";

pub(super) fn log_perf(label: &str, duration: std::time::Duration) {
    tracing::debug!("[{label}] {}ms", duration.as_millis());
    tracing::debug!("PERF_RAW|stage={label}|elapsed_ms={}", duration.as_millis());
}

#[cfg(test)]
pub(super) fn format_perf(label: &str, duration: std::time::Duration) -> String {
    format!("[{label}] {}ms", duration.as_millis())
}

pub(super) fn upload_log_enabled(config: &Config, level: UploadLogLevel) -> bool {
    config.upload_log_level.allows(level)
}

pub(super) fn should_set_upload_pool_idle_timeout(secs: u64) -> bool {
    secs > 0
}

pub(super) fn download_chunk_bytes(config: &Config) -> usize {
    config
        .download_chunk_size_kb
        .saturating_mul(1024)
        .max(MIN_DOWNLOAD_CHUNK_BYTES)
}

pub(super) fn append_search_result_line(
    results: &mut String,
    index: usize,
    song_name: &str,
    artists: &str,
) {
    use std::fmt::Write;

    if let Err(e) = writeln!(results, "{index}.「{song_name}」 - {artists}") {
        tracing::error!("Failed to format search result line: {}", e);
    }
}

pub(super) fn resource_availability_status(
    download_enabled: bool,
    is_available: bool,
) -> &'static str {
    match (download_enabled, is_available) {
        (false, _) => "Skipped",
        (true, true) => "Available",
        (true, false) => "None",
    }
}

pub(super) async fn cleanup_audio_buffer(buffer: AudioBuffer) {
    if let Err(e) = buffer.cleanup().await {
        tracing::warn!("Audio buffer cleanup failed: {}", e);
    }
}

pub(super) async fn cleanup_thumbnail_buffer(buffer: Option<ThumbnailBuffer>) {
    if let Some(thumbnail) = buffer
        && let Err(e) = thumbnail.cleanup().await
    {
        tracing::warn!("Thumbnail cleanup failed: {}", e);
    }
}

pub(super) fn get_upload_bot(upload_state: &UploadClientState) -> Result<Bot> {
    if let Some(bot) = upload_state.bot.clone() {
        Ok(bot)
    } else {
        tracing::error!("Upload bot not initialized");
        Err(BotError::Other(anyhow::anyhow!(
            "upload bot not initialized"
        )))
    }
}

pub(super) struct UploadBotBundle {
    pub(super) bot: Bot,
    pub(super) raw_client: reqwest::Client,
    /// Full API base URL including bot token, e.g. "http://host:port/bot<TOKEN>/"
    pub(super) api_base_url: String,
}

/// Streaming chunk size for raw uploads (256 KiB).
/// Matches the benchmark script's chunk size that achieves ~14 MB/s.
pub(super) const RAW_UPLOAD_CHUNK_SIZE: usize = 256 * 1024;

/// Parameters for raw Telegram file upload.
pub(super) struct RawUploadParams<'a> {
    pub(super) chat_id: i64,
    pub(super) caption: &'a str,
    pub(super) reply_to_message_id: i32,
    pub(super) reply_markup_json: Option<String>,
    /// sendAudio-specific fields
    pub(super) title: Option<&'a str>,
    pub(super) performer: Option<&'a str>,
    pub(super) duration: Option<u32>,
    /// Thumbnail data (already in memory as bytes)
    pub(super) thumbnail: Option<&'a ThumbnailBuffer>,
}

/// Parameters for raw Telegram document upload.
pub(super) struct RawDocumentParams<'a> {
    pub(super) chat_id: i64,
    pub(super) caption: Option<&'a str>,
    pub(super) reply_to_message_id: i32,
    pub(super) reply_markup_json: Option<String>,
}
