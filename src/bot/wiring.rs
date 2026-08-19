use super::{
    Arc, AtomicBool, AtomicU32, AtomicU64, Bot, ChatId, Config, DashMap, Database, Instant,
    LazyLock, MusicApi, Mutex, Notify, Ordering, System, VecDeque, percentile_95,
    sample_current_process_memory_mb, throughput_mbps, u64_to_f64,
};

pub(super) struct BotState {
    pub config: Config,
    pub database: Database,
    pub music_api: Arc<MusicApi>,
    pub(super) inflight_downloads: Arc<InflightDownloads>,
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
    pub clearallcache_confirms: Arc<DashMap<(i64, ChatId), std::time::Instant>>,
    /// Per-chat language override cache (`chat_id` -> locale), backed by
    /// the `chat_settings` table. See ADR-0001.
    pub chat_languages: Arc<DashMap<i64, String>>,
}

#[derive(Debug)]
pub(super) struct UploadClientState {
    pub bot: Option<Bot>,
    pub raw_client: Option<reqwest::Client>,
    pub upload_api_url: String,
    pub reuse_count: u32,
}

#[derive(Debug, Default)]
pub(super) struct UploadCounters {
    pub in_flight: AtomicU32,
    pub peak_in_flight: AtomicU32,
}

/// Audio file format — replaces stringly-typed `file_ext` dispatch in the tagging path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioFormat {
    Mp3,
    Flac,
}

impl AudioFormat {
    /// Canonical lowercase extension string, used for filenames and DB storage.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
        }
    }
}

impl std::fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MusicLinkTarget {
    Song(u64),
    Program(u64),
}

pub(super) const SPEED_SAMPLE_WINDOW: usize = 20;
pub(super) const STATUS_RESOURCE_REFRESH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(2);
pub(super) const MIN_DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;
pub(super) const MAINTENANCE_QUEUE_CAPACITY: usize = 32;
pub(super) const CACHE_PRUNE_INTERVAL_REQUESTS: u32 = 50;
pub(super) const PERF_LOG_PREFIX: &str = "PERF";
pub(super) const PERF_STAGE_CACHE_LOOKUP: &str = "cache_lookup";
pub(super) const PERF_STAGE_SINGLEFLIGHT_WAIT: &str = "singleflight_wait";
pub(super) const PERF_STAGE_COVER_DOWNLOAD: &str = "cover_download";
pub(super) const PERF_STAGE_DOWNLOAD_AUDIO: &str = "download_audio";
pub(super) const PERF_STAGE_UPLOAD_PERMIT_WAIT: &str = "upload_permit_wait";
pub(super) const PERF_STAGE_UPLOAD_CLIENT_ACQUIRE: &str = "upload_client_acquire";
pub(super) const PERF_STAGE_UPLOAD_SEND: &str = "upload_send";
pub(super) const PERF_STAGE_TAG_PROCESS: &str = "tag_process";
pub(super) const PERF_STAGE_DB_SAVE: &str = "db_save";
pub(super) const PERF_STAGE_E2E_TOTAL: &str = "e2e_total";
pub(super) static PERF_TRACE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub(super) struct PerfTraceContext {
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

    pub(super) fn with_cache_path(&self, cache_path: &'static str) -> Self {
        Self {
            cache_path,
            ..self.clone()
        }
    }

    pub(super) fn log_stage(&self, stage: &str, duration: std::time::Duration) {
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

pub(super) fn build_perf_trace_context(
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

pub(super) fn next_perf_trace_id() -> String {
    let ts_millis = chrono::Utc::now().timestamp_millis();
    let seq = PERF_TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{ts_millis:x}-{seq:x}")
}

pub(super) fn format_perf_stage_line(
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

pub(super) fn upload_topology_label(config: &Config, is_official_api: bool) -> &'static str {
    if is_official_api {
        "official_api"
    } else if config.flags.upload_local_file_uri() {
        "selfhost_api_uri_upload"
    } else {
        "selfhost_api_multipart_upload"
    }
}

#[derive(Debug)]
pub(super) struct RuntimeMetrics {
    started_at: Instant,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    speed_metrics: std::sync::Mutex<SpeedMetrics>,
}

#[derive(Debug, Default)]
pub(super) struct SpeedMetrics {
    download: DirectionSpeedMetrics,
    upload: DirectionSpeedMetrics,
}

#[derive(Debug, Default)]
pub(super) struct DirectionSpeedMetrics {
    recent_mbps: VecDeque<f64>,
    total_bytes: u128,
    total_nanos: u128,
    pub(super) samples: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CacheSnapshot {
    pub(super) hits: u64,
    pub(super) misses: u64,
    pub(super) hit_rate_percent: f64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SpeedSnapshot {
    pub(super) last_mbps: f64,
    pub(super) avg_mbps: f64,
    pub(super) p95_mbps: f64,
    pub(super) samples: u64,
    pub(super) recent_samples: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ResourceSnapshot {
    pub(super) cpu_percent: f32,
    pub(super) system_used_memory_mb: u64,
    pub(super) system_total_memory_mb: u64,
    pub(super) bot_memory_mb: Option<u64>,
}

pub(super) static STATUS_RESOURCE_CACHE: LazyLock<
    std::sync::Mutex<(System, Instant, ResourceSnapshot)>,
> = LazyLock::new(|| {
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
pub(super) struct MaintenanceCounters {
    pub memory_release: AtomicU32,
    pub db_analyze: AtomicU32,
    pub api_cache_prune: AtomicU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MaintenanceSignal {
    AnalyzeDb,
    ReleaseMemory,
    PruneApiCache,
}

#[derive(Debug, Default)]
pub(super) struct InflightDownloads {
    entries: DashMap<u64, Arc<InflightEntry>>,
}

#[derive(Debug)]
pub(super) struct InflightEntry {
    notify: Notify,
    done: AtomicBool,
}

#[derive(Debug)]
pub(super) enum InflightClaim {
    Leader(InflightLeaderGuard),
    Follower(Arc<InflightEntry>),
}

#[derive(Debug)]
pub(super) struct InflightLeaderGuard {
    music_id: u64,
    inflight: Arc<InflightDownloads>,
}

impl InflightDownloads {
    pub(super) fn begin(self: &Arc<Self>, music_id: u64) -> InflightClaim {
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

    pub(super) fn finish(&self, music_id: u64) {
        if let Some((_, entry)) = self.entries.remove(&music_id) {
            entry.finish();
        }
    }
}

impl InflightEntry {
    pub(super) fn new() -> Self {
        Self {
            notify: Notify::new(),
            done: AtomicBool::new(false),
        }
    }

    pub(super) async fn wait(&self) {
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

    pub(super) fn finish(&self) {
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

pub(super) fn lock_unpoisoned<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
type InflightWaitHook = Box<dyn FnOnce() + Send + 'static>;

#[cfg(test)]
pub(super) static INFLIGHT_WAIT_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<InflightWaitHook>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(super) fn set_inflight_wait_hook(hook: impl FnOnce() + Send + 'static) {
    let slot = INFLIGHT_WAIT_HOOK.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = lock_unpoisoned(slot);
    *guard = Some(Box::new(hook));
}

#[cfg(test)]
pub(super) fn take_inflight_wait_hook() -> Option<InflightWaitHook> {
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
    pub(super) fn new() -> Self {
        Self {
            memory_release: AtomicU32::new(0),
            db_analyze: AtomicU32::new(0),
            api_cache_prune: AtomicU32::new(0),
        }
    }

    pub(super) fn should_run(counter: &AtomicU32, interval: u32) -> bool {
        if interval == 0 {
            return false;
        }
        let next = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        next.is_multiple_of(interval)
    }
}

impl RuntimeMetrics {
    pub(super) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            speed_metrics: std::sync::Mutex::new(SpeedMetrics::default()),
        }
    }

    pub(super) fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn cache_snapshot(&self) -> CacheSnapshot {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let total = hits.saturating_add(misses);
        let hit_rate_percent = if total == 0 {
            0.0
        } else {
            u64_to_f64(hits) * 100.0 / u64_to_f64(total)
        };

        CacheSnapshot {
            hits,
            misses,
            hit_rate_percent,
        }
    }

    pub(super) fn record_download_speed(&self, bytes: u64, duration: std::time::Duration) {
        self.record_speed(Direction::Download, bytes, duration);
    }

    pub(super) fn record_upload_speed(&self, bytes: u64, duration: std::time::Duration) {
        self.record_speed(Direction::Upload, bytes, duration);
    }

    fn record_speed(&self, direction: Direction, bytes: u64, duration: std::time::Duration) {
        let mut guard = lock_unpoisoned(&self.speed_metrics);
        match direction {
            Direction::Download => guard.download.record(bytes, duration),
            Direction::Upload => guard.upload.record(bytes, duration),
        }
    }

    pub(super) fn speed_snapshots(&self) -> (Option<SpeedSnapshot>, Option<SpeedSnapshot>) {
        let guard = lock_unpoisoned(&self.speed_metrics);
        (guard.download.snapshot(), guard.upload.snapshot())
    }

    pub(super) fn uptime(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
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
        let total_bytes_f64 = u64_to_f64(u64::try_from(self.total_bytes).unwrap_or(u64::MAX));
        let total_nanos_f64 = u64_to_f64(u64::try_from(self.total_nanos).unwrap_or(u64::MAX));
        let avg_mbps = (total_bytes_f64 / (1024.0 * 1024.0)) / (total_nanos_f64 / 1e9);
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
