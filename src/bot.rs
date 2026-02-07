use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use futures_util::StreamExt;
use teloxide::RequestError;
use teloxide::prelude::*;
use teloxide::sugar::request::RequestLinkPreviewExt;
use teloxide::types::{
    CallbackQuery, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InlineQuery,
    InlineQueryResult, InlineQueryResultArticle, InputFile, InputMessageContent,
    InputMessageContentText, MaybeInaccessibleMessage, Message, MessageKind, ParseMode,
    ReplyParameters,
};
use tokio::sync::{Mutex, Notify};

use crate::audio_buffer::{AudioBuffer, ThumbnailBuffer};
use crate::config::{Config, CoverMode, UploadLogLevel};
use crate::database::{Database, SongInfo};
use crate::error::{BotError, Result};
use crate::music_api::{MusicApi, format_artists};
use crate::utils::{
    clean_filename, ensure_dir, extract_first_url, parse_music_id, throughput_mbps, update_peak,
};

pub struct BotState {
    pub config: Config,
    pub database: Database,
    pub music_api: MusicApi,
    inflight_downloads: Arc<InflightDownloads>,
    pub download_semaphore: Arc<tokio::sync::Semaphore>,
    pub upload_semaphore: Arc<tokio::sync::Semaphore>,
    pub message_task_semaphore: Arc<tokio::sync::Semaphore>,
    pub maintenance_tx: tokio::sync::mpsc::UnboundedSender<MaintenanceSignal>,
    pub bot_username: String,
    pub upload_client_state: Arc<Mutex<UploadClientState>>,
    pub maintenance_counters: MaintenanceCounters,
    pub upload_counters: UploadCounters,
}

#[derive(Debug)]
pub struct UploadClientState {
    pub bot: Option<Bot>,
    pub reuse_count: u32,
}

#[derive(Debug, Default)]
pub struct UploadCounters {
    pub in_flight: AtomicU32,
    pub peak_in_flight: AtomicU32,
}

#[derive(Debug)]
pub struct MaintenanceCounters {
    pub memory_release_requests: AtomicU32,
    pub db_analyze_requests: AtomicU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceSignal {
    AnalyzeDb,
    ReleaseMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadMode {
    Audio,
    Document,
}

#[derive(Debug, Default)]
struct InflightDownloads {
    entries: std::sync::Mutex<HashMap<u64, Arc<InflightEntry>>>,
}

#[derive(Debug)]
struct InflightEntry {
    notify: Notify,
    done: AtomicBool,
}

#[derive(Debug)]
enum InflightClaim {
    Leader(InflightLeaderGuard),
    Follower(Arc<InflightEntry>),
}

#[derive(Debug)]
struct InflightLeaderGuard {
    music_id: u64,
    inflight: Arc<InflightDownloads>,
}

impl InflightDownloads {
    fn begin(self: &Arc<Self>, music_id: u64) -> InflightClaim {
        let mut entries = self.lock_entries();
        if let Some(existing) = entries.get(&music_id) {
            return InflightClaim::Follower(Arc::clone(existing));
        }

        entries.insert(music_id, Arc::new(InflightEntry::new()));
        InflightClaim::Leader(InflightLeaderGuard {
            music_id,
            inflight: Arc::clone(self),
        })
    }

    fn finish(&self, music_id: u64) {
        if let Some(entry) = self.lock_entries().remove(&music_id) {
            entry.finish();
        }
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<InflightEntry>>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl InflightEntry {
    fn new() -> Self {
        Self {
            notify: Notify::new(),
            done: AtomicBool::new(false),
        }
    }

    async fn wait(&self) {
        if self.done.load(Ordering::Acquire) {
            return;
        }

        self.notify.notified().await;
    }

    fn finish(&self) {
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

impl Drop for InflightLeaderGuard {
    fn drop(&mut self) {
        self.inflight.finish(self.music_id);
    }
}

impl MaintenanceCounters {
    fn new() -> Self {
        Self {
            memory_release_requests: AtomicU32::new(0),
            db_analyze_requests: AtomicU32::new(0),
        }
    }

    fn should_run(counter: &AtomicU32, interval: u32) -> bool {
        if interval == 0 {
            return false;
        }
        let next = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        next.is_multiple_of(interval)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CoverPolicy {
    download_original: bool,
    download_thumbnail: bool,
    embed_tags: bool,
    embed_cover: bool,
}

fn resolve_cover_policy(cover_mode: CoverMode) -> CoverPolicy {
    let download_original = matches!(cover_mode, CoverMode::Original | CoverMode::Both);
    let download_thumbnail = matches!(cover_mode, CoverMode::Thumbnail | CoverMode::Both);

    CoverPolicy {
        download_original,
        download_thumbnail,
        embed_tags: true,
        embed_cover: download_original,
    }
}

pub async fn run(config: Config) -> Result<()> {
    tracing::info!("Starting Telegram bot...");

    // Ensure cache directory exists
    ensure_dir(&config.cache_dir)?;

    // Initialize database
    let database = Database::new(&config.database).await?;
    tracing::info!("Database initialized");

    let (maintenance_tx, maintenance_rx) = tokio::sync::mpsc::unbounded_channel();
    let maintenance_database = database.clone();
    tokio::spawn(async move {
        maintenance_worker(maintenance_rx, maintenance_database).await;
    });

    // Initialize music API
    let music_api = MusicApi::new_with_config(&config);
    tracing::info!("Music API initialized");

    // Initialize bot with custom API URL support
    let bot = if !config.bot_api.is_empty() && config.bot_api != "https://api.telegram.org" {
        // 使用自定义API URL
        // API URL must be base URL without "/bot" suffix - teloxide appends "bot<TOKEN>/" automatically
        let api_url_str = format!("{}/", config.bot_api.trim_end_matches("/bot"));

        match reqwest::Url::parse(&api_url_str) {
            Ok(api_url) => {
                tracing::info!("Using custom Telegram API URL: {}", api_url);

                // Create a custom HTTP client tuned for Cloudflare compatibility (mimic Go http client)
                // pool_max_idle_per_host(2) keeps reasonable connection pool for API efficiency
                let client_builder = reqwest::Client::builder()
                    .use_rustls_tls()
                    .user_agent("Go-http-client/2.0")
                    .pool_max_idle_per_host(2)
                    .pool_idle_timeout(std::time::Duration::from_secs(60))
                    .danger_accept_invalid_certs(false)
                    .timeout(std::time::Duration::from_secs(30))
                    .no_gzip();
                let client = build_reqwest_client(client_builder)?;

                // Create bot with custom client and API URL
                let bot = Bot::with_client(&config.bot_token, client).set_api_url(api_url.clone());

                // Test the connection with timeout and better error handling
                tracing::info!("Testing custom API connection...");
                match tokio::time::timeout(std::time::Duration::from_secs(15), bot.get_me()).await {
                    Ok(Ok(_)) => {
                        tracing::info!("✅ Custom API connection successful: {}", api_url);
                        bot
                    }
                    Ok(Err(e)) => {
                        let error_msg = format!("{e}");
                        // Check if it's a CloudFlare challenge or other blocking issue
                        if error_msg.contains("Just a moment")
                            || error_msg.contains("cloudflare")
                            || error_msg.contains("challenge")
                        {
                            tracing::warn!(
                                "❌ Custom API blocked by CloudFlare protection. Falling back to official API."
                            );
                        } else {
                            tracing::warn!(
                                "❌ Custom API connection failed: {}. Falling back to official API.",
                                e
                            );
                        }
                        tracing::info!("Using fallback Telegram API URL: https://api.telegram.org");
                        Bot::new(&config.bot_token)
                    }
                    Err(_) => {
                        tracing::warn!(
                            "❌ Custom API connection timeout (15s). Falling back to official API."
                        );
                        tracing::info!("Using fallback Telegram API URL: https://api.telegram.org");
                        Bot::new(&config.bot_token)
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Invalid custom API URL '{}': {}. Using official API.",
                    config.bot_api,
                    e
                );
                tracing::info!("Using fallback Telegram API URL: https://api.telegram.org");
                Bot::new(&config.bot_token)
            }
        }
    } else {
        // 使用默认API URL，但配置连接池以提高效率
        tracing::info!("Using default Telegram API URL: https://api.telegram.org");
        let client_builder = reqwest::Client::builder()
            .use_rustls_tls()
            .pool_max_idle_per_host(2)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .timeout(std::time::Duration::from_secs(30));
        let client = build_reqwest_client(client_builder)?;
        Bot::with_client(&config.bot_token, client)
    };

    // Log the API configuration
    tracing::info!("Music API configured: {}", &config.music_api);

    let me = bot.get_me().await?;
    let bot_username = me
        .username
        .clone()
        .unwrap_or_else(|| "Music163bot".to_string());
    tracing::info!("Bot @{} started successfully!", bot_username);

    // Create bot state (needs bot username)
    let bot_state = Arc::new(BotState {
        config: config.clone(),
        database,
        music_api,
        inflight_downloads: Arc::new(InflightDownloads::default()),
        download_semaphore: Arc::new(tokio::sync::Semaphore::new(
            config.max_concurrent_downloads as usize,
        )),
        upload_semaphore: Arc::new(tokio::sync::Semaphore::new(upload_task_limit(
            config.upload_max_concurrent,
        ))),
        message_task_semaphore: Arc::new(tokio::sync::Semaphore::new(message_task_limit(
            config.max_concurrent_downloads,
        ))),
        maintenance_tx,
        bot_username,
        upload_client_state: Arc::new(Mutex::new(UploadClientState {
            bot: None,
            reuse_count: 0,
        })),
        maintenance_counters: MaintenanceCounters::new(),
        upload_counters: UploadCounters::default(),
    });

    let prewarm_state = Arc::clone(&bot_state);
    tokio::spawn(async move {
        let _ =
            run_upload_prewarm(&prewarm_state.config, || acquire_upload_bot(&prewarm_state)).await;
    });

    // Create dispatcher
    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback))
        .branch(Update::filter_inline_query().endpoint(handle_inline_query));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![bot_state])
        .default_handler(|upd| async move {
            tracing::debug!("Unhandled update: {:?}", upd);
        })
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

async fn handle_message(bot: Bot, msg: Message, state: Arc<BotState>) -> ResponseResult<()> {
    if let MessageKind::Common(common) = &msg.kind
        && let teloxide::types::MediaKind::Text(text_content) = &common.media_kind
    {
        let text = text_content.text.clone();
        if !should_spawn_message_task(&text) {
            return Ok(());
        }

        let permit = match state.message_task_semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(e) => {
                tracing::error!("Message task semaphore closed: {}", e);
                return Ok(());
            }
        };

        let bot = bot.clone();
        let msg = msg.clone();
        let state = state.clone();

        // Spawn a new task to handle the message concurrently
        // This allows multiple messages to be processed in parallel
        tokio::spawn(async move {
            let _permit = permit;

            // Handle commands
            if text.starts_with('/') {
                if let Err(e) = handle_command(&bot, &msg, &state, &text).await {
                    tracing::error!("Error handling command: {}", e);
                }
            }
            // Handle music URLs
            else if (text.contains("music.163.com")
                || text.contains("163cn.tv")
                || text.contains("163cn.link"))
                && let Err(e) = handle_music_url(&bot, &msg, &state, &text).await
            {
                tracing::error!("Error handling music URL: {}", e);
            }
        });
    }
    Ok(())
}

async fn handle_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    text: &str,
) -> ResponseResult<()> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let mut command = parts[0].trim_start_matches('/');

    // Remove bot username if present (e.g., "/start@BotName" -> "start")
    if let Some(at_pos) = command.find('@') {
        command = &command[..at_pos];
    }

    let args = if parts.len() > 1 {
        Some(parts[1..].join(" "))
    } else {
        None
    };

    // Only log music/search commands and admin commands
    match command {
        "music" | "netease" | "search" | "rmcache" | "clearallcache" => {
            tracing::info!("Command: /{} from chat {}", command, msg.chat.id);
        }
        _ => {} // Don't log about/start/status commands
    }

    match command {
        "start" => handle_start_command(bot, msg, state, args).await,
        "help" => handle_help_command(bot, msg, state).await,
        "music" | "netease" => handle_music_command(bot, msg, state, args).await,
        "search" => handle_search_command(bot, msg, state, args).await,
        "about" => handle_about_command(bot, msg, state).await,
        "lyric" => handle_lyric_command(bot, msg, state, args).await,
        "status" => handle_status_command(bot, msg, state).await,
        "rmcache" => handle_rmcache_command(bot, msg, state, args).await,
        "clearallcache" => {
            // Check if this is a confirmation
            if let Some(ref arg) = args {
                if arg.trim() == "confirm" {
                    handle_clearallcache_confirm_command(bot, msg, state).await
                } else {
                    handle_clearallcache_command(bot, msg, state).await
                }
            } else {
                handle_clearallcache_command(bot, msg, state).await
            }
        }
        _ => {
            // Unknown commands: don't respond (as requested)
            Ok(())
        }
    }
}

fn parse_start_music_id(args: Option<&str>) -> Option<u64> {
    args.and_then(|arg| arg.trim().parse::<u64>().ok())
}

fn parse_inline_query_keyword(text: &str) -> (&str, bool) {
    let trimmed = text.trim();

    if let Some(prefix) = trimmed.get(..7)
        && prefix.eq_ignore_ascii_case("search ")
    {
        let keyword = trimmed.get(7..).unwrap_or("").trim();
        (keyword, true)
    } else if let Some(prefix) = trimmed.get(..6)
        && prefix.eq_ignore_ascii_case("search")
    {
        ("", true)
    } else {
        (trimmed, false)
    }
}

async fn handle_start_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    if let Some(music_id) = parse_start_music_id(args.as_deref()) {
        return process_music(bot, msg, state, music_id).await;
    }

    let welcome_text = format!(
        "👋 欢迎使用网易云音乐机器人 <b>@{}</b>\n\n\
        我可以帮你解析网易云音乐链接、搜索音乐、获取歌词。\n\n\
        <b>主要功能：</b>\n\
        • 直接发送网易云音乐链接进行解析\n\
        • 使用 <code>/search &lt;关键词&gt;</code> 搜索音乐\n\
        • 在任何聊天中使用 <code>@{} &lt;关键词&gt;</code> 进行 Inline 搜索\n\
        • 使用 <code>/lyric &lt;关键词或ID&gt;</code> 获取歌词\n\n\
        <b>开源地址：</b> <a href=\"https://github.com/Lemonawa/music163bot-rust\">Lemonawa/music163bot-rust</a>",
        state.bot_username, state.bot_username
    );

    bot.send_message(msg.chat.id, welcome_text)
        .parse_mode(ParseMode::Html)
        .disable_link_preview(true)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    Ok(())
}

async fn handle_help_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let help_text = format!(
        "📖 <b>使用帮助</b>\n\n\
        1️⃣ <b>直接解析</b>\n\
        发送网易云音乐链接给机器人，例如：\n\
        <code>https://music.163.com/song?id=12345</code>\n\n\
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
        bot.send_message(msg.chat.id, "请输入歌曲ID或歌曲关键词")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
        return Ok(());
    }

    // Try to parse as music ID first
    if let Some(music_id) = parse_music_id(&args) {
        return process_music(bot, msg, state, music_id).await;
    }

    // If not a number, search for the song
    match state.music_api.search_songs(&args, 1).await {
        Ok(songs) => {
            if let Some(song) = songs.first() {
                process_music(bot, msg, state, song.id).await
            } else {
                bot.send_message(msg.chat.id, "未找到相关歌曲")
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .await?;
                Ok(())
            }
        }
        Err(e) => {
            bot.send_message(msg.chat.id, format!("搜索失败: {e}"))
                .reply_parameters(ReplyParameters::new(msg.id))
                .await?;
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
            let err_str = format!("{e}");
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
    if try_send_cached_song(bot, msg, state, music_id).await? {
        return Ok(());
    }

    let _singleflight_guard = loop {
        if let Some(leader_guard) =
            acquire_download_leader(&state.inflight_downloads, music_id).await
        {
            break leader_guard;
        }

        if try_send_cached_song(bot, msg, state, music_id).await? {
            return Ok(());
        }
    };

    if try_send_cached_song(bot, msg, state, music_id).await? {
        return Ok(());
    }

    // Send status message and fetch song detail+URL in parallel
    let status_init_start = std::time::Instant::now();
    let bitrate_candidates = url_bitrate_candidates(state.music_api.music_u.is_some());

    let status_fut = bot
        .send_message(msg.chat.id, "🔄 正在获取歌曲信息...")
        .reply_parameters(ReplyParameters::new(msg.id))
        .send();
    let fetch_fut = state
        .music_api
        .get_song_detail_and_best_url(music_id, &bitrate_candidates);

    let (status_result, detail_and_url_result) = tokio::join!(status_fut, fetch_fut);
    let status_msg = status_result?;
    let select_url_duration = status_init_start.elapsed();
    tracing::info!(
        "{}",
        format_perf(PERF_STAGE_SELECT_URL, select_url_duration)
    );

    let (song_detail, song_url) = match detail_and_url_result {
        Ok(result) => result,
        Err(e) => {
            bot.edit_message_text(
                msg.chat.id,
                status_msg.id,
                format!("❌ 获取歌曲信息或下载链接失败: {e}"),
            )
            .await?;
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
        &song_detail,
        &song_url,
        &status_msg,
        pre_upload_path_start,
    )
    .await
    {
        Ok(()) => {
            // Delete status message
            bot.delete_message(msg.chat.id, status_msg.id).await.ok();
        }
        Err(e) => {
            bot.edit_message_text(msg.chat.id, status_msg.id, format!("❌ 处理失败: {e}"))
                .await?;
        }
    }

    Ok(())
}

async fn download_and_send_music(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    song_detail: &crate::music_api::SongDetail,
    song_url: &crate::music_api::SongUrl,
    status_msg: &Message,
    pre_upload_path_start: std::time::Instant,
) -> Result<()> {
    let _permit = acquire_download_permit(&state.download_semaphore).await?;

    // Determine file extension
    let file_ext = if song_url.url.contains(".flac") {
        "flac"
    } else {
        "mp3"
    };

    let artists = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
    let filename = clean_filename(&format!(
        "{} - {}.{}",
        artists.replace('/', ","),
        song_detail.name,
        file_ext
    ));

    // Ensure cache directory exists
    ensure_dir(&state.config.cache_dir)?;

    let cover_mode = state.config.cover_mode;
    let cover_policy = resolve_cover_policy(cover_mode);
    let download_original = cover_policy.download_original;
    let download_thumbnail = cover_policy.download_thumbnail;

    // Start parallel downloads: audio file and album art
    let artwork_future = async {
        if let Some(ref al) = song_detail.al {
            tracing::debug!("Album info found: id={}, name={}", al.id, al.name);
            if let Some(ref pic_url) = al.pic_url {
                if pic_url.is_empty() {
                    tracing::warn!("Album art URL is empty for music_id {}", song_detail.id);
                    (None, None)
                } else {
                    tracing::info!(
                        "Starting album art download for music_id {} (mode: {:?}), pic_url: {}",
                        song_detail.id,
                        cover_mode,
                        pic_url
                    );

                    if download_original && download_thumbnail {
                        // Download original once, then derive thumbnail locally.
                        let original_data =
                            match state.music_api.download_album_art_original(pic_url).await {
                                Ok(data) => {
                                    tracing::info!(
                                        "Downloaded original album art for music_id {} ({} bytes)",
                                        song_detail.id,
                                        data.len()
                                    );
                                    Some(data)
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to download original album art for music_id {}: {}",
                                        song_detail.id,
                                        e
                                    );
                                    None
                                }
                            };

                        let thumbnail_buffer = if let Some(original_bytes) =
                            original_data.as_deref()
                        {
                            match crate::music_api::resize_album_art_to_thumbnail(original_bytes) {
                                Ok(data) => {
                                    tracing::info!(
                                        "Derived thumbnail from original for music_id {} ({} bytes)",
                                        song_detail.id,
                                        data.len()
                                    );
                                    let thumb_filename = format!(
                                        "thumb_{}_{}.jpg",
                                        song_detail.id,
                                        chrono::Utc::now().timestamp()
                                    );
                                    ThumbnailBuffer::new(
                                        &state.config,
                                        data,
                                        &state.config.cache_dir,
                                        &thumb_filename,
                                    )
                                    .await
                                    .ok()
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to derive thumbnail from original for music_id {}: {}",
                                        song_detail.id,
                                        e
                                    );
                                    match state.music_api.download_album_art_data(pic_url).await {
                                        Ok(data) => {
                                            tracing::info!(
                                                "Fallback thumbnail download for music_id {} ({} bytes)",
                                                song_detail.id,
                                                data.len()
                                            );
                                            let thumb_filename = format!(
                                                "thumb_{}_{}.jpg",
                                                song_detail.id,
                                                chrono::Utc::now().timestamp()
                                            );
                                            ThumbnailBuffer::new(
                                                &state.config,
                                                data,
                                                &state.config.cache_dir,
                                                &thumb_filename,
                                            )
                                            .await
                                            .ok()
                                        }
                                        Err(err) => {
                                            tracing::warn!(
                                                "Failed to download thumbnail for music_id {}: {}",
                                                song_detail.id,
                                                err
                                            );
                                            None
                                        }
                                    }
                                }
                            }
                        } else {
                            match state.music_api.download_album_art_data(pic_url).await {
                                Ok(data) => {
                                    tracing::info!(
                                        "Fallback thumbnail download for music_id {} ({} bytes)",
                                        song_detail.id,
                                        data.len()
                                    );
                                    let thumb_filename = format!(
                                        "thumb_{}_{}.jpg",
                                        song_detail.id,
                                        chrono::Utc::now().timestamp()
                                    );
                                    ThumbnailBuffer::new(
                                        &state.config,
                                        data,
                                        &state.config.cache_dir,
                                        &thumb_filename,
                                    )
                                    .await
                                    .ok()
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to download thumbnail for music_id {}: {}",
                                        song_detail.id,
                                        e
                                    );
                                    None
                                }
                            }
                        };

                        (original_data, thumbnail_buffer)
                    } else {
                        let original_data = if download_original {
                            match state.music_api.download_album_art_original(pic_url).await {
                                Ok(data) => {
                                    tracing::info!(
                                        "Downloaded original album art for music_id {} ({} bytes)",
                                        song_detail.id,
                                        data.len()
                                    );
                                    Some(data)
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to download original album art for music_id {}: {}",
                                        song_detail.id,
                                        e
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        let thumbnail_buffer = if download_thumbnail {
                            match state.music_api.download_album_art_data(pic_url).await {
                                Ok(data) => {
                                    tracing::info!(
                                        "Downloaded thumbnail for music_id {} ({} bytes)",
                                        song_detail.id,
                                        data.len()
                                    );
                                    let thumb_filename = format!(
                                        "thumb_{}_{}.jpg",
                                        song_detail.id,
                                        chrono::Utc::now().timestamp()
                                    );
                                    ThumbnailBuffer::new(
                                        &state.config,
                                        data,
                                        &state.config.cache_dir,
                                        &thumb_filename,
                                    )
                                    .await
                                    .ok()
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to download thumbnail for music_id {}: {}",
                                        song_detail.id,
                                        e
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        (original_data, thumbnail_buffer)
                    }
                }
            } else {
                tracing::warn!("No pic_url found in album for music_id {}", song_detail.id);
                (None, None)
            }
        } else {
            tracing::warn!("No album info found for music_id {}", song_detail.id);
            (None, None)
        }
    };

    // Download audio file using smart storage
    let audio_future = async {
        let download_start = std::time::Instant::now();
        let response = state.music_api.download_file(&song_url.url).await?;

        // Check response status
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP {}", response.status()));
        }

        // Check content length
        let content_length = response.content_length().unwrap_or(0);
        if content_length == 0 {
            return Err(anyhow::anyhow!("Empty file or unable to get file size"));
        }

        // Create audio buffer based on storage mode configuration
        let mut audio_buffer = AudioBuffer::new(
            &state.config,
            content_length,
            filename.clone(),
            file_ext,
            &state.config.cache_dir,
        )
        .await?;

        let mut stream = response.bytes_stream();
        let mut downloaded = 0u64;
        let chunk_size = state.config.download_chunk_size_kb * 1024;
        let mut buffer = Vec::with_capacity(chunk_size);

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            downloaded += chunk.len() as u64;

            if buffer.len() + chunk.len() > chunk_size {
                if !buffer.is_empty() {
                    audio_buffer.write_chunk(&buffer).await?;
                    buffer.clear();
                }
                if chunk.len() >= chunk_size {
                    audio_buffer.write_chunk(&chunk).await?;
                } else {
                    buffer.extend_from_slice(&chunk);
                }
            } else {
                buffer.extend_from_slice(&chunk);
            }
        }
        if !buffer.is_empty() {
            audio_buffer.write_chunk(&buffer).await?;
        }
        audio_buffer.finish().await?;
        let download_duration = download_start.elapsed();
        let download_mbps = throughput_mbps(downloaded, download_duration);
        tracing::info!(
            "Audio download completed in {:.2}s ({:.2} MB/s)",
            download_duration.as_secs_f64(),
            download_mbps
        );
        tracing::info!("{}", format_perf("download_audio", download_duration));

        Ok::<(AudioBuffer, u64), anyhow::Error>((audio_buffer, downloaded))
    };

    // Execute both downloads in parallel
    let (downloaded_result, (original_artwork_data, thumbnail_buffer)) =
        tokio::join!(audio_future, artwork_future);
    let (mut audio_buffer, downloaded) = downloaded_result?;

    tracing::info!(
        "Audio download completed: {} bytes (mode: {})",
        downloaded,
        if audio_buffer.is_memory() {
            "memory"
        } else {
            "disk"
        }
    );
    let original_status = if download_original {
        if original_artwork_data.is_some() {
            "Available"
        } else {
            "None"
        }
    } else {
        "Skipped"
    };
    let thumbnail_status = if download_thumbnail {
        if thumbnail_buffer.is_some() {
            "Available"
        } else {
            "None"
        }
    } else {
        "Skipped"
    };
    tracing::info!(
        "Cover download result - Original: {}, Thumbnail: {}",
        original_status,
        thumbnail_status
    );

    // Validate file size using downloaded byte count
    if downloaded == 0 {
        audio_buffer.cleanup().await.ok();
        bot.edit_message_text(msg.chat.id, status_msg.id, "下载失败: 文件为空")
            .await?;
        return Ok(());
    }

    if downloaded < 1024 {
        audio_buffer.cleanup().await.ok();
        bot.edit_message_text(
            msg.chat.id,
            status_msg.id,
            format!("下载失败: 文件太小({downloaded} bytes)"),
        )
        .await?;
        return Ok(());
    }

    tracing::info!("File validation passed: {} bytes", downloaded);

    // 封面处理：使用原始高分辨率图片嵌入文件，缩略图用于Telegram显示
    let tags_start = std::time::Instant::now();
    tracing::info!("Processing tags for {} format", file_ext);
    audio_buffer = apply_tags_in_blocking(
        audio_buffer,
        file_ext.to_string(),
        song_detail.clone(),
        original_artwork_data,
        cover_policy.embed_cover,
    )
    .await?;

    tracing::info!("{}", format_perf("process_tags", tags_start.elapsed()));

    // Get file size for database and logging (async to avoid blocking)
    let file_size = audio_buffer.size().await;
    let audio_file_size = file_size as i64;
    let duration_sec = (song_detail.dt.unwrap_or(0) / 1000) as i64;

    // Calculate actual bitrate from file size and duration
    // API's song_url.br is often theoretical (e.g., 1411kbps for FLAC) but
    // actual file may be compressed (e.g., 960kbps). Use real calculated value.
    let actual_bitrate_bps = if duration_sec > 0 {
        (8 * audio_file_size) / duration_sec
    } else {
        // Fallback to API value if duration is missing
        song_url.br as i64
    };

    tracing::info!(
        "Bitrate - API: {} bps, Calculated from file: {} bps (duration: {}s)",
        song_url.br,
        actual_bitrate_bps,
        duration_sec
    );

    // Create song info for database
    let mut song_info = SongInfo {
        music_id: song_detail.id as i64,
        song_name: song_detail.name.clone(),
        song_artists: artists,
        song_album: song_detail
            .al
            .as_ref()
            .map_or_else(|| "Unknown Album".to_string(), |al| al.name.clone()),
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
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        ..Default::default()
    };

    // Log final thumbnail status
    tracing::info!("Final thumbnail status: {}", thumbnail_status);

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

    let keyboard = create_music_keyboard(
        song_detail.id,
        &song_info.song_name,
        &song_info.song_artists,
    );

    if file_size == 0 {
        audio_buffer.cleanup().await.ok();
        if let Some(thumb_buf) = thumbnail_buffer {
            thumb_buf.cleanup().await.ok();
        }
        return Err(anyhow::anyhow!("Audio file is empty after processing").into());
    }

    tracing::info!(
        "Prepared audio: {} ({:.2} MB, mode: {})",
        audio_buffer.filename(),
        file_size as f64 / 1024.0 / 1024.0,
        if audio_buffer.is_memory() {
            "memory"
        } else {
            "disk"
        }
    );

    // Bound upload concurrency to keep tail latency and memory stable under burst traffic.
    let _upload_permit = acquire_upload_permit(&state.upload_semaphore).await?;

    // Acquire upload bot with minimal lock contention.
    let upload_bot = acquire_upload_bot(state).await?;

    tracing::info!(
        "{}",
        format_perf(PERF_STAGE_PRE_UPLOAD_PATH, pre_upload_path_start.elapsed())
    );

    // Send audio file with enhanced error handling and proper MIME type
    tracing::info!(
        "Sending audio file: {} ({:.2} MB)",
        audio_buffer.filename(),
        file_size as f64 / 1024.0 / 1024.0
    );

    // Simple approach: try sending as audio first, fallback to document if needed
    let is_flac = file_ext == "flac";

    tracing::info!("File format: {}", if is_flac { "FLAC" } else { "MP3" });

    // Try sending as audio first, then fallback to document if audio upload fails.
    let in_flight = state
        .upload_counters
        .in_flight
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    let peak_in_flight = update_peak(&state.upload_counters.peak_in_flight, in_flight);
    let upload_start = std::time::Instant::now();
    let mut mode = UploadMode::Audio;
    let mut sent_message = None;
    let mut last_error = None;

    loop {
        let attempt_result = match mode {
            UploadMode::Audio => {
                let mut audio_req = upload_bot
                    .send_audio(msg.chat.id, audio_buffer.to_input_file())
                    .caption(&caption)
                    .title(&song_info.song_name)
                    .performer(&song_info.song_artists)
                    .duration(song_info.duration as u32)
                    .reply_markup(keyboard.clone())
                    .reply_parameters(ReplyParameters::new(msg.id));

                if let Some(thumb_buf) = thumbnail_buffer.as_ref() {
                    match thumb_buf.to_input_file() {
                        Ok(thumb_input) => {
                            audio_req = audio_req.thumbnail(thumb_input);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to prepare thumbnail input: {}", e);
                        }
                    }
                }

                audio_req.await
            }
            UploadMode::Document => {
                upload_bot
                    .send_document(msg.chat.id, audio_buffer.to_input_file())
                    .caption(&caption)
                    .reply_markup(keyboard.clone())
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .await
            }
        };

        match attempt_result {
            Ok(sent_msg) => {
                sent_message = Some(sent_msg);
                break;
            }
            Err(e) => {
                tracing::warn!("Upload attempt in mode {:?} failed: {}", mode, e);
                last_error = Some(e);
                if let Some(next_mode) = next_upload_mode(mode, false) {
                    tracing::warn!("Audio send failed, retrying as document");
                    mode = next_mode;
                    continue;
                }
                break;
            }
        }
    }

    let upload_duration = upload_start.elapsed();
    let in_flight_after = state
        .upload_counters
        .in_flight
        .fetch_sub(1, Ordering::Relaxed)
        - 1;
    tracing::info!("{}", format_perf("upload_audio", upload_duration));

    if let Some(sent_msg) = sent_message {
        let upload_mbps = throughput_mbps(file_size, upload_duration);
        tracing::info!(
            "Upload completed in {:.2}s ({:.2} MB/s, inflight: {}, peak: {})",
            upload_duration.as_secs_f64(),
            upload_mbps,
            in_flight_after,
            peak_in_flight
        );
        tracing::info!("Upload completed in mode: {:?}", mode);
        if mode == UploadMode::Audio {
            tracing::info!(
                "Successfully sent as audio: {}",
                if is_flac { "FLAC" } else { "MP3" }
            );
        } else {
            tracing::info!("Fallback document upload succeeded");
        }

        // Extract file_id from sent message (audio/document)
        if let MessageKind::Common(common) = &sent_msg.kind {
            match &common.media_kind {
                teloxide::types::MediaKind::Audio(audio) => {
                    song_info.file_id = Some(audio.audio.file.id.to_string());
                }
                teloxide::types::MediaKind::Document(document) => {
                    song_info.file_id = Some(document.document.file.id.to_string());
                }
                _ => {}
            }
        }
    } else {
        let Some(e) = last_error else {
            tracing::error!("Upload failed without error details");
            audio_buffer.cleanup().await.ok();
            if let Some(thumb_buf) = thumbnail_buffer {
                thumb_buf.cleanup().await.ok();
            }
            return Err(BotError::Other(anyhow::anyhow!(
                "upload failed without error details"
            )));
        };

        let upload_mbps = throughput_mbps(file_size, upload_duration);
        tracing::warn!(
            "Upload failed after {:.2}s ({:.2} MB/s, inflight: {}, peak: {})",
            upload_duration.as_secs_f64(),
            upload_mbps,
            in_flight_after,
            peak_in_flight
        );
        tracing::warn!("Upload failed: {}", e);

        bot.edit_message_text(msg.chat.id, status_msg.id, format!("发送失败: {e}"))
            .await
            .ok();

        audio_buffer.cleanup().await.ok();
        if let Some(thumb_buf) = thumbnail_buffer {
            thumb_buf.cleanup().await.ok();
        }

        return Err(e.into());
    }

    audio_buffer.cleanup().await.ok();
    if let Some(thumb_buf) = thumbnail_buffer {
        thumb_buf.cleanup().await.ok();
    }

    // Save to database and update query statistics
    state.database.save_song_info(&song_info).await?;
    for signal in collect_maintenance_signals(&state.maintenance_counters, &state.config) {
        if state.maintenance_tx.send(signal).is_err() {
            tracing::warn!("Maintenance worker unavailable; skipping signal");
        }
    }

    // Delete status message
    bot.delete_message(msg.chat.id, status_msg.id).await.ok();

    Ok(())
}

async fn apply_tags_in_blocking(
    mut audio_buffer: AudioBuffer,
    file_ext: String,
    song_detail: crate::music_api::SongDetail,
    artwork_data: Option<Vec<u8>>,
    embed_cover: bool,
) -> Result<AudioBuffer> {
    tokio::task::spawn_blocking(move || {
        let embed_artwork = if embed_cover {
            artwork_data.as_deref()
        } else {
            None
        };

        match file_ext.as_str() {
            "mp3" => {
                let cover_label = if embed_cover { "original" } else { "none" };
                tracing::info!("Adding ID3 tags to MP3 (cover: {})", cover_label);
                match audio_buffer.add_id3_tags(&song_detail, embed_artwork) {
                    Ok(()) => tracing::info!("MP3 tags added successfully"),
                    Err(e) => tracing::warn!("Failed to add MP3 tags: {}", e),
                }
            }
            "flac" => {
                let cover_label = if embed_cover { "original" } else { "none" };
                tracing::info!("Adding FLAC metadata (cover: {})", cover_label);
                match audio_buffer.add_flac_metadata(&song_detail, embed_artwork) {
                    Ok(()) => tracing::info!("FLAC metadata added successfully"),
                    Err(e) => tracing::warn!("Failed to add FLAC metadata: {}", e),
                }
            }
            _ => {
                tracing::info!("Unknown format {}, skipping tag embedding", file_ext);
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

fn url_bitrate_candidates(has_music_u: bool) -> Vec<u64> {
    if has_music_u {
        vec![999_000, 320_000, 128_000]
    } else {
        vec![320_000, 128_000]
    }
}

fn should_spawn_message_task(text: &str) -> bool {
    text.starts_with('/')
        || text.contains("music.163.com")
        || text.contains("163cn.tv")
        || text.contains("163cn.link")
}

fn message_task_limit(max_concurrent_downloads: u32) -> usize {
    (max_concurrent_downloads as usize)
        .saturating_mul(4)
        .clamp(8, 256)
}

fn upload_task_limit(max_concurrent_uploads: u32) -> usize {
    (max_concurrent_uploads as usize).clamp(1, 64)
}

fn should_refresh_upload_client(upload_state: &UploadClientState, reuse_limit: u32) -> bool {
    if upload_state.bot.is_none() {
        return true;
    }

    if reuse_limit == 0 {
        return false;
    }

    upload_state.reuse_count >= reuse_limit
}

fn next_upload_mode(mode: UploadMode, succeeded: bool) -> Option<UploadMode> {
    if succeeded {
        return None;
    }

    match mode {
        UploadMode::Audio => Some(UploadMode::Document),
        UploadMode::Document => None,
    }
}

fn collect_maintenance_signals(
    counters: &MaintenanceCounters,
    config: &Config,
) -> Vec<MaintenanceSignal> {
    let mut signals = Vec::with_capacity(2);

    if MaintenanceCounters::should_run(
        &counters.db_analyze_requests,
        config.db_analyze_interval_requests,
    ) {
        signals.push(MaintenanceSignal::AnalyzeDb);
    }

    if MaintenanceCounters::should_run(
        &counters.memory_release_requests,
        config.memory_release_interval_requests,
    ) {
        signals.push(MaintenanceSignal::ReleaseMemory);
    }

    signals
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
    mut rx: tokio::sync::mpsc::UnboundedReceiver<MaintenanceSignal>,
    database: Database,
) {
    while let Some(signal) = rx.recv().await {
        match signal {
            MaintenanceSignal::AnalyzeDb => {
                if let Err(e) = database.optimize_planner().await {
                    tracing::warn!("Database planner optimize failed: {}", e);
                }
            }
            MaintenanceSignal::ReleaseMemory => {
                crate::memory::force_memory_release();
                crate::memory::log_memory_stats();
            }
        }
    }
}

fn build_reqwest_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    builder.build().map_err(|e| {
        tracing::error!("Failed to build HTTP client: {}", e);
        e.into()
    })
}

const PERF_STAGE_SELECT_URL: &str = "select_url";
const PERF_STAGE_PRE_UPLOAD_PATH: &str = "pre_upload_path";

#[cfg(test)]
fn critical_path_stage_labels() -> [&'static str; 2] {
    [PERF_STAGE_SELECT_URL, PERF_STAGE_PRE_UPLOAD_PATH]
}

fn format_perf(label: &str, duration: std::time::Duration) -> String {
    format!("[{label}] {}ms", duration.as_millis())
}

fn upload_log_enabled(config: &Config, level: UploadLogLevel) -> bool {
    config.upload_log_level.allows(level)
}

fn should_set_upload_pool_idle_timeout(secs: u64) -> bool {
    secs > 0
}

fn append_search_result_line(results: &mut String, index: usize, song_name: &str, artists: &str) {
    use std::fmt::Write;

    if let Err(e) = writeln!(results, "{index}.「{song_name}」 - {artists}") {
        tracing::error!("Failed to format search result line: {}", e);
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

fn build_upload_bot(config: &Config) -> Result<Bot> {
    // API URL must match teloxide's internal format: base URL without "/bot" suffix
    // teloxide automatically appends "bot<TOKEN>/" to the path
    let api_url_str = if !config.bot_api.is_empty() && config.bot_api != "https://api.telegram.org"
    {
        let base = config.bot_api.trim_end_matches("/bot");
        format!("{base}/")
    } else {
        "https://api.telegram.org/".to_string()
    };

    let api_url = match parse_api_url(&api_url_str) {
        Ok(url) => url,
        Err(e) => {
            tracing::warn!(
                "Invalid upload API URL '{}': {}. Using default.",
                api_url_str,
                e
            );
            match parse_api_url("https://api.telegram.org/") {
                Ok(url) => url,
                Err(err) => {
                    tracing::error!("Failed to parse fallback API URL: {}", err);
                    return Err(BotError::Other(anyhow::anyhow!(
                        "failed to parse fallback API URL"
                    )));
                }
            }
        }
    };

    if api_url_str != "https://api.telegram.org/" {
        tracing::info!("Using custom API for upload: {}", api_url);
    }

    let mut client_builder = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(std::time::Duration::from_secs(config.upload_timeout_secs))
        .pool_max_idle_per_host(config.upload_pool_max_idle_per_host)
        .tcp_nodelay(true)
        .no_gzip()
        .user_agent("Go-http-client/2.0")
        .default_headers(reqwest::header::HeaderMap::new());

    if should_set_upload_pool_idle_timeout(config.upload_pool_idle_timeout_secs) {
        client_builder = client_builder.pool_idle_timeout(std::time::Duration::from_secs(
            config.upload_pool_idle_timeout_secs,
        ));
    }

    if upload_log_enabled(config, UploadLogLevel::Debug) {
        tracing::debug!(
            "Upload diag: client settings pool_max_idle_per_host={}, pool_idle_timeout_secs={}, timeout_secs={}, api_url={}",
            config.upload_pool_max_idle_per_host,
            config.upload_pool_idle_timeout_secs,
            config.upload_timeout_secs,
            api_url
        );
    }

    let client = build_reqwest_client(client_builder)?;
    Ok(Bot::with_client(&config.bot_token, client).set_api_url(api_url))
}

async fn acquire_upload_bot(state: &Arc<BotState>) -> Result<Bot> {
    let reuse_limit = state.config.upload_client_reuse_requests;

    let (reason, reuse_count_before) = {
        let mut upload_state = state.upload_client_state.lock().await;

        if !should_refresh_upload_client(&upload_state, reuse_limit) {
            let next_reuse_count = upload_state.reuse_count.saturating_add(1);
            if upload_log_enabled(&state.config, UploadLogLevel::Debug) {
                tracing::debug!(
                    "Upload diag: reusing client (reuse_count: {}, reuse_limit: {})",
                    upload_state.reuse_count,
                    reuse_limit
                );
                tracing::debug!("Upload diag: reuse_count -> {}", next_reuse_count);
            }
            upload_state.reuse_count = next_reuse_count;
            return get_upload_bot(&upload_state);
        }

        let reason = if upload_state.bot.is_none() {
            "uninitialized"
        } else {
            "reuse_limit"
        };
        (reason, upload_state.reuse_count)
    };

    if upload_log_enabled(&state.config, UploadLogLevel::Info) {
        tracing::info!(
            "Upload diag: creating client (reason: {}, reuse_count: {}, reuse_limit: {})",
            reason,
            reuse_count_before,
            reuse_limit
        );
    }

    let build_start = std::time::Instant::now();
    let built_bot = build_upload_bot(&state.config)?;

    let mut upload_state = state.upload_client_state.lock().await;
    if should_refresh_upload_client(&upload_state, reuse_limit) {
        upload_state.bot = Some(built_bot);
        upload_state.reuse_count = 0;
        if upload_log_enabled(&state.config, UploadLogLevel::Info) {
            tracing::info!(
                "Upload diag: client ready in {}ms",
                build_start.elapsed().as_millis()
            );
        }
    } else if upload_log_enabled(&state.config, UploadLogLevel::Debug) {
        tracing::debug!("Upload diag: client refreshed by another task");
    }

    let next_reuse_count = upload_state.reuse_count.saturating_add(1);
    if upload_log_enabled(&state.config, UploadLogLevel::Debug) {
        tracing::debug!("Upload diag: reuse_count -> {}", next_reuse_count);
    }
    upload_state.reuse_count = next_reuse_count;

    get_upload_bot(&upload_state)
}

async fn run_upload_prewarm<T, F, Fut>(config: &Config, warmup: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    match warmup().await {
        Ok(_) => {
            if upload_log_enabled(config, UploadLogLevel::Info) {
                tracing::info!("Upload prewarm completed");
            }
            true
        }
        Err(e) => {
            tracing::warn!("Upload prewarm failed, continuing startup: {}", e);
            false
        }
    }
}

async fn acquire_download_permit(
    semaphore: &tokio::sync::Semaphore,
) -> Result<tokio::sync::SemaphorePermit<'_>> {
    semaphore.acquire().await.map_err(|e| {
        tracing::error!("Download semaphore closed: {}", e);
        BotError::Other(anyhow::anyhow!("download semaphore closed"))
    })
}

async fn acquire_upload_permit(
    semaphore: &tokio::sync::Semaphore,
) -> Result<tokio::sync::SemaphorePermit<'_>> {
    semaphore.acquire().await.map_err(|e| {
        tracing::error!("Upload semaphore closed: {}", e);
        BotError::Other(anyhow::anyhow!("upload semaphore closed"))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::UploadClientState;
    use super::acquire_download_permit;
    use super::append_search_result_line;
    use super::build_music_url;
    use super::build_reqwest_client;
    use super::format_perf;
    use super::get_upload_bot;
    use super::parse_api_url;
    use crate::config::UploadLogLevel;
    use teloxide::Bot;

    fn cached_size(size: u64) -> u64 {
        size
    }

    #[test]
    fn perf_timer_formats_label_and_duration() {
        let label = "fetch_url";
        let formatted = format_perf(label, std::time::Duration::from_millis(12));
        assert!(formatted.contains("fetch_url"));
        assert!(formatted.contains("12"));
    }

    #[test]
    fn build_music_url_accepts_valid_base() {
        let url = build_music_url("https://music.163.com", 123).expect("valid url");
        assert_eq!(url.as_str(), "https://music.163.com/song?id=123");
    }

    #[test]
    fn build_music_url_rejects_invalid_base() {
        assert!(build_music_url("ht!tp:// bad", 1).is_err());
    }

    #[test]
    fn parse_api_url_accepts_valid_base() {
        let url = parse_api_url("https://api.telegram.org/").expect("valid url");
        assert_eq!(url.as_str(), "https://api.telegram.org/");
    }

    #[test]
    fn parse_api_url_rejects_invalid_base() {
        assert!(parse_api_url("not a url").is_err());
    }

    #[test]
    fn build_reqwest_client_returns_client() {
        let client =
            build_reqwest_client(reqwest::Client::builder()).expect("client should be built");
        let _ = client;
    }

    #[test]
    fn get_upload_bot_returns_error_when_missing() {
        let state = UploadClientState {
            bot: None,
            reuse_count: 0,
        };
        assert!(get_upload_bot(&state).is_err());
    }

    #[test]
    fn get_upload_bot_returns_bot_when_present() {
        let bot = Bot::new("token");
        let state = UploadClientState {
            bot: Some(bot),
            reuse_count: 0,
        };
        assert!(get_upload_bot(&state).is_ok());
    }

    #[tokio::test]
    async fn acquire_download_permit_returns_error_when_closed() {
        let semaphore = tokio::sync::Semaphore::new(1);
        semaphore.close();

        let err = acquire_download_permit(&semaphore)
            .await
            .expect_err("expected error for closed semaphore");
        let err_str = format!("{err}");
        assert!(err_str.contains("download semaphore closed"));
    }

    #[tokio::test]
    async fn fetch_detail_and_url_in_parallel() {
        let (detail, url) = tokio::join!(async { 1 }, async { 2 });
        assert_eq!(detail, 1);
        assert_eq!(url, 2);
    }

    #[test]
    fn append_search_result_line_formats_output() {
        let mut results = String::new();
        append_search_result_line(&mut results, 1, "Song", "Artist");
        assert_eq!(results, "1.「Song」 - Artist\n");
    }

    #[test]
    fn cached_file_size_is_reused() {
        let size = cached_size(1024);
        assert_eq!(size, 1024);
    }

    #[test]
    fn perf_log_includes_stage_label() {
        let s = format_perf("download", std::time::Duration::from_millis(50));
        assert!(s.contains("download"));
    }

    #[test]
    fn critical_path_stage_labels_are_stable() {
        assert_eq!(
            super::critical_path_stage_labels(),
            ["select_url", "pre_upload_path"]
        );
    }

    #[test]
    fn url_bitrate_candidates_prefers_flac_with_music_u() {
        assert_eq!(
            super::url_bitrate_candidates(true),
            vec![999_000, 320_000, 128_000]
        );
    }

    #[test]
    fn url_bitrate_candidates_uses_mp3_without_music_u() {
        assert_eq!(super::url_bitrate_candidates(false), vec![320_000, 128_000]);
    }

    #[test]
    fn spawn_gate_identifies_supported_messages() {
        assert!(super::should_spawn_message_task("/start"));
        assert!(super::should_spawn_message_task(
            "https://music.163.com/song?id=1"
        ));
        assert!(super::should_spawn_message_task("https://163cn.tv/abcd"));
        assert!(super::should_spawn_message_task("https://163cn.link/abcd"));
        assert!(!super::should_spawn_message_task("hello world"));
    }

    #[test]
    fn spawn_gate_calculates_reasonable_limit() {
        assert_eq!(super::message_task_limit(0), 8);
        assert_eq!(super::message_task_limit(1), 8);
        assert_eq!(super::message_task_limit(3), 12);
        assert_eq!(super::message_task_limit(200), 256);
    }

    #[test]
    fn maintenance_scheduler_emits_expected_signals() {
        let counters = super::MaintenanceCounters::new();
        let mut config = crate::config::Config::default();
        config.db_analyze_interval_requests = 2;
        config.memory_release_interval_requests = 3;

        assert!(super::collect_maintenance_signals(&counters, &config).is_empty());

        let second = super::collect_maintenance_signals(&counters, &config);
        assert_eq!(second, vec![super::MaintenanceSignal::AnalyzeDb]);

        let third = super::collect_maintenance_signals(&counters, &config);
        assert_eq!(third, vec![super::MaintenanceSignal::ReleaseMemory]);
    }

    #[test]
    fn upload_pool_idle_timeout_disabled_when_zero() {
        assert!(!super::should_set_upload_pool_idle_timeout(0));
        assert!(super::should_set_upload_pool_idle_timeout(60));
    }

    #[test]
    fn upload_log_level_allows_thresholds() {
        assert!(UploadLogLevel::Info.allows(UploadLogLevel::Error));
        assert!(UploadLogLevel::Info.allows(UploadLogLevel::Warning));
        assert!(UploadLogLevel::Info.allows(UploadLogLevel::Info));
        assert!(!UploadLogLevel::Info.allows(UploadLogLevel::Debug));
        assert!(!UploadLogLevel::None.allows(UploadLogLevel::Error));
    }

    #[test]
    fn upload_limit_clamps_bounds() {
        assert_eq!(super::upload_task_limit(0), 1);
        assert_eq!(super::upload_task_limit(1), 1);
        assert_eq!(super::upload_task_limit(4), 4);
        assert_eq!(super::upload_task_limit(1000), 64);
    }

    #[test]
    fn upload_client_refresh_decision_works() {
        let has_bot = UploadClientState {
            bot: Some(Bot::new("token")),
            reuse_count: 0,
        };
        let no_bot = UploadClientState {
            bot: None,
            reuse_count: 0,
        };
        let exhausted = UploadClientState {
            bot: Some(Bot::new("token")),
            reuse_count: 10,
        };

        assert!(super::should_refresh_upload_client(&no_bot, 10));
        assert!(!super::should_refresh_upload_client(&has_bot, 10));
        assert!(super::should_refresh_upload_client(&exhausted, 10));
        assert!(!super::should_refresh_upload_client(&exhausted, 0));
    }

    #[tokio::test]
    async fn upload_prewarm_failure_is_non_fatal() {
        let config = crate::config::Config::default();
        let ok = super::run_upload_prewarm(&config, || async {
            Err::<(), crate::error::BotError>(crate::error::BotError::MusicApi(
                "simulated prewarm failure".to_string(),
            ))
        })
        .await;

        assert!(!ok);
    }

    #[tokio::test]
    async fn upload_prewarm_runs_warmup_path() {
        let config = crate::config::Config::default();
        let ok =
            super::run_upload_prewarm(&config, || async { Ok::<(), crate::error::BotError>(()) })
                .await;

        assert!(ok);
    }

    #[test]
    fn upload_mode_switches_to_document_after_audio_failure() {
        assert_eq!(
            super::next_upload_mode(super::UploadMode::Audio, false),
            Some(super::UploadMode::Document)
        );
        assert_eq!(
            super::next_upload_mode(super::UploadMode::Audio, true),
            None
        );
        assert_eq!(
            super::next_upload_mode(super::UploadMode::Document, false),
            None
        );
    }

    #[test]
    fn inflight_registry_first_is_leader() {
        let inflight = Arc::new(super::InflightDownloads::default());
        let claim = inflight.begin(42);
        assert!(matches!(claim, super::InflightClaim::Leader(_)));
    }

    #[tokio::test]
    async fn inflight_registry_second_waits() {
        let inflight = Arc::new(super::InflightDownloads::default());
        let leader = match inflight.begin(99) {
            super::InflightClaim::Leader(guard) => guard,
            super::InflightClaim::Follower(_) => panic!("first claim should be leader"),
        };

        let follower_entry = match inflight.begin(99) {
            super::InflightClaim::Leader(_) => panic!("second claim should be follower"),
            super::InflightClaim::Follower(entry) => entry,
        };

        let pending = tokio::time::timeout(Duration::from_millis(20), follower_entry.wait()).await;
        assert!(pending.is_err(), "follower should wait while leader active");

        drop(leader);

        tokio::time::timeout(Duration::from_secs(1), follower_entry.wait())
            .await
            .expect("follower should be released after leader finishes");
    }

    #[tokio::test]
    async fn singleflight_claim_helper_waits_for_existing_leader() {
        let inflight = Arc::new(super::InflightDownloads::default());
        let leader = super::acquire_download_leader(&inflight, 7)
            .await
            .expect("first claim should be leader");

        let inflight_for_follower = Arc::clone(&inflight);
        let follower = tokio::spawn(async move {
            super::acquire_download_leader(&inflight_for_follower, 7)
                .await
                .is_none()
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!follower.is_finished(), "follower should still be waiting");

        drop(leader);

        let waited = tokio::time::timeout(Duration::from_secs(1), follower)
            .await
            .expect("follower task should complete")
            .expect("follower task join should succeed");
        assert!(waited, "follower claim should resolve as waiting follower");
    }

    #[tokio::test]
    async fn tagging_wrapper_returns_buffer_for_unknown_format() {
        let buffer = crate::audio_buffer::AudioBuffer::Memory {
            data: vec![1, 2, 3],
            filename: "sample.bin".to_string(),
            capacity: 3,
        };
        let detail = crate::music_api::SongDetail {
            id: 1,
            name: "Song".to_string(),
            dt: Some(1_000),
            ar: Some(vec![]),
            al: None,
        };

        let tagged = super::apply_tags_in_blocking(buffer, "bin".to_string(), detail, None, false)
            .await
            .expect("unknown format should keep buffer unchanged");

        assert_eq!(tagged.size().await, 3);
    }

    #[tokio::test]
    async fn tagging_wrapper_adds_mp3_id3_header() {
        let buffer = crate::audio_buffer::AudioBuffer::Memory {
            data: vec![0xFF, 0xFB, 0x90, 0x64],
            filename: "sample.mp3".to_string(),
            capacity: 4,
        };
        let detail = crate::music_api::SongDetail {
            id: 2,
            name: "Song".to_string(),
            dt: Some(120_000),
            ar: Some(vec![crate::music_api::Artist {
                id: 1,
                name: "Artist".to_string(),
            }]),
            al: Some(crate::music_api::Album {
                id: 1,
                name: "Album".to_string(),
                pic_url: None,
            }),
        };

        let tagged = super::apply_tags_in_blocking(buffer, "mp3".to_string(), detail, None, false)
            .await
            .expect("mp3 tagging should succeed");
        let data = tagged.get_data().await.expect("read tagged data");
        assert!(data.starts_with(b"ID3"));
    }

    #[test]
    fn inline_query_search_prefix_parsed_once() {
        let (keyword, is_search) = super::parse_inline_query_keyword("search keyword");
        assert!(is_search);
        assert_eq!(keyword, "keyword");

        let (keyword, is_search) = super::parse_inline_query_keyword("search");
        assert!(is_search);
        assert!(keyword.is_empty());

        let (keyword, is_search) = super::parse_inline_query_keyword("hello world");
        assert!(!is_search);
        assert_eq!(keyword, "hello world");
    }

    #[test]
    fn start_with_music_id_uses_direct_process_path() {
        assert_eq!(super::parse_start_music_id(Some("123")), Some(123));
        assert_eq!(super::parse_start_music_id(Some("  456  ")), Some(456));
        assert_eq!(super::parse_start_music_id(Some("invalid")), None);
        assert_eq!(super::parse_start_music_id(None), None);
    }
}

async fn handle_music_url(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    text: &str,
) -> ResponseResult<()> {
    if let Some(music_id) = parse_music_id(text) {
        return process_music(bot, msg, state, music_id).await;
    }

    let Some(url) = extract_first_url(text) else {
        bot.send_message(msg.chat.id, "无法从链接中提取音乐ID")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
        return Ok(());
    };

    let final_url = match state.music_api.resolve_share_link(&url).await {
        Ok(final_url) => final_url.to_string(),
        Err(e) => {
            tracing::warn!("Failed to resolve share link: {}", e);
            bot.send_message(msg.chat.id, "无法从链接中提取音乐ID")
                .reply_parameters(ReplyParameters::new(msg.id))
                .await?;
            return Ok(());
        }
    };

    if let Some(music_id) = parse_music_id(&final_url) {
        process_music(bot, msg, state, music_id).await
    } else {
        bot.send_message(msg.chat.id, "无法从链接中提取音乐ID")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
        Ok(())
    }
}

async fn handle_search_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let keyword = match args {
        Some(kw) if !kw.is_empty() => kw,
        _ => {
            bot.send_message(msg.chat.id, "请输入搜索关键词")
                .reply_parameters(ReplyParameters::new(msg.id))
                .await?;
            return Ok(());
        }
    };

    let search_msg = bot
        .send_message(msg.chat.id, "🔍 搜索中...")
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    match state.music_api.search_songs(&keyword, 10).await {
        Ok(songs) => {
            if songs.is_empty() {
                bot.edit_message_text(msg.chat.id, search_msg.id, "未找到相关歌曲")
                    .await?;
                return Ok(());
            }

            let mut results = String::new();
            let mut buttons = Vec::new();

            for (i, song) in songs.iter().take(8).enumerate() {
                let artists = format_artists(&song.artists);
                append_search_result_line(&mut results, i + 1, &song.name, &artists);
                buttons.push(InlineKeyboardButton::callback(
                    format!("{}", i + 1),
                    format!("music {}", song.id),
                ));
            }

            let keyboard = InlineKeyboardMarkup::new(vec![buttons]);

            bot.edit_message_text(msg.chat.id, search_msg.id, results)
                .reply_markup(keyboard)
                .await?;
        }
        Err(e) => {
            bot.edit_message_text(msg.chat.id, search_msg.id, format!("搜索失败: {e}"))
                .await?;
        }
    }

    Ok(())
}

async fn handle_about_command(
    bot: &Bot,
    msg: &Message,
    _state: &Arc<BotState>,
) -> ResponseResult<()> {
    let about_text = format!(
        r"🎵 Music163bot-Rust v{}

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
        env!("CARGO_PKG_VERSION")
    );

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
        bot.send_message(msg.chat.id, "请输入歌曲ID或关键词")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
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
                    bot.send_message(msg.chat.id, "未找到相关歌曲")
                        .reply_parameters(ReplyParameters::new(msg.id))
                        .await?;
                    return Ok(());
                }
            }
            Err(e) => {
                bot.send_message(msg.chat.id, format!("搜索失败: {e}"))
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .await?;
                return Ok(());
            }
        }
    };

    let status_msg = bot
        .send_message(msg.chat.id, "🎵 正在获取歌词...")
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    match state.music_api.get_song_lyric(music_id).await {
        Ok(lyric) => {
            if lyric.trim().is_empty() || lyric == "No lyrics available" {
                bot.edit_message_text(msg.chat.id, status_msg.id, "该歌曲暂无歌词")
                    .await?;
                return Ok(());
            }

            // Get song detail for filename
            let song_detail = match state.music_api.get_song_detail(music_id).await {
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
            let lrc_path = format!("{}/{}", state.config.cache_dir, lrc_filename);

            tokio::fs::write(&lrc_path, &lyric)
                .await
                .map_err(|e| RequestError::Io(Arc::new(e)))?;

            bot.send_document(
                msg.chat.id,
                InputFile::file(std::path::Path::new(&lrc_path)),
            )
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;

            tokio::fs::remove_file(&lrc_path).await.ok();
            bot.delete_message(msg.chat.id, status_msg.id).await.ok();
        }
        Err(e) => {
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

    let status_text = format!(
        r"📊 *统计信息*

🎵 数据库中总缓存歌曲数量: {total_count}
👤 当前用户缓存歌曲数量: {user_count}
💬 当前对话缓存歌曲数量: {chat_count}

🤖 Bot 运行状态: 正常
🦀 语言: Rust
⚡ 框架: Teloxide
"
    );

    bot.send_message(msg.chat.id, status_text)
        .parse_mode(ParseMode::MarkdownV2)
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

    if !state.config.bot_admin.contains(&user_id) {
        bot.send_message(msg.chat.id, "❌ 该命令仅限管理员使用")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
        return Ok(());
    }

    let args = args.unwrap_or_default();

    if args.is_empty() {
        bot.send_message(
            msg.chat.id,
            "请输入要删除缓存的歌曲ID\n\n用法: `/rmcache <音乐ID>`",
        )
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;
        return Ok(());
    }

    if let Some(music_id) = parse_music_id(&args) {
        let music_id_i64 = music_id as i64;

        // Get song info before deletion
        if let Ok(Some(song_info)) = state.database.get_song_by_music_id(music_id_i64).await {
            match state.database.delete_song_by_music_id(music_id_i64).await {
                Ok(deleted) => {
                    if deleted {
                        bot.send_message(
                            msg.chat.id,
                            format!("✅ 已删除歌曲缓存: {}", song_info.song_name),
                        )
                        .reply_parameters(ReplyParameters::new(msg.id))
                        .await?;
                    } else {
                        bot.send_message(msg.chat.id, "歌曲未缓存")
                            .reply_parameters(ReplyParameters::new(msg.id))
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("删除缓存失败: {e}"))
                        .reply_parameters(ReplyParameters::new(msg.id))
                        .await?;
                }
            }
        } else {
            bot.send_message(msg.chat.id, "歌曲未缓存")
                .reply_parameters(ReplyParameters::new(msg.id))
                .await?;
        }
    } else {
        bot.send_message(msg.chat.id, "无效的歌曲ID")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
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

    if !state.config.bot_admin.contains(&user_id) {
        bot.send_message(msg.chat.id, "❌ 该命令仅限管理员使用")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
        return Ok(());
    }

    // Send confirmation message
    bot
        .send_message(msg.chat.id, "⚠️ 确认要清除所有缓存吗？\n\n这将删除数据库中的所有歌曲缓存记录。\n\n请在30秒内再次发送 `/clearallcache confirm` 确认操作。")
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    Ok(())
}

async fn handle_clearallcache_confirm_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    // Check if user is admin
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);

    if !state.config.bot_admin.contains(&user_id) {
        bot.send_message(msg.chat.id, "❌ 该命令仅限管理员使用")
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
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
    if let Some(data) = query.data {
        let parts: Vec<&str> = data.split_whitespace().collect();
        if parts.len() >= 2
            && parts[0] == "music"
            && let Ok(music_id) = parts[1].parse::<u64>()
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
            let mut results = Vec::new();

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
    // bitrate_bps may already be bps, convert to kbps with 2 decimals
    let kbps = (bitrate_bps as f64) / 1000.0;
    let ext = file_ext.to_lowercase();
    format!(
        "「{title}」- {artists}\n专辑: {album}\n#网易云音乐 #{ext} {size_mb:.2}MB {kbps:.2}kbps\nvia @{bot_username}",
    )
}
