use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Storage mode for temporary files during download processing
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    /// Traditional disk file storage (stable, low memory, compatible with all scenarios)
    Disk,
    /// In-memory processing (faster, reduces disk I/O, requires sufficient RAM)
    Memory,
    /// Smart selection based on file size and available memory (recommended)
    Hybrid,
}

/// Cover art handling mode for downloads
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CoverMode {
    /// Only download a thumbnail for Telegram display
    Thumbnail,
    /// Only download original cover art for embedding
    Original,
    /// Download both original and thumbnail (legacy behavior)
    Both,
}

impl Default for CoverMode {
    fn default() -> Self {
        Self::Thumbnail
    }
}

impl std::str::FromStr for CoverMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "thumbnail" => Ok(Self::Thumbnail),
            "original" => Ok(Self::Original),
            "both" => Ok(Self::Both),
            _ => Err(anyhow::anyhow!("Invalid cover mode: {s}")),
        }
    }
}

impl Default for StorageMode {
    fn default() -> Self {
        Self::Disk // Backward compatible default
    }
}

impl std::str::FromStr for StorageMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "disk" => Ok(Self::Disk),
            "memory" => Ok(Self::Memory),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(anyhow::anyhow!("Invalid storage mode: {s}")),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // Required fields
    pub bot_token: String,
    pub music_u: Option<String>,

    // Optional fields with defaults
    pub bot_api: String,
    pub music_api: String,
    pub bot_admin: Vec<i64>,
    pub bot_debug: bool,
    pub database: String,
    pub log_level: String,
    pub cache_dir: String,
    pub auto_update: bool,
    pub auto_retry: bool,
    pub max_retry_times: u32,
    pub download_timeout: u64,
    pub check_md5: bool,

    // Smart storage settings (v1.1.0+)
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
    /// Upload timeout (seconds)
    pub upload_timeout_secs: u64,
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
            bot_debug: false,
            database: "cache.db".to_string(),
            log_level: "info".to_string(),
            cache_dir: "./cache".to_string(),
            auto_update: true,
            auto_retry: true,
            max_retry_times: 3,
            download_timeout: 60,
            check_md5: true,
            // Smart storage defaults (v1.1.0+)
            storage_mode: StorageMode::Disk, // Backward compatible
            memory_threshold_mb: 100,
            memory_buffer_mb: 100,
            memory_max_file_mb: 100,
            max_concurrent_downloads: 3, // 从 10 减少到 3，减少内存峰值
            download_pool_max_idle_per_host: 2,
            download_connect_timeout_secs: 10,
            download_chunk_size_kb: 256,
            cover_mode: CoverMode::Thumbnail,
            upload_client_reuse_requests: 50,
            upload_timeout_secs: 300,
            memory_release_interval_requests: 10,
            db_analyze_interval_requests: 20,
        }
    }
}

impl Config {
    pub fn load(config_path: &str) -> Result<Self> {
        let mut config = Config::default();

        if !std::path::Path::new(config_path).exists() {
            tracing::warn!("Config file {} not found, using defaults", config_path);
            return Ok(config);
        }

        let file = File::open(config_path)?;
        let reader = BufReader::new(file);
        let mut config_map = HashMap::new();
        let mut current_section = String::new();

        // Parse INI-like format with sections
        for line in reader.lines() {
            let line = line?;
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Check for section headers [section]
            if line.starts_with('[') && line.ends_with(']') {
                current_section = line[1..line.len() - 1].to_string();
                continue;
            }

            // Parse key=value pairs
            if let Some(pos) = line.find('=') {
                let key = line[..pos].trim().to_lowercase();
                let value = line[pos + 1..].trim().to_string();

                // Create full key with section prefix
                let full_key = if current_section.is_empty() {
                    key
                } else {
                    format!("{current_section}.{key}")
                };

                config_map.insert(full_key, value);
            }
        }

        // Map configuration values
        if let Some(token) = config_map.get("bot.token") {
            config.bot_token.clone_from(token);
        }

        config.music_u = config_map.get("music.music_u").cloned();

        if let Some(api) = config_map.get("bot.api") {
            config.bot_api.clone_from(api);
        }

        if let Some(api) = config_map.get("music.api") {
            config.music_api.clone_from(api);
        }

        if let Some(url) = config_map.get("database.url") {
            config.database.clone_from(url);
        }

        if let Some(dir) = config_map.get("download.dir") {
            config.cache_dir.clone_from(dir);
        }

        if let Some(admins) = config_map.get("bot.botadmin") {
            config.bot_admin = admins
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            tracing::info!("Loaded bot admins: {:?}", config.bot_admin);
        } else if let Some(admins) = config_map.get("bot.admin") {
            // Support alternative config key "bot.admin"
            config.bot_admin = admins
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            tracing::info!("Loaded bot admins (from bot.admin): {:?}", config.bot_admin);
        }

        if let Some(debug) = config_map.get("botdebug") {
            config.bot_debug = debug.to_lowercase() == "true";
        }

        if let Some(db) = config_map.get("database") {
            config.database.clone_from(db);
        }

        if let Some(level) = config_map.get("loglevel") {
            config.log_level.clone_from(level);
        }

        if let Some(auto_update) = config_map.get("autoupdate") {
            config.auto_update = auto_update.to_lowercase() == "true";
        }

        if let Some(auto_retry) = config_map.get("autoretry") {
            config.auto_retry = auto_retry.to_lowercase() == "true";
        }

        if let Some(max_retry) = config_map.get("maxretrytimes") {
            config.max_retry_times = max_retry.parse().unwrap_or(3);
        }

        if let Some(timeout) = config_map.get("downloadtimeout") {
            config.download_timeout = timeout.parse().unwrap_or(60);
        }

        if let Some(check_md5) = config_map.get("checkmd5") {
            config.check_md5 = check_md5.to_lowercase() == "true";
        }

        // Smart storage settings (v1.1.0+)
        if let Some(mode) = config_map.get("download.storage_mode") {
            match mode.parse::<StorageMode>() {
                Ok(m) => config.storage_mode = m,
                Err(e) => tracing::warn!("Invalid storage_mode '{}': {}, using default", mode, e),
            }
        }
        if let Some(threshold) = config_map.get("download.memory_threshold") {
            config.memory_threshold_mb = threshold.parse().unwrap_or(100);
        }
        if let Some(buffer) = config_map.get("download.memory_buffer") {
            config.memory_buffer_mb = buffer.parse().unwrap_or(100);
        }
        if let Some(max_file) = config_map.get("download.memory_max_file_mb") {
            config.memory_max_file_mb = max_file.parse().unwrap_or(64);
        }
        if let Some(concurrent) = config_map.get("download.max_concurrent") {
            config.max_concurrent_downloads = concurrent.parse().unwrap_or(3);
        }

        if let Some(pool_size) = config_map.get("download.pool_max_idle_per_host") {
            config.download_pool_max_idle_per_host = pool_size.parse().unwrap_or(2);
        }
        if let Some(timeout) = config_map.get("download.connect_timeout_secs") {
            config.download_connect_timeout_secs = timeout.parse().unwrap_or(10);
        }
        if let Some(chunk_kb) = config_map.get("download.chunk_size_kb") {
            config.download_chunk_size_kb = chunk_kb.parse().unwrap_or(256);
        }
        if let Some(mode) = config_map.get("download.cover_mode") {
            match mode.parse::<CoverMode>() {
                Ok(m) => config.cover_mode = m,
                Err(e) => tracing::warn!("Invalid cover_mode '{}': {}, using default", mode, e),
            }
        }

        if let Some(reuse_requests) = config_map.get("upload.client_reuse_requests") {
            config.upload_client_reuse_requests = reuse_requests.parse().unwrap_or(50);
        }
        if let Some(timeout) = config_map.get("upload.timeout_secs") {
            config.upload_timeout_secs = timeout.parse().unwrap_or(300);
        }

        if let Some(interval) = config_map.get("maintenance.memory_release_interval_requests") {
            config.memory_release_interval_requests = interval.parse().unwrap_or(1);
        }
        if let Some(interval) = config_map.get("maintenance.db_analyze_interval_requests") {
            config.db_analyze_interval_requests = interval.parse().unwrap_or(1);
        }

        // Validate required fields
        if config.bot_token.is_empty() {
            return Err(anyhow::anyhow!("BOT_TOKEN is required"));
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, CoverMode};

    #[test]
    fn download_pool_defaults_are_tunable() {
        let config = Config::default();
        assert!(config.download_pool_max_idle_per_host > 0);
        assert!(config.download_connect_timeout_secs > 0);
    }

    #[test]
    fn download_chunk_size_has_default() {
        let config = Config::default();
        assert!(config.download_chunk_size_kb >= 64);
    }

    #[test]
    fn memory_max_file_has_default() {
        let config = Config::default();
        assert_eq!(config.memory_max_file_mb, 100);
    }

    #[test]
    fn upload_client_reuse_has_default() {
        let config = Config::default();
        assert!(config.upload_client_reuse_requests > 0);
        assert!(config.upload_timeout_secs > 0);
    }

    #[test]
    fn maintenance_interval_defaults_exist() {
        let config = Config::default();
        assert!(config.memory_release_interval_requests >= 1);
        assert!(config.db_analyze_interval_requests >= 1);
    }

    #[test]
    fn default_cover_mode_is_thumbnail() {
        let config = Config::default();
        assert_eq!(config.cover_mode, CoverMode::Thumbnail);
    }
}
