async fn apply_tags_in_blocking(
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

fn create_music_keyboard(music_id: u64, song_name: &str, artists: &str) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    match build_music_url("https://music.163.com", music_id) {
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

    rows.push(vec![InlineKeyboardButton::switch_inline_query(
        "分享给朋友",
        format!("https://music.163.com/song?id={music_id}"),
    )]);

    InlineKeyboardMarkup::new(rows)
}

fn build_music_url(
    base_url: &str,
    music_id: u64,
) -> std::result::Result<reqwest::Url, url::ParseError> {
    let mut url = reqwest::Url::parse(base_url)?;
    url.set_path("song");
    url.set_query(Some(&format!("id={music_id}")));
    Ok(url)
}

fn parse_api_url(api_url: &str) -> std::result::Result<reqwest::Url, url::ParseError> {
    reqwest::Url::parse(api_url)
}

fn is_admin(msg: &Message, config: &Config) -> bool {
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);
    config.bot_admin.contains(&user_id)
}

async fn ensure_admin(bot: &Bot, msg: &Message, config: &Config) -> ResponseResult<bool> {
    if is_admin(msg, config) {
        Ok(true)
    } else {
        send_reply_text(bot, msg, "❌ 该命令仅限管理员使用").await?;
        Ok(false)
    }
}

fn is_official_telegram_api(api_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(api_url) else {
        return false;
    };

    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.telegram.org"))
}

async fn local_file_uri_from_path(path: &std::path::Path) -> Option<String> {
    let canonical = tokio::fs::canonicalize(path).await.ok()?;
    url::Url::from_file_path(canonical)
        .ok()
        .map(|url| url.to_string())
}

async fn maybe_local_file_uri(
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
enum UploadFileTarget {
    LocalUri(String),
    Multipart,
}

async fn select_local_upload_target(
    config: &Config,
    is_official_api: bool,
    path: &std::path::Path,
) -> UploadFileTarget {
    maybe_local_file_uri(config, is_official_api, path)
        .await
        .map_or(UploadFileTarget::Multipart, UploadFileTarget::LocalUri)
}

fn url_bitrate_candidates(has_music_u: bool) -> &'static [u64] {
    if has_music_u {
        &[999_000, 320_000, 128_000]
    } else {
        &[320_000, 128_000]
    }
}

fn should_remove_song_cache_after_partial_failure(cover_retry_exhausted: bool) -> bool {
    cover_retry_exhausted
}

const MESSAGE_TASK_LINK_HINTS: [&str; 3] = ["music.163.com", "163cn.tv", "163cn.link"];
const MUSIC_ID_EXTRACT_FAILED_TEXT: &str = "无法从链接中提取音乐ID";

fn contains_music_link_hint(text: &str) -> bool {
    MESSAGE_TASK_LINK_HINTS
        .iter()
        .any(|hint| text.contains(hint))
}

fn is_spawnable_command_text(text: &str) -> bool {
    text.starts_with('/')
}

fn is_command_text(text: &str) -> bool {
    text.starts_with('/')
}

fn should_spawn_message_task(text: &str) -> bool {
    is_spawnable_command_text(text) || contains_music_link_hint(text)
}

fn should_log_command(command: &str) -> bool {
    matches!(
        command,
        "music" | "netease" | "search" | "rmcache" | "clearallcache"
    )
}

fn is_clearallcache_confirm(args: Option<&str>) -> bool {
    matches!(args.map(str::trim), Some("confirm"))
}

fn rmcache_usage_prompt() -> &'static str {
    "请输入要删除缓存的歌曲ID\n\n用法: <code>/rmcache &lt;音乐ID&gt;</code>"
}

fn clearallcache_confirmation_prompt() -> &'static str {
    "⚠️ 确认要清除所有缓存吗？\n\n这将删除数据库中的所有歌曲缓存记录。\n\n请在30秒内再次发送 <code>/clearallcache confirm</code> 确认操作。"
}

async fn send_reply_text(bot: &Bot, msg: &Message, text: impl Into<String>) -> ResponseResult<()> {
    bot.send_message(msg.chat.id, text)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;
    Ok(())
}

async fn send_reply_html(bot: &Bot, msg: &Message, text: impl Into<String>) -> ResponseResult<()> {
    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;
    Ok(())
}

fn message_task_limit(max_concurrent_downloads: u32) -> usize {
    (max_concurrent_downloads as usize)
        .saturating_mul(4)
        .clamp(8, 256)
}

fn exceeds_batch_download_limit(track_count: usize, max_batch_download_tracks: u32) -> bool {
    track_count > max_batch_download_tracks.max(1) as usize
}

fn upload_task_limit(max_concurrent_uploads: u32) -> usize {
    (max_concurrent_uploads as usize).clamp(1, 64)
}

fn should_refresh_upload_client(upload_state: &UploadClientState, reuse_limit: u32) -> bool {
    upload_state.bot.is_none() || (reuse_limit != 0 && upload_state.reuse_count >= reuse_limit)
}

fn collect_maintenance_signals(
    counters: &MaintenanceCounters,
    config: &Config,
) -> Vec<MaintenanceSignal> {
    let mut signals = Vec::with_capacity(3);
    for (counter, interval, signal) in [
        (
            &counters.db_analyze_requests,
            config.db_analyze_interval_requests,
            MaintenanceSignal::AnalyzeDb,
        ),
        (
            &counters.memory_release_requests,
            config.memory_release_interval_requests,
            MaintenanceSignal::ReleaseMemory,
        ),
        (
            &counters.api_cache_prune_requests,
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

async fn join_futures<F1, F2, T1, T2, E>(
    f1: F1,
    f2: F2,
) -> (std::result::Result<T1, E>, std::result::Result<T2, E>)
where
    F1: std::future::Future<Output = std::result::Result<T1, E>>,
    F2: std::future::Future<Output = std::result::Result<T2, E>>,
{
    tokio::join!(f1, f2)
}

async fn acquire_download_leader(
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

async fn maintenance_worker(
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

const PERF_STAGE_SELECT_URL: &str = "select_url";
const PERF_STAGE_PRE_UPLOAD_PATH: &str = "pre_upload_path";

fn log_perf(label: &str, duration: std::time::Duration) {
    tracing::debug!("[{label}] {}ms", duration.as_millis());
    tracing::debug!("PERF_RAW|stage={label}|elapsed_ms={}", duration.as_millis());
}

#[cfg(test)]
fn format_perf(label: &str, duration: std::time::Duration) -> String {
    format!("[{label}] {}ms", duration.as_millis())
}

fn upload_log_enabled(config: &Config, level: UploadLogLevel) -> bool {
    config.upload_log_level.allows(level)
}

fn should_set_upload_pool_idle_timeout(secs: u64) -> bool {
    secs > 0
}

fn download_chunk_bytes(config: &Config) -> usize {
    config
        .download_chunk_size_kb
        .saturating_mul(1024)
        .max(MIN_DOWNLOAD_CHUNK_BYTES)
}

fn append_search_result_line(results: &mut String, index: usize, song_name: &str, artists: &str) {
    use std::fmt::Write;

    if let Err(e) = writeln!(results, "{index}.「{song_name}」 - {artists}") {
        tracing::error!("Failed to format search result line: {}", e);
    }
}

fn resource_availability_status(download_enabled: bool, is_available: bool) -> &'static str {
    match (download_enabled, is_available) {
        (false, _) => "Skipped",
        (true, true) => "Available",
        (true, false) => "None",
    }
}

async fn cleanup_audio_buffer(buffer: AudioBuffer) {
    if let Err(e) = buffer.cleanup().await {
        tracing::warn!("Audio buffer cleanup failed: {}", e);
    }
}

async fn cleanup_thumbnail_buffer(buffer: Option<ThumbnailBuffer>) {
    if let Some(thumbnail) = buffer
        && let Err(e) = thumbnail.cleanup().await
    {
        tracing::warn!("Thumbnail cleanup failed: {}", e);
    }
}

fn get_upload_bot(upload_state: &UploadClientState) -> Result<Bot> {
    if let Some(bot) = upload_state.bot.clone() {
        Ok(bot)
    } else {
        tracing::error!("Upload bot not initialized");
        Err(BotError::Other(anyhow::anyhow!(
            "upload bot not initialized"
        )))
    }
}

struct UploadBotBundle {
    bot: Bot,
    raw_client: reqwest::Client,
    /// Full API base URL including bot token, e.g. "http://host:port/bot<TOKEN>/"
    api_base_url: String,
}

/// Streaming chunk size for raw uploads (256 KiB).
/// Matches the benchmark script's chunk size that achieves ~14 MB/s.
const RAW_UPLOAD_CHUNK_SIZE: usize = 256 * 1024;

/// Parameters for raw Telegram file upload.
struct RawUploadParams<'a> {
    chat_id: i64,
    caption: &'a str,
    reply_to_message_id: i32,
    reply_markup_json: Option<String>,
    /// sendAudio-specific fields
    title: Option<&'a str>,
    performer: Option<&'a str>,
    duration: Option<u32>,
    /// Thumbnail data (already in memory as bytes)
    thumbnail: Option<&'a ThumbnailBuffer>,
}

/// Parameters for raw Telegram document upload.
struct RawDocumentParams<'a> {
    chat_id: i64,
    caption: Option<&'a str>,
    reply_to_message_id: i32,
    reply_markup_json: Option<String>,
}

/// Upload an in-memory document via raw reqwest multipart.
async fn raw_send_document_bytes(
    client: &reqwest::Client,
    api_base_url: &str,
    filename: &str,
    content: Bytes,
    params: &RawDocumentParams<'_>,
) -> Result<serde_json::Value> {
    let len = content.len() as u64;
    let mut form = reqwest::multipart::Form::new().text("chat_id", params.chat_id.to_string());

    if let Some(caption) = params.caption {
        form = form.text("caption", caption.to_owned());
    }

    let file_part = reqwest::multipart::Part::stream_with_length(content, len)
        .file_name(filename.to_owned())
        .mime_str("text/plain; charset=utf-8")?;
    form = form.part("document", file_part);

    let reply_params = format!(r#"{{"message_id":{}}}"#, params.reply_to_message_id);
    form = form.text("reply_parameters", reply_params);

    if let Some(ref markup_json) = params.reply_markup_json {
        form = form.text("reply_markup", markup_json.clone());
    }

    let url = format!("{api_base_url}sendDocument");
    let resp = client
        .post(&url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| BotError::Other(anyhow::anyhow!("Raw upload request failed: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| BotError::Other(anyhow::anyhow!("Failed to read upload response: {e}")))?;
    parse_telegram_api_response(&body, status, "sendDocument")
}

