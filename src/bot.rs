use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::Context;
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::{StreamExt, TryStreamExt};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, get_current_pid};
use teloxide::prelude::*;
use teloxide::sugar::request::RequestLinkPreviewExt;
use teloxide::types::{
    CallbackQuery, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InlineQuery,
    InlineQueryResult, InlineQueryResultArticle, InputFile, InputMessageContent,
    InputMessageContentText, MaybeInaccessibleMessage, Message, MessageKind, ParseMode,
    ReplyParameters,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::audio_buffer::{AudioBuffer, ThumbnailBuffer};
use crate::config::{Config, CoverMode, UploadLogLevel};
use crate::database::{Database, SongInfo};
use crate::error::{BotError, Result};
use crate::music_api::{MusicApi, format_artists};
use crate::utils::{
    MusicCollectionTarget, build_http_client, clean_filename, ensure_dir, extract_first_url,
    parse_music_collection_target, parse_music_id, throughput_mbps, update_peak,
};

pub struct BotState {
    pub config: Config,
    pub database: Database,
    pub music_api: Arc<MusicApi>,
    inflight_downloads: Arc<InflightDownloads>,
    pub download_semaphore: Arc<tokio::sync::Semaphore>,
    pub upload_semaphore: Arc<tokio::sync::Semaphore>,
    pub message_task_semaphore: Arc<tokio::sync::Semaphore>,
    pub maintenance_tx: tokio::sync::mpsc::Sender<MaintenanceSignal>,
    pub bot_username: String,
    pub upload_client_state: Arc<Mutex<UploadClientState>>,
    pub maintenance_counters: MaintenanceCounters,
    pub upload_counters: UploadCounters,
    pub runtime_metrics: RuntimeMetrics,
    pub is_official_api: bool,
}

#[derive(Debug)]
pub struct UploadClientState {
    pub bot: Option<Bot>,
    pub raw_client: Option<reqwest::Client>,
    pub upload_api_url: String,
    pub reuse_count: u32,
}

#[derive(Debug, Default)]
pub struct UploadCounters {
    pub in_flight: AtomicU32,
    pub peak_in_flight: AtomicU32,
}

const SPEED_SAMPLE_WINDOW: usize = 20;
const STATUS_RESOURCE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const MIN_DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;
const MAINTENANCE_QUEUE_CAPACITY: usize = 32;
const CACHE_PRUNE_INTERVAL_REQUESTS: u32 = 50;
const PERF_LOG_PREFIX: &str = "PERF";
const PERF_STAGE_CACHE_LOOKUP: &str = "cache_lookup";
const PERF_STAGE_SINGLEFLIGHT_WAIT: &str = "singleflight_wait";
const PERF_STAGE_COVER_DOWNLOAD: &str = "cover_download";
const PERF_STAGE_DOWNLOAD_AUDIO: &str = "download_audio";
const PERF_STAGE_UPLOAD_PERMIT_WAIT: &str = "upload_permit_wait";
const PERF_STAGE_UPLOAD_CLIENT_ACQUIRE: &str = "upload_client_acquire";
const PERF_STAGE_UPLOAD_SEND: &str = "upload_send";
const PERF_STAGE_TAG_PROCESS: &str = "tag_process";
const PERF_STAGE_DB_SAVE: &str = "db_save";
const PERF_STAGE_E2E_TOTAL: &str = "e2e_total";
static PERF_TRACE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
struct PerfTraceContext {
    trace_id: String,
    music_id: u64,
    topology: &'static str,
    cache_path: &'static str,
}

impl PerfTraceContext {
    fn new(music_id: u64, topology: &'static str, cache_path: &'static str) -> Self {
        Self {
            trace_id: next_perf_trace_id(),
            music_id,
            topology,
            cache_path,
        }
    }

    fn with_cache_path(&self, cache_path: &'static str) -> Self {
        Self {
            cache_path,
            ..self.clone()
        }
    }

    fn log_stage(&self, stage: &str, duration: std::time::Duration) {
        tracing::info!(
            "{}",
            format_perf_stage_line(
                &self.trace_id,
                self.music_id,
                self.topology,
                self.cache_path,
                stage,
                duration,
            )
        );
    }
}

fn build_perf_trace_context(
    state: &BotState,
    music_id: u64,
    cache_path: &'static str,
) -> PerfTraceContext {
    PerfTraceContext::new(
        music_id,
        upload_topology_label(&state.config, state.is_official_api),
        cache_path,
    )
}

fn next_perf_trace_id() -> String {
    let ts_millis = chrono::Utc::now().timestamp_millis();
    let seq = PERF_TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{ts_millis:x}-{seq:x}")
}

fn format_perf_stage_line(
    trace_id: &str,
    music_id: u64,
    topology: &str,
    cache_path: &str,
    stage: &str,
    duration: std::time::Duration,
) -> String {
    format!(
        "{PERF_LOG_PREFIX}|trace_id={trace_id}|music_id={music_id}|topology={topology}|cache_path={cache_path}|stage={stage}|elapsed_ms={}",
        duration.as_millis()
    )
}

fn upload_topology_label(config: &Config, is_official_api: bool) -> &'static str {
    if is_official_api {
        "official_api"
    } else if config.upload_local_file_uri {
        "selfhost_api_uri_upload"
    } else {
        "selfhost_api_multipart_upload"
    }
}

#[derive(Debug)]
pub struct RuntimeMetrics {
    started_at: Instant,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    speed_metrics: std::sync::Mutex<SpeedMetrics>,
}

#[derive(Debug, Default)]
struct SpeedMetrics {
    download: DirectionSpeedMetrics,
    upload: DirectionSpeedMetrics,
}

#[derive(Debug, Default)]
struct DirectionSpeedMetrics {
    recent_mbps: VecDeque<f64>,
    total_bytes: u128,
    total_nanos: u128,
    samples: u64,
}

#[derive(Debug, Clone, Copy)]
struct CacheSnapshot {
    hits: u64,
    misses: u64,
    hit_rate_percent: f64,
}

#[derive(Debug, Clone, Copy)]
struct SpeedSnapshot {
    last_mbps: f64,
    avg_mbps: f64,
    p95_mbps: f64,
    samples: u64,
    recent_samples: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResourceSnapshot {
    cpu_percent: f32,
    system_used_memory_mb: u64,
    system_total_memory_mb: u64,
    bot_memory_mb: Option<u64>,
}

static STATUS_RESOURCE_CACHE: LazyLock<std::sync::Mutex<(System, Instant, ResourceSnapshot)>> =
    LazyLock::new(|| {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_memory();
        let bot_memory_mb = sample_current_process_memory_mb(&mut system);
        let snapshot = ResourceSnapshot {
            cpu_percent: system.global_cpu_usage(),
            system_used_memory_mb: system.used_memory() / (1024 * 1024),
            system_total_memory_mb: system.total_memory() / (1024 * 1024),
            bot_memory_mb,
        };
        std::sync::Mutex::new((system, Instant::now(), snapshot))
    });

#[derive(Debug)]
pub struct MaintenanceCounters {
    pub memory_release_requests: AtomicU32,
    pub db_analyze_requests: AtomicU32,
    pub api_cache_prune_requests: AtomicU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceSignal {
    AnalyzeDb,
    ReleaseMemory,
    PruneApiCache,
}

#[derive(Debug, Default)]
struct InflightDownloads {
    entries: DashMap<u64, Arc<InflightEntry>>,
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
        match self.entries.entry(music_id) {
            dashmap::mapref::entry::Entry::Occupied(existing) => {
                InflightClaim::Follower(Arc::clone(existing.get()))
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(Arc::new(InflightEntry::new()));
                InflightClaim::Leader(InflightLeaderGuard {
                    music_id,
                    inflight: Arc::clone(self),
                })
            }
        }
    }

    fn finish(&self, music_id: u64) {
        if let Some((_, entry)) = self.entries.remove(&music_id) {
            entry.finish();
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
        let notified = self.notify.notified();

        #[cfg(test)]
        if let Some(hook) = take_inflight_wait_hook() {
            hook();
        }

        if self.done.load(Ordering::Acquire) {
            return;
        }

        notified.await;
    }

    fn finish(&self) {
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

#[cfg(test)]
static INFLIGHT_WAIT_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn set_inflight_wait_hook(hook: impl FnOnce() + Send + 'static) {
    let slot = INFLIGHT_WAIT_HOOK.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(Box::new(hook));
}

#[cfg(test)]
fn take_inflight_wait_hook() -> Option<Box<dyn FnOnce() + Send + 'static>> {
    let slot = INFLIGHT_WAIT_HOOK.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.take()
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
            api_cache_prune_requests: AtomicU32::new(0),
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

impl RuntimeMetrics {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            speed_metrics: std::sync::Mutex::new(SpeedMetrics::default()),
        }
    }

    fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    fn cache_snapshot(&self) -> CacheSnapshot {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let total = hits.saturating_add(misses);
        let hit_rate_percent = if total == 0 {
            0.0
        } else {
            hits as f64 * 100.0 / total as f64
        };

        CacheSnapshot {
            hits,
            misses,
            hit_rate_percent,
        }
    }

    fn record_download_speed(&self, bytes: u64, duration: std::time::Duration) {
        self.record_speed(Direction::Download, bytes, duration);
    }

    fn record_upload_speed(&self, bytes: u64, duration: std::time::Duration) {
        self.record_speed(Direction::Upload, bytes, duration);
    }

    fn record_speed(&self, direction: Direction, bytes: u64, duration: std::time::Duration) {
        let mut guard = match self.speed_metrics.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match direction {
            Direction::Download => guard.download.record(bytes, duration),
            Direction::Upload => guard.upload.record(bytes, duration),
        }
    }

    fn speed_snapshots(&self) -> (Option<SpeedSnapshot>, Option<SpeedSnapshot>) {
        let guard = match self.speed_metrics.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        (guard.download.snapshot(), guard.upload.snapshot())
    }

    fn uptime(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Download,
    Upload,
}

impl DirectionSpeedMetrics {
    fn record(&mut self, bytes: u64, duration: std::time::Duration) {
        let duration_nanos = duration.as_nanos();
        if bytes == 0 || duration_nanos == 0 {
            return;
        }

        let mbps = throughput_mbps(bytes, duration);
        if !mbps.is_finite() || mbps <= 0.0 {
            return;
        }

        self.total_bytes = self.total_bytes.saturating_add(u128::from(bytes));
        self.total_nanos = self.total_nanos.saturating_add(duration_nanos);
        self.samples = self.samples.saturating_add(1);
        self.recent_mbps.push_back(mbps);
        if self.recent_mbps.len() > SPEED_SAMPLE_WINDOW {
            self.recent_mbps.pop_front();
        }
    }

    fn snapshot(&self) -> Option<SpeedSnapshot> {
        if self.samples == 0 || self.total_nanos == 0 || self.recent_mbps.is_empty() {
            return None;
        }

        let last_mbps = self.recent_mbps.back().copied()?;
        let avg_mbps =
            (self.total_bytes as f64 / (1024.0 * 1024.0)) / (self.total_nanos as f64 / 1e9);
        let p95_mbps = percentile_95(&self.recent_mbps);
        Some(SpeedSnapshot {
            last_mbps,
            avg_mbps,
            p95_mbps,
            samples: self.samples,
            recent_samples: self.recent_mbps.len(),
        })
    }
}

fn percentile_95(samples: &VecDeque<f64>) -> f64 {
    let mut values: Vec<f64> = samples.iter().copied().collect();
    values.sort_by(f64::total_cmp);
    let len = values.len();
    let idx = ((len * 95).div_ceil(100)).saturating_sub(1);
    values[idx.min(len.saturating_sub(1))]
}

fn sample_resource_snapshot() -> ResourceSnapshot {
    let mut guard = match STATUS_RESOURCE_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let (system, last_refresh, snapshot) = &mut *guard;
    if last_refresh.elapsed() >= STATUS_RESOURCE_REFRESH_INTERVAL {
        system.refresh_cpu_usage();
        system.refresh_memory();
        let bot_memory_mb = sample_current_process_memory_mb(system);
        *snapshot = ResourceSnapshot {
            cpu_percent: system.global_cpu_usage(),
            system_used_memory_mb: system.used_memory() / (1024 * 1024),
            system_total_memory_mb: system.total_memory() / (1024 * 1024),
            bot_memory_mb,
        };
        *last_refresh = Instant::now();
    }
    *snapshot
}

fn sample_current_process_memory_mb(system: &mut System) -> Option<u64> {
    let current_pid = get_current_pid().ok()?;
    let pids = [current_pid];
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        false,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system
        .process(current_pid)
        .map(|process| process.memory() / (1024 * 1024))
}

fn format_bot_memory(bot_memory_mb: Option<u64>) -> String {
    bot_memory_mb.map_or_else(|| "n/a".to_string(), |mb| format!("{mb} MB"))
}

fn build_status_text(
    total_count: i64,
    user_count: i64,
    chat_count: i64,
    cache_snapshot: CacheSnapshot,
    resource_snapshot: ResourceSnapshot,
    uptime: &str,
    download_line: &str,
    upload_line: &str,
) -> String {
    format!(
        "📊 <b>系统状态</b>\n\
<b>实时运行指标</b>\n\n\
<b>💾 缓存</b>\n\
• 总缓存: <code>{total_count}</code>\n\
• 用户缓存: <code>{user_count}</code>\n\
• 群组缓存: <code>{chat_count}</code>\n\n\
<b>⚡ 运行缓存</b>\n\
• 命中: <code>{hits}</code>\n\
• 未命中: <code>{misses}</code>\n\
• 命中率: <code>{hit_rate:.2}%</code>\n\n\
<b>🖥️ 资源</b>\n\
• CPU: <code>{cpu:.1}%</code>\n\
• 系统内存: <code>{system_used}/{system_total} MB</code>\n\
• Bot 内存: <code>{bot_memory}</code>\n\
• 运行时长: <code>{uptime}</code>\n\n\
<b>🚀 传输</b>\n\
• {download_line}\n\
• {upload_line}",
        hits = cache_snapshot.hits,
        misses = cache_snapshot.misses,
        hit_rate = cache_snapshot.hit_rate_percent,
        cpu = resource_snapshot.cpu_percent,
        system_used = resource_snapshot.system_used_memory_mb,
        system_total = resource_snapshot.system_total_memory_mb,
        bot_memory = format_bot_memory(resource_snapshot.bot_memory_mb),
    )
}

fn format_uptime(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_speed_line(label: &str, snapshot: Option<SpeedSnapshot>) -> String {
    if let Some(snapshot) = snapshot {
        format!(
            "{label}: 实时 <code>{last:.2}</code> MB/s | 平均 <code>{avg:.2}</code> MB/s | P95 <code>{p95:.2}</code> MB/s | 样本 <code>{total}</code> (窗口 <code>{window}</code>)",
            last = snapshot.last_mbps,
            avg = snapshot.avg_mbps,
            p95 = snapshot.p95_mbps,
            total = snapshot.samples,
            window = snapshot.recent_samples
        )
    } else {
        format!("{label}: 暂无非缓存测速样本")
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
        embed_cover: download_original || download_thumbnail,
    }
}

#[must_use]
fn should_download_cover(policy: CoverPolicy) -> bool {
    policy.embed_cover || policy.download_thumbnail
}

pub async fn run(config: Config) -> Result<()> {
    tracing::info!("Starting Telegram bot...");

    // Ensure cache directory exists
    ensure_dir(&config.cache_dir)?;

    // Initialize database
    let database = Database::new(&config.database).await?;
    tracing::info!("Database initialized");

    // Initialize music API
    let music_api = Arc::new(MusicApi::new_with_config(&config));
    tracing::info!("Music API initialized");

    let (maintenance_tx, maintenance_rx) = tokio::sync::mpsc::channel(MAINTENANCE_QUEUE_CAPACITY);
    let maintenance_database = database.clone();
    let maintenance_music_api = Arc::clone(&music_api);
    tokio::spawn(async move {
        maintenance_worker(maintenance_rx, maintenance_database, maintenance_music_api).await;
    });

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
                    .timeout(std::time::Duration::from_secs(30))
                    .no_gzip();
                let client = build_http_client(client_builder)?;

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
        let client = build_http_client(client_builder)?;
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
    let max_concurrent_downloads = config.max_concurrent_downloads;
    let upload_max_concurrent = config.upload_max_concurrent;
    let is_official_api = is_official_telegram_api(&config.bot_api);

    let bot_state = Arc::new(BotState {
        config,
        database,
        music_api: Arc::clone(&music_api),
        inflight_downloads: Arc::new(InflightDownloads::default()),
        download_semaphore: Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_downloads as usize,
        )),
        upload_semaphore: Arc::new(tokio::sync::Semaphore::new(upload_task_limit(
            upload_max_concurrent,
        ))),
        message_task_semaphore: Arc::new(tokio::sync::Semaphore::new(message_task_limit(
            max_concurrent_downloads,
        ))),
        maintenance_tx,
        bot_username,
        upload_client_state: Arc::new(Mutex::new(UploadClientState {
            bot: None,
            raw_client: None,
            upload_api_url: String::new(),
            reuse_count: 0,
        })),
        maintenance_counters: MaintenanceCounters::new(),
        upload_counters: UploadCounters::default(),
        runtime_metrics: RuntimeMetrics::new(),
        is_official_api,
    });

    let prewarm_state = Arc::clone(&bot_state);
    tokio::spawn(async move {
        let _ = run_upload_prewarm(&prewarm_state.config, || {
            acquire_upload_client(&prewarm_state)
        })
        .await;
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
        if !should_spawn_message_task(&text_content.text) {
            return Ok(());
        }
        let text = text_content.text.clone();

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
            if is_command_text(&text) {
                if let Err(e) = handle_command(&bot, &msg, &state, &text).await {
                    tracing::error!("Error handling command: {}", e);
                }
            }
            // Handle music URLs
            else if contains_music_link_hint(&text)
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
    let (command, args) = parse_command_and_args(text);

    // Only log music/search commands and admin commands
    if should_log_command(command) {
        tracing::info!("Command: /{} from chat {}", command, msg.chat.id);
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
            if is_clearallcache_confirm(args.as_deref()) {
                handle_clearallcache_confirm_command(bot, msg, state).await
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

fn parse_command_and_args(text: &str) -> (&str, Option<String>) {
    let (command_part, args) = if let Some((cmd, rest)) = text.split_once(char::is_whitespace) {
        (cmd, Some(rest.trim_start().to_string()))
    } else {
        (text, None)
    };
    let args = args.filter(|arg| !arg.is_empty());
    let command = command_part.trim_start_matches('/');
    let command = command
        .split_once('@')
        .map_or(command, |(without_username, _)| without_username);
    (command, args)
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

async fn download_and_send_music(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    song_detail: Arc<crate::music_api::SongDetail>,
    song_url: &crate::music_api::SongUrl,
    status_msg: &Message,
    pre_upload_path_start: std::time::Instant,
    perf_ctx: &PerfTraceContext,
    artists: &str,
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
    let artwork_future = async {
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
                                    let thumb_filename = format!(
                                        "thumb_{}_{}.jpg",
                                        song_id,
                                        chrono::Utc::now().timestamp()
                                    );
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
        cover_perf_ctx.log_stage(PERF_STAGE_COVER_DOWNLOAD, cover_download_start.elapsed());
        result
    };

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
            downloaded
        } else {
            let mut downloaded = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                downloaded += chunk.len() as u64;

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

    let upload_permit_perf_ctx = perf_ctx.clone();
    let upload_permit_future = async {
        let upload_permit_wait_start = std::time::Instant::now();
        let permit = acquire_upload_permit_owned(Arc::clone(&state.upload_semaphore)).await;
        upload_permit_perf_ctx.log_stage(
            PERF_STAGE_UPLOAD_PERMIT_WAIT,
            upload_permit_wait_start.elapsed(),
        );
        permit
    };

    // Execute downloads and upload permit acquisition in parallel
    // Acquire upload permit early to minimize delay before upload starts
    let (
        downloaded_result,
        (cover_artwork_data, thumbnail_buffer, cover_retry_exhausted),
        upload_permit_result,
    ) = tokio::join!(audio_future, artwork_future, upload_permit_future);
    let (mut audio_buffer, downloaded) = downloaded_result?;
    let _upload_permit = upload_permit_result?;
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
        bot.edit_message_text(msg.chat.id, status_msg.id, "下载失败: 文件为空")
            .await?;
        return Ok(());
    }

    if downloaded < 1024 {
        cleanup_audio_buffer(audio_buffer).await;
        cleanup_thumbnail_buffer(thumbnail_buffer).await;
        bot.edit_message_text(
            msg.chat.id,
            status_msg.id,
            format!("下载失败: 文件太小({downloaded} bytes)"),
        )
        .await?;
        return Ok(());
    }

    tracing::debug!("File validation passed: {} bytes", downloaded);

    // 封面处理：使用320x320图片嵌入文件，缩略图用于Telegram显示
    // Overlap tag processing with upload client/permit acquisition — they are independent.
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
    audio_buffer = tag_result?;
    let (_upload_bot, raw_client, api_base_url) = upload_client_result?;
    // upload_permit already acquired during download phase

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
    let mut song_info = SongInfo {
        music_id: song_id as i64,
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

    let keyboard = create_music_keyboard(song_id, &song_info.song_name, &song_info.song_artists);

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
        tracing::warn!("Upload failed: {}", e);

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

    // Delete status message
    bot.delete_message(msg.chat.id, status_msg.id).await.ok();

    Ok(())
}

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

/// Upload a file via raw reqwest multipart with pre-computed Content-Length
/// and 256 KiB streaming chunks — bypasses teloxide's 8 KiB FramedRead + chunked encoding.
async fn raw_send_file(
    client: &reqwest::Client,
    api_base_url: &str,
    config: &Config,
    is_official_api: bool,
    audio_buffer: &AudioBuffer,
    audio_bytes: Option<&Bytes>,
    file_size: u64,
    params: &RawUploadParams<'_>,
) -> Result<serde_json::Value> {
    let filename = audio_buffer.filename().to_owned();
    let mime_type = mime_for_filename(&filename);

    let mut form = reqwest::multipart::Form::new()
        .text("chat_id", params.chat_id.to_string())
        .text("caption", params.caption.to_owned());

    let audio_target = match audio_buffer {
        AudioBuffer::Disk { path, .. } => {
            select_local_upload_target(config, is_official_api, path).await
        }
        AudioBuffer::Memory { .. } => UploadFileTarget::Multipart,
    };

    match audio_target {
        UploadFileTarget::LocalUri(uri) => {
            form = form.text("audio", uri);
        }
        UploadFileTarget::Multipart => {
            // Build the file part with known length for Content-Length header
            let file_part = if let Some(bytes) = audio_bytes {
                // Memory mode: Bytes::clone() is O(1) — atomic refcount, no memcpy
                reqwest::multipart::Part::stream_with_length(bytes.clone(), file_size)
                    .file_name(filename.clone())
                    .mime_str(mime_type)?
            } else if let AudioBuffer::Disk { path, .. } = audio_buffer {
                let file = tokio::fs::File::open(path).await.map_err(|e| {
                    BotError::Other(anyhow::anyhow!("Failed to open file for upload: {e}"))
                })?;
                let stream = ReaderStream::with_capacity(file, RAW_UPLOAD_CHUNK_SIZE);
                let body = reqwest::Body::wrap_stream(stream);
                reqwest::multipart::Part::stream_with_length(body, file_size)
                    .file_name(filename.clone())
                    .mime_str(mime_type)?
            } else {
                return Err(BotError::Other(anyhow::anyhow!(
                    "Memory buffer without pre-shared Bytes"
                )));
            };
            form = form.part("audio", file_part);
        }
    }

    // reply_parameters as JSON
    let reply_params = format!(r#"{{"message_id":{}}}"#, params.reply_to_message_id);
    form = form.text("reply_parameters", reply_params);

    // reply_markup as JSON
    if let Some(ref markup_json) = params.reply_markup_json {
        form = form.text("reply_markup", markup_json.clone());
    }

    if let Some(title) = params.title {
        form = form.text("title", title.to_owned());
    }
    if let Some(performer) = params.performer {
        form = form.text("performer", performer.to_owned());
    }
    if let Some(duration) = params.duration {
        form = form.text("duration", duration.to_string());
    }

    // Attach thumbnail if available
    if let Some(thumb) = params.thumbnail {
        match thumb {
            ThumbnailBuffer::Memory { data } => {
                let len = data.len() as u64;
                let thumb_part = reqwest::multipart::Part::stream_with_length(data.clone(), len)
                    .file_name("thumb.jpg")
                    .mime_str("image/jpeg")?;
                form = form.part("thumbnail", thumb_part);
            }
            ThumbnailBuffer::Disk { path } => {
                match select_local_upload_target(config, is_official_api, path).await {
                    UploadFileTarget::LocalUri(uri) => {
                        form = form.text("thumbnail", uri);
                    }
                    UploadFileTarget::Multipart => {
                        let file = tokio::fs::File::open(path).await.map_err(|e| {
                            BotError::Other(anyhow::anyhow!("Failed to open thumbnail: {e}"))
                        })?;
                        let len = file
                            .metadata()
                            .await
                            .map_err(|e| {
                                BotError::Other(anyhow::anyhow!("Failed to stat thumbnail: {e}"))
                            })?
                            .len();
                        let stream = ReaderStream::with_capacity(file, RAW_UPLOAD_CHUNK_SIZE);
                        let body = reqwest::Body::wrap_stream(stream);
                        let thumb_part = reqwest::multipart::Part::stream_with_length(body, len)
                            .file_name("thumb.jpg")
                            .mime_str("image/jpeg")?;
                        form = form.part("thumbnail", thumb_part);
                    }
                }
            }
        }
    }

    let url = format!("{api_base_url}sendAudio");
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
    parse_telegram_api_response(&body, status, "sendAudio")
}

fn parse_telegram_api_response(
    body: &str,
    status: reqwest::StatusCode,
    method: &str,
) -> Result<serde_json::Value> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!(
            "Upload response parse error: {e}. Body: {}",
            &body[..body.len().min(500)]
        );
        BotError::Other(anyhow::anyhow!("Failed to parse upload response: {e}"))
    })?;

    if !status.is_success() || json.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let description = json
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        tracing::error!("Telegram API error ({status}): {description} [method={method}]",);
        return Err(BotError::Other(anyhow::anyhow!(
            "Telegram API error: {description} (HTTP {status})",
        )));
    }

    Ok(json)
}

/// Extract file_id from a raw Telegram API sendAudio response.
fn extract_file_id_from_response(json: &serde_json::Value) -> Option<String> {
    let result = json.get("result")?;
    result
        .get("audio")
        .and_then(|a| a.get("file_id"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Map filename extension to MIME type string.
fn mime_for_filename(filename: &str) -> &'static str {
    let path = std::path::Path::new(filename);
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("flac") => "audio/flac",
        Some(ext) if ext.eq_ignore_ascii_case("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn build_upload_bot(config: &Config) -> Result<UploadBotBundle> {
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

    let client = build_http_client(client_builder)?;
    let bot = Bot::with_client(&config.bot_token, client.clone()).set_api_url(api_url);

    // Build full API base URL for raw requests: "{base}bot{token}/"
    let raw_api_base = format!("{}bot{}/", api_url_str, config.bot_token);

    Ok(UploadBotBundle {
        bot,
        raw_client: client,
        api_base_url: raw_api_base,
    })
}

async fn acquire_upload_client(state: &Arc<BotState>) -> Result<(Bot, reqwest::Client, String)> {
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
            let bot = get_upload_bot(&upload_state)?;
            let raw_client = upload_state
                .raw_client
                .clone()
                .unwrap_or_else(reqwest::Client::new);
            let api_url = upload_state.upload_api_url.clone();
            return Ok((bot, raw_client, api_url));
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
    let bundle = build_upload_bot(&state.config)?;

    let mut upload_state = state.upload_client_state.lock().await;
    if should_refresh_upload_client(&upload_state, reuse_limit) {
        upload_state.bot = Some(bundle.bot);
        upload_state.raw_client = Some(bundle.raw_client);
        upload_state.upload_api_url = bundle.api_base_url;
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

    let bot = get_upload_bot(&upload_state)?;
    let raw_client = upload_state
        .raw_client
        .clone()
        .unwrap_or_else(reqwest::Client::new);
    let api_url = upload_state.upload_api_url.clone();
    Ok((bot, raw_client, api_url))
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

async fn acquire_upload_permit_owned(
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit> {
    semaphore.acquire_owned().await.map_err(|e| {
        tracing::error!("Upload semaphore closed: {}", e);
        BotError::Other(anyhow::anyhow!("upload semaphore closed"))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use super::UploadClientState;
    use super::acquire_download_permit;
    use super::append_search_result_line;
    use super::build_music_url;
    use super::format_perf;
    use super::get_upload_bot;
    use super::parse_api_url;
    use super::resolve_cover_policy;
    use super::should_download_cover;
    use crate::config::Config;
    use crate::config::CoverMode;
    use crate::config::UploadLogLevel;
    use crate::utils::build_http_client;
    use teloxide::Bot;
    use uuid::Uuid;

    fn create_temp_file() -> PathBuf {
        let filename = format!("music163bot_local_uri_{}", Uuid::new_v4());
        let path = std::env::temp_dir().join(filename);
        fs::write(&path, b"ok").expect("write temp file");
        path
    }

    fn critical_path_stage_labels() -> [&'static str; 2] {
        [
            super::PERF_STAGE_SELECT_URL,
            super::PERF_STAGE_PRE_UPLOAD_PATH,
        ]
    }

    #[tokio::test]
    async fn inflight_entry_wait_returns_after_finish() {
        let entry = super::InflightEntry::new();
        entry.finish();

        tokio::time::timeout(Duration::from_secs(1), entry.wait())
            .await
            .expect("wait should return when already finished");
    }

    #[tokio::test]
    async fn inflight_entry_wait_wakes_on_finish() {
        let entry = Arc::new(super::InflightEntry::new());
        let entry_for_hook = Arc::clone(&entry);
        super::set_inflight_wait_hook(move || {
            entry_for_hook.finish();
        });

        let result = tokio::time::timeout(Duration::from_secs(1), entry.wait()).await;
        assert!(result.is_ok(), "wait should complete after finish");
    }

    #[tokio::test]
    async fn lyric_parallel_fetch() {
        let start = std::time::Instant::now();
        let (res1, res2) = super::join_futures(
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("lyric")
            },
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("detail")
            },
        )
        .await;

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(90),
            "Should run in parallel"
        );
        assert_eq!(res1, Ok("lyric"));
        assert_eq!(res2, Ok("detail"));
    }

    #[tokio::test]
    async fn lyric_upload_resource_parallel() {
        let start = std::time::Instant::now();
        let (res1, res2) = super::join_futures(
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("client")
            },
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("permit")
            },
        )
        .await;

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(90),
            "Should run in parallel"
        );
        assert_eq!(res1, Ok("client"));
        assert_eq!(res2, Ok("permit"));
    }

    #[test]
    fn cover_policy_embeds_for_thumbnail_mode() {
        let policy = resolve_cover_policy(CoverMode::Thumbnail);
        assert!(policy.embed_cover);
        assert!(policy.download_thumbnail);
        assert!(!policy.download_original);
    }

    #[test]
    fn cover_policy_requires_download_when_embed_or_thumbnail() {
        let embed_only = super::CoverPolicy {
            download_original: false,
            download_thumbnail: false,
            embed_tags: true,
            embed_cover: true,
        };
        assert!(should_download_cover(embed_only));

        let thumbnail_only = super::CoverPolicy {
            download_original: false,
            download_thumbnail: true,
            embed_tags: true,
            embed_cover: false,
        };
        assert!(should_download_cover(thumbnail_only));

        let none = super::CoverPolicy {
            download_original: false,
            download_thumbnail: false,
            embed_tags: true,
            embed_cover: false,
        };
        assert!(!should_download_cover(none));
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

    #[tokio::test]
    async fn local_file_uri_disabled_by_default() {
        let mut config = crate::config::Config::default();
        config.bot_api = "http://localhost:8081".to_string();

        let path = create_temp_file();
        let uri = super::maybe_local_file_uri(&config, false, &path).await;
        fs::remove_file(&path).expect("remove temp file");

        assert!(uri.is_none());
    }

    #[tokio::test]
    async fn local_file_uri_skips_official_api() {
        let mut config = crate::config::Config::default();
        config.upload_local_file_uri = true;

        let path = create_temp_file();
        let uri = super::maybe_local_file_uri(&config, true, &path).await;
        fs::remove_file(&path).expect("remove temp file");

        assert!(uri.is_none());
    }

    #[tokio::test]
    async fn local_file_uri_builds_from_existing_path() {
        let mut config = crate::config::Config::default();
        config.upload_local_file_uri = true;

        let path = create_temp_file();
        let uri = super::maybe_local_file_uri(&config, false, &path).await;
        fs::remove_file(&path).expect("remove temp file");

        let Some(uri) = uri else {
            panic!("expected local file uri");
        };
        assert!(uri.starts_with("file://"));
    }

    #[tokio::test]
    async fn local_file_uri_returns_none_for_missing_path() {
        let mut config = crate::config::Config::default();
        config.upload_local_file_uri = true;

        let path = std::env::temp_dir().join(format!("missing_{}", Uuid::new_v4()));
        if path.exists() {
            fs::remove_file(&path).expect("remove temp file");
        }

        let uri = super::maybe_local_file_uri(&config, false, &path).await;
        assert!(uri.is_none());
    }

    #[tokio::test]
    async fn upload_target_defaults_to_multipart() {
        let config = Config::default();
        let path = std::path::Path::new("/tmp/test.mp3");
        assert_eq!(
            super::select_local_upload_target(&config, false, path).await,
            super::UploadFileTarget::Multipart
        );
    }

    #[tokio::test]
    async fn upload_target_uses_local_uri_when_enabled() {
        let mut config = Config::default();
        config.upload_local_file_uri = true;

        let path = create_temp_file();
        let target = super::select_local_upload_target(&config, false, &path).await;
        fs::remove_file(&path).expect("remove temp file");

        match target {
            super::UploadFileTarget::LocalUri(uri) => assert!(uri.starts_with("file://")),
            super::UploadFileTarget::Multipart => panic!("expected local uri"),
        }
    }

    #[test]
    fn build_http_client_returns_client() {
        let client = build_http_client(reqwest::Client::builder()).expect("client should be built");
        let _ = client;
    }

    #[test]
    fn get_upload_bot_returns_error_when_missing() {
        let state = UploadClientState {
            bot: None,
            raw_client: None,
            upload_api_url: String::new(),
            reuse_count: 0,
        };
        assert!(get_upload_bot(&state).is_err());
    }

    #[test]
    fn get_upload_bot_returns_bot_when_present() {
        let bot = Bot::new("token");
        let state = UploadClientState {
            bot: Some(bot),
            raw_client: None,
            upload_api_url: String::new(),
            reuse_count: 0,
        };
        assert!(get_upload_bot(&state).is_ok());
    }

    #[tokio::test]
    async fn acquire_download_permit_returns_error_when_closed() {
        let semaphore = tokio::sync::Semaphore::new(1);
        semaphore.close();

        let err = match acquire_download_permit(&semaphore).await {
            Ok(_) => panic!("expected error for closed semaphore"),
            Err(err) => err,
        };
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
    fn perf_log_includes_stage_label() {
        let s = format_perf("download", std::time::Duration::from_millis(50));
        assert!(s.contains("download"));
    }

    #[test]
    fn structured_perf_line_contains_required_fields() {
        let line = super::format_perf_stage_line(
            "trace-123",
            42,
            "official_api",
            "miss_cold",
            "e2e_total",
            std::time::Duration::from_millis(88),
        );
        assert!(line.starts_with("PERF|"));
        assert!(line.contains("trace_id=trace-123"));
        assert!(line.contains("music_id=42"));
        assert!(line.contains("topology=official_api"));
        assert!(line.contains("cache_path=miss_cold"));
        assert!(line.contains("stage=e2e_total"));
        assert!(line.contains("elapsed_ms=88"));
    }

    #[test]
    fn upload_topology_label_matches_mode() {
        let mut config = Config::default();
        config.upload_local_file_uri = false;
        assert_eq!(super::upload_topology_label(&config, true), "official_api");

        config.upload_local_file_uri = true;
        assert_eq!(
            super::upload_topology_label(&config, false),
            "selfhost_api_uri_upload"
        );

        config.upload_local_file_uri = false;
        assert_eq!(
            super::upload_topology_label(&config, false),
            "selfhost_api_multipart_upload"
        );
    }

    #[test]
    fn critical_path_stage_labels_are_stable() {
        assert_eq!(
            critical_path_stage_labels(),
            ["select_url", "pre_upload_path"]
        );
    }

    #[test]
    fn url_bitrate_candidates_prefers_flac_with_music_u() {
        assert_eq!(
            super::url_bitrate_candidates(true),
            &[999_000, 320_000, 128_000]
        );
    }

    #[test]
    fn url_bitrate_candidates_uses_mp3_without_music_u() {
        assert_eq!(super::url_bitrate_candidates(false), &[320_000, 128_000]);
    }

    #[test]
    fn spawn_gate_identifies_supported_messages() {
        assert!(super::should_spawn_message_task("/start"));
        assert!(
            !super::should_spawn_message_task("   /start"),
            "leading whitespace should not be treated as a command"
        );
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
    fn batch_download_limit_rejects_only_when_over_limit() {
        assert!(!super::exceeds_batch_download_limit(20, 20));
        assert!(super::exceeds_batch_download_limit(21, 20));
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
    fn maintenance_scheduler_emits_cache_prune_signal_on_interval() {
        let counters = super::MaintenanceCounters::new();
        let mut config = crate::config::Config::default();
        config.db_analyze_interval_requests = 0;
        config.memory_release_interval_requests = 0;

        for _ in 0..super::CACHE_PRUNE_INTERVAL_REQUESTS.saturating_sub(1) {
            let _ = super::collect_maintenance_signals(&counters, &config);
        }

        let signals = super::collect_maintenance_signals(&counters, &config);
        assert_eq!(signals, vec![super::MaintenanceSignal::PruneApiCache]);
    }

    #[test]
    fn upload_pool_idle_timeout_disabled_when_zero() {
        assert!(!super::should_set_upload_pool_idle_timeout(0));
        assert!(super::should_set_upload_pool_idle_timeout(60));
    }

    #[test]
    fn download_chunk_bytes_uses_configured_kib() {
        let mut config = Config::default();
        config.download_chunk_size_kb = 512;

        assert_eq!(super::download_chunk_bytes(&config), 512 * 1024);
    }

    #[test]
    fn download_chunk_bytes_clamps_zero_to_minimum() {
        let mut config = Config::default();
        config.download_chunk_size_kb = 0;

        assert_eq!(
            super::download_chunk_bytes(&config),
            super::MIN_DOWNLOAD_CHUNK_BYTES
        );
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
            raw_client: None,
            upload_api_url: String::new(),
            reuse_count: 0,
        };
        let no_bot = UploadClientState {
            bot: None,
            raw_client: None,
            upload_api_url: String::new(),
            reuse_count: 0,
        };
        let exhausted = UploadClientState {
            bot: Some(Bot::new("token")),
            raw_client: None,
            upload_api_url: String::new(),
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
    fn parse_telegram_api_response_returns_error_when_http_200_ok_false() {
        let body = r#"{"ok": false, "description": "chat not found"}"#;
        let err = super::parse_telegram_api_response(body, reqwest::StatusCode::OK, "sendAudio")
            .expect_err("ok=false should be treated as Telegram API error");
        let err_msg = err.to_string();

        assert!(err_msg.contains("chat not found"));
        assert!(err_msg.contains("HTTP 200"));
    }

    #[test]
    fn parse_telegram_api_response_returns_error_when_http_500_for_any_ok_flag() {
        let cases = [
            (r#"{"ok": true, "result": {}}"#, "unknown error"),
            (
                r#"{"ok": false, "description": "server failed"}"#,
                "server failed",
            ),
        ];

        for (body, expected_desc) in cases {
            let err = super::parse_telegram_api_response(
                body,
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                "sendAudio",
            )
            .expect_err("non-2xx status should always be treated as error");
            let err_msg = err.to_string();

            assert!(err_msg.contains(expected_desc));
            assert!(err_msg.contains("HTTP 500"));
        }
    }

    #[test]
    fn parse_telegram_api_response_returns_parse_error_for_non_json_body() {
        let err = super::parse_telegram_api_response(
            "<html>502 bad gateway</html>",
            reqwest::StatusCode::OK,
            "sendAudio",
        )
        .expect_err("non-JSON body should fail response parsing");
        let err_msg = err.to_string();

        assert!(err_msg.contains("Failed to parse upload response"));
    }

    #[test]
    fn parse_telegram_api_response_uses_unknown_error_when_description_missing() {
        let err = super::parse_telegram_api_response(
            r#"{"ok": false}"#,
            reqwest::StatusCode::OK,
            "sendAudio",
        )
        .expect_err("missing description should still return a Telegram API error");
        let err_msg = err.to_string();

        assert!(err_msg.contains("unknown error"));
        assert!(err_msg.contains("HTTP 200"));
    }

    #[test]
    fn extract_file_id_reads_audio_field() {
        let payload = serde_json::json!({
            "ok": true,
            "result": {
                "audio": {
                    "file_id": "audio_file_123"
                }
            }
        });

        assert_eq!(
            super::extract_file_id_from_response(&payload),
            Some("audio_file_123".to_string())
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
        };
        let detail = crate::music_api::SongDetail {
            id: 1,
            name: "Song".to_string(),
            dt: Some(1_000),
            ar: Some(vec![]),
            al: None,
        };

        let tagged = super::apply_tags_in_blocking(buffer, "bin", Arc::new(detail), None, false)
            .await
            .expect("unknown format should keep buffer unchanged");

        assert_eq!(tagged.size().await, 3);
    }

    #[tokio::test]
    async fn tagging_wrapper_adds_mp3_id3_header() {
        let buffer = crate::audio_buffer::AudioBuffer::Memory {
            data: vec![0xFF, 0xFB, 0x90, 0x64],
            filename: "sample.mp3".to_string(),
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

        let tagged = super::apply_tags_in_blocking(buffer, "mp3", Arc::new(detail), None, false)
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

    #[test]
    fn parse_command_and_args_handles_bot_mention_and_whitespace() {
        let (cmd, args) = super::parse_command_and_args("/search@mybot    hello world");
        assert_eq!(cmd, "search");
        assert_eq!(args.as_deref(), Some("hello world"));

        let (cmd, args) = super::parse_command_and_args("/status@mybot");
        assert_eq!(cmd, "status");
        assert_eq!(args, None);

        let (cmd, args) = super::parse_command_and_args("/start   ");
        assert_eq!(cmd, "start");
        assert_eq!(args, None);
    }

    #[test]
    fn runtime_metrics_cache_hit_rate_tracks_counts() {
        let metrics = super::RuntimeMetrics::new();
        metrics.record_cache_hit();
        metrics.record_cache_hit();
        metrics.record_cache_miss();

        let snapshot = metrics.cache_snapshot();
        assert_eq!(snapshot.hits, 2);
        assert_eq!(snapshot.misses, 1);
        assert!((snapshot.hit_rate_percent - 66.67).abs() < 0.1);
    }

    #[test]
    fn runtime_metrics_speed_window_keeps_recent_samples() {
        let metrics = super::RuntimeMetrics::new();
        for mb in 1..=25u64 {
            metrics.record_download_speed(mb * 1024 * 1024, Duration::from_secs(1));
        }

        let (download, upload) = metrics.speed_snapshots();
        let download = download.expect("download snapshot should exist");
        assert!(upload.is_none(), "upload should have no samples");
        assert_eq!(download.samples, 25);
        assert_eq!(download.recent_samples, 20);
        assert!((download.last_mbps - 25.0).abs() < 0.01);
        assert!(download.p95_mbps >= 24.0);
    }

    #[test]
    fn speed_line_reports_cache_hit_when_no_samples() {
        let line = super::format_speed_line("下载", None);
        assert!(line.contains("暂无非缓存测速样本"));
    }

    #[test]
    fn speed_line_uses_monospace_for_numeric_values() {
        let line = super::format_speed_line(
            "下载",
            Some(super::SpeedSnapshot {
                last_mbps: 6.0,
                avg_mbps: 4.5,
                p95_mbps: 5.2,
                samples: 12,
                recent_samples: 12,
            }),
        );
        assert!(line.contains("<code>6.00</code>"));
        assert!(line.contains("<code>4.50</code>"));
        assert!(line.contains("<code>5.20</code>"));
        assert!(line.contains("<code>12</code>"));
    }

    #[test]
    fn status_text_uses_section_layout_and_split_memory_fields() {
        let cache_snapshot = super::CacheSnapshot {
            hits: 9,
            misses: 3,
            hit_rate_percent: 75.0,
        };
        let resource_snapshot = super::ResourceSnapshot {
            cpu_percent: 12.5,
            system_used_memory_mb: 512,
            system_total_memory_mb: 1024,
            bot_memory_mb: Some(12),
        };
        let text = super::build_status_text(
            100,
            20,
            8,
            cache_snapshot,
            resource_snapshot,
            "00:10:00",
            "下载: 实时 <code>6.00</code> MB/s | 平均 <code>4.00</code> MB/s | P95 <code>5.00</code> MB/s | 样本 <code>12</code> (窗口 <code>12</code>)",
            "上传: 实时 <code>2.00</code> MB/s | 平均 <code>1.50</code> MB/s | P95 <code>1.80</code> MB/s | 样本 <code>12</code> (窗口 <code>12</code>)",
        );
        assert!(text.contains("<b>系统状态</b>"));
        assert!(text.contains("<b>实时运行指标</b>"));
        assert!(text.contains("<b>💾 缓存</b>"));
        assert!(text.contains("• 总缓存: <code>100</code>"));
        assert!(text.contains("• 用户缓存: <code>20</code>"));
        assert!(text.contains("• 群组缓存: <code>8</code>"));
        assert!(text.contains("• 系统内存: <code>512/1024 MB</code>"));
        assert!(text.contains("• Bot 内存: <code>12 MB</code>"));
        assert!(text.contains("• 下载: 实时 <code>6.00</code> MB/s"));
    }

    #[test]
    fn rmcache_usage_prompt_uses_html_code() {
        let text = super::rmcache_usage_prompt();
        assert!(text.contains("<code>/rmcache &lt;音乐ID&gt;</code>"));
    }

    #[test]
    fn clearallcache_confirmation_prompt_uses_html_code() {
        let text = super::clearallcache_confirmation_prompt();
        assert!(text.contains("<code>/clearallcache confirm</code>"));
    }

    #[test]
    fn about_text_includes_build_commit_in_version_line() {
        let text = super::build_about_text();
        assert!(text.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains(&format!("({})", super::BUILD_GIT_COMMIT)));
    }

    #[test]
    fn is_spawnable_command_text_requires_leading_slash() {
        assert!(super::is_spawnable_command_text("/start"));
        assert!(super::is_spawnable_command_text("/music 123"));
        assert!(!super::is_spawnable_command_text("  /start"));
        assert!(!super::is_spawnable_command_text("hello"));
    }

    #[test]
    fn is_command_text_requires_leading_slash() {
        assert!(super::is_command_text("/start"));
        assert!(!super::is_command_text("  /start"));
        assert!(!super::is_command_text("hello"));
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

    if let Some(target) = parse_music_collection_target(text) {
        return process_music_collection(bot, msg, state, target).await;
    }

    let Some(url) = extract_first_url(text) else {
        send_reply_text(bot, msg, MUSIC_ID_EXTRACT_FAILED_TEXT).await?;
        return Ok(());
    };

    if let Some(music_id) = parse_music_id(&url) {
        return process_music(bot, msg, state, music_id).await;
    }
    if let Some(target) = parse_music_collection_target(&url) {
        return process_music_collection(bot, msg, state, target).await;
    }

    let final_url = match state.music_api.resolve_share_link(&url).await {
        Ok(final_url) => final_url.to_string(),
        Err(e) => {
            tracing::warn!("Failed to resolve share link: {}", e);
            send_reply_text(bot, msg, MUSIC_ID_EXTRACT_FAILED_TEXT).await?;
            return Ok(());
        }
    };

    if let Some(music_id) = parse_music_id(&final_url) {
        process_music(bot, msg, state, music_id).await
    } else if let Some(target) = parse_music_collection_target(&final_url) {
        process_music_collection(bot, msg, state, target).await
    } else {
        send_reply_text(bot, msg, MUSIC_ID_EXTRACT_FAILED_TEXT).await?;
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
            send_reply_text(bot, msg, "请输入搜索关键词").await?;
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
            let mut buttons = Vec::with_capacity(songs.len().min(8));

            for (i, song) in songs.iter().take(8).enumerate() {
                let artists = format_artists(&song.artists);
                append_search_result_line(&mut results, i + 1, &song.name, &artists);
                buttons.push(InlineKeyboardButton::callback(
                    (i + 1).to_string(),
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

const BUILD_GIT_COMMIT: &str = match option_env!("BUILD_GIT_COMMIT") {
    Some(value) => value,
    None => "unknown",
};

fn build_about_text() -> String {
    format!(
        r"🎵 Music163bot-Rust v{} ({})

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
        env!("CARGO_PKG_VERSION"),
        BUILD_GIT_COMMIT
    )
}

async fn handle_about_command(
    bot: &Bot,
    msg: &Message,
    _state: &Arc<BotState>,
) -> ResponseResult<()> {
    let about_text = build_about_text();

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
        send_reply_text(bot, msg, "请输入歌曲ID或关键词").await?;
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
                    send_reply_text(bot, msg, "未找到相关歌曲").await?;
                    return Ok(());
                }
            }
            Err(e) => {
                send_reply_text(bot, msg, format!("搜索失败: {e}")).await?;
                return Ok(());
            }
        }
    };

    let status_msg = bot
        .send_message(msg.chat.id, "🎵 正在获取歌词...")
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    match join_futures(
        state.music_api.get_song_lyric(music_id),
        state.music_api.get_song_detail(music_id),
    )
    .await
    {
        (Ok(lyric), detail_result) => {
            if lyric.trim().is_empty() || lyric == "No lyrics available" {
                bot.edit_message_text(msg.chat.id, status_msg.id, "该歌曲暂无歌词")
                    .await?;
                return Ok(());
            }

            // Get song detail for filename
            let song_detail = match detail_result {
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
            let lyric_bytes = Bytes::from(lyric.into_bytes());

            let (client_result, permit_result) = join_futures(
                acquire_upload_client(state),
                acquire_upload_permit(&state.upload_semaphore),
            )
            .await;

            let (_upload_bot, raw_client, api_base_url) = match client_result {
                Ok(bundle) => bundle,
                Err(e) => {
                    bot.edit_message_text(
                        msg.chat.id,
                        status_msg.id,
                        format!("初始化上传客户端失败: {e}"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let _upload_permit = match permit_result {
                Ok(permit) => permit,
                Err(e) => {
                    bot.edit_message_text(
                        msg.chat.id,
                        status_msg.id,
                        format!("等待上传通道失败: {e}"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let params = RawDocumentParams {
                chat_id: msg.chat.id.0,
                caption: None,
                reply_to_message_id: msg.id.0,
                reply_markup_json: None,
            };

            let upload_result = raw_send_document_bytes(
                &raw_client,
                &api_base_url,
                &lrc_filename,
                lyric_bytes,
                &params,
            )
            .await;

            match upload_result {
                Ok(_) => {
                    bot.delete_message(msg.chat.id, status_msg.id).await.ok();
                }
                Err(e) => {
                    bot.edit_message_text(msg.chat.id, status_msg.id, format!("发送歌词失败: {e}"))
                        .await?;
                }
            }
        }
        (Err(e), _) => {
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
    let cache_snapshot = state.runtime_metrics.cache_snapshot();
    let resource_snapshot = sample_resource_snapshot();
    let (download_speed, upload_speed) = state.runtime_metrics.speed_snapshots();
    let uptime = format_uptime(state.runtime_metrics.uptime());
    let download_line = format_speed_line("下载", download_speed);
    let upload_line = format_speed_line("上传", upload_speed);
    let status_text = build_status_text(
        total_count,
        user_count,
        chat_count,
        cache_snapshot,
        resource_snapshot,
        &uptime,
        &download_line,
        &upload_line,
    );

    bot.send_message(msg.chat.id, status_text)
        .parse_mode(ParseMode::Html)
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

    if !ensure_admin(bot, msg, &state.config).await? {
        return Ok(());
    }

    let args = args.unwrap_or_default();

    if args.is_empty() {
        send_reply_html(bot, msg, rmcache_usage_prompt()).await?;
        return Ok(());
    }

    if let Some(music_id) = parse_music_id(&args) {
        let music_id_i64 = music_id as i64;

        // Get song info before deletion
        if let Ok(Some(song_info)) = state.database.get_song_by_music_id(music_id_i64).await {
            match state.database.delete_song_by_music_id(music_id_i64).await {
                Ok(deleted) => {
                    if deleted {
                        send_reply_text(
                            bot,
                            msg,
                            format!("✅ 已删除歌曲缓存: {}", song_info.song_name),
                        )
                        .await?;
                    } else {
                        send_reply_text(bot, msg, "歌曲未缓存").await?;
                    }
                }
                Err(e) => {
                    send_reply_text(bot, msg, format!("删除缓存失败: {e}")).await?;
                }
            }
        } else {
            send_reply_text(bot, msg, "歌曲未缓存").await?;
        }
    } else {
        send_reply_text(bot, msg, "无效的歌曲ID").await?;
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

    if !ensure_admin(bot, msg, &state.config).await? {
        return Ok(());
    }

    // Send confirmation message
    send_reply_html(bot, msg, clearallcache_confirmation_prompt()).await?;

    Ok(())
}

async fn handle_clearallcache_confirm_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    // Check if user is admin
    let user_id = msg.from.as_ref().map_or(0, |u| u.id.0 as i64);

    if !ensure_admin(bot, msg, &state.config).await? {
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
    if let Some(data) = query.data
        && let Some((cmd, rest)) = data.split_once(' ')
        && cmd == "music"
        && let Ok(music_id) = rest.trim_start().parse::<u64>()
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
            let mut results = Vec::with_capacity(songs.len().min(10));

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
    let kbps = format_bitrate_kbps(bitrate_bps);
    format!(
        "「{title}」- {artists}\n专辑: {album}\n#网易云音乐 #{file_ext} {size_mb:.2}MB {kbps}kbps\nvia @{bot_username}",
    )
}

#[must_use]
fn format_bitrate_kbps(bitrate_bps: i64) -> String {
    let bitrate_bps = bitrate_bps.max(0) as f64;
    format!("{:.2}", bitrate_bps / 1000.0)
}
