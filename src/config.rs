use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Storage mode for temporary files during download processing
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    /// Traditional disk file storage (stable, low memory, compatible with all scenarios)
    #[default]
    Disk,
    /// In-memory processing (faster, reduces disk I/O, requires sufficient RAM)
    Memory,
    /// Smart selection based on file size and available memory (recommended)
    Hybrid,
}

/// Cover art handling mode for downloads
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CoverMode {
    /// Only download a thumbnail for Telegram display
    #[default]
    Thumbnail,
    /// Only download original cover art for embedding
    Original,
    /// Download both original and thumbnail (legacy behavior)
    Both,
}

impl std::str::FromStr for CoverMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("thumbnail") {
            Ok(Self::Thumbnail)
        } else if s.eq_ignore_ascii_case("original") {
            Ok(Self::Original)
        } else if s.eq_ignore_ascii_case("both") {
            Ok(Self::Both)
        } else {
            Err(anyhow::anyhow!("Invalid cover mode: {s}"))
        }
    }
}

impl std::str::FromStr for StorageMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("disk") {
            Ok(Self::Disk)
        } else if s.eq_ignore_ascii_case("memory") {
            Ok(Self::Memory)
        } else if s.eq_ignore_ascii_case("hybrid") {
            Ok(Self::Hybrid)
        } else {
            Err(anyhow::anyhow!("Invalid storage mode: {s}"))
        }
    }
}

impl std::fmt::Display for StorageMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disk => write!(f, "disk"),
            Self::Memory => write!(f, "memory"),
            Self::Hybrid => write!(f, "hybrid"),
        }
    }
}

fn parse_bool_like(value: &str) -> Option<bool> {
    match value.trim() {
        v if v.eq_ignore_ascii_case("true") => Some(true),
        v if v.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// Parse a string value into type T, returning `default` and logging a warning on failure.
fn parse_field<T: std::str::FromStr>(value: &str, default: T, key: &str) -> T {
    value.parse().unwrap_or_else(|_| {
        tracing::warn!("Invalid {key} '{value}', using default");
        default
    })
}

/// Parse a boolean config field, updating `target` on success and logging a warning on failure.
fn apply_bool_field(value: &str, target: &mut bool, key: &str) {
    if let Some(parsed) = parse_bool_like(value) {
        *target = parsed;
    } else {
        tracing::warn!("Invalid {key} '{value}', using default {target}");
    }
}

fn parse_admin_list(admins: &str) -> Vec<i64> {
    if admins.trim().is_empty() {
        return Vec::new();
    }
    admins
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub bot_token: String,
    pub music_u: Option<String>,

    pub bot_api: String,
    pub music_api: String,
    pub bot_admin: Vec<i64>,
    pub database: String,
    pub log_level: String,
    pub cache_dir: String,
    pub auto_update: bool,
    pub auto_retry: bool,
    pub max_retry_times: u32,
    pub download_timeout: u64,
    pub check_md5: bool,

    /// Storage mode for temporary files: disk, memory, or hybrid
    pub storage_mode: StorageMode,
    /// Memory threshold in MB for hybrid mode (files larger than this use disk)
    pub memory_threshold_mb: u64,
    /// Memory buffer in MB (available memory must exceed file size + buffer to use memory mode)
    pub memory_buffer_mb: u64,
    /// Maximum file size in MB allowed for memory mode (larger files use disk)
    pub memory_max_file_mb: u64,
    /// Maximum concurrent downloads (lower = less memory, higher = more throughput)
    pub max_concurrent_downloads: u32,
    /// Maximum tracks allowed for a single playlist/album download request
    pub max_batch_download_tracks: u32,
    /// Max idle connections per host for download client
    pub download_pool_max_idle_per_host: usize,
    /// Download connect timeout (seconds)
    pub download_connect_timeout_secs: u64,
    /// Download chunk size in KB for buffering
    pub download_chunk_size_kb: usize,
    /// Cover art mode: thumbnail, original, or both
    pub cover_mode: CoverMode,
    /// Upload client reuse request limit
    pub upload_client_reuse_requests: u32,
    /// Max concurrent uploads
    pub upload_max_concurrent: u32,
    /// Upload pool max idle connections per host
    pub upload_pool_max_idle_per_host: usize,
    /// Upload pool idle timeout (seconds)
    pub upload_pool_idle_timeout_secs: u64,
    /// Upload timeout (seconds)
    pub upload_timeout_secs: u64,
    /// Use file:// URIs for local uploads (telegram-bot-api --local only)
    pub upload_local_file_uri: bool,
    /// Memory release interval in handled requests
    pub memory_release_interval_requests: u32,
    /// Database analyze interval in handled requests
    pub db_analyze_interval_requests: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            music_u: None,
            bot_api: "https://api.telegram.org".to_string(),
            music_api: "https://music.163.com".to_string(),
            bot_admin: Vec::new(),
            database: "cache.db".to_string(),
            log_level: "info".to_string(),
            cache_dir: "./cache".to_string(),
            auto_update: true,
            auto_retry: true,
            max_retry_times: 3,
            download_timeout: 60,
            check_md5: true,
            storage_mode: StorageMode::Disk,
            memory_threshold_mb: 100,
            memory_buffer_mb: 100,
            memory_max_file_mb: 100,
            max_concurrent_downloads: 4,
            max_batch_download_tracks: 20,
            download_pool_max_idle_per_host: 2,
            download_connect_timeout_secs: 10,
            download_chunk_size_kb: 256,
            cover_mode: CoverMode::Thumbnail,
            upload_client_reuse_requests: 0,
            upload_max_concurrent: 1,
            upload_pool_max_idle_per_host: 1,
            upload_pool_idle_timeout_secs: 300,
            upload_timeout_secs: 300,
            upload_local_file_uri: false,
            memory_release_interval_requests: 10,
            db_analyze_interval_requests: 20,
        }
    }
}

mod load;

#[cfg(test)]
mod tests;
