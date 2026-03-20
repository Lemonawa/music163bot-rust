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
use crate::music_api::{MusicApi, ProgramMainTrack, format_artists};
use crate::utils::{
    MusicCollectionTarget, build_http_client, clean_filename, ensure_dir, extract_first_url,
    parse_music_collection_target, parse_music_id, parse_music_program_id,
    sanitize_sensitive_text, throughput_mbps, update_peak,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MusicLinkTarget {
    Song(u64),
    Program(u64),
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

fn lock_unpoisoned<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
static INFLIGHT_WAIT_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn set_inflight_wait_hook(hook: impl FnOnce() + Send + 'static) {
    let slot = INFLIGHT_WAIT_HOOK.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = lock_unpoisoned(slot);
    *guard = Some(Box::new(hook));
}

#[cfg(test)]
fn take_inflight_wait_hook() -> Option<Box<dyn FnOnce() + Send + 'static>> {
    let slot = INFLIGHT_WAIT_HOOK.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = lock_unpoisoned(slot);
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
        let mut guard = lock_unpoisoned(&self.speed_metrics);
        match direction {
            Direction::Download => guard.download.record(bytes, duration),
            Direction::Upload => guard.upload.record(bytes, duration),
        }
    }

    fn speed_snapshots(&self) -> (Option<SpeedSnapshot>, Option<SpeedSnapshot>) {
        let guard = lock_unpoisoned(&self.speed_metrics);
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
