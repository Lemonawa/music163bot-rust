use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum UploadLogLevel {
    None,
    Error,
    Warning,
    #[default]
    Info,
    Debug,
}

impl std::str::FromStr for UploadLogLevel {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("none") {
            Ok(Self::None)
        } else if trimmed.eq_ignore_ascii_case("error") {
            Ok(Self::Error)
        } else if trimmed.eq_ignore_ascii_case("warn") || trimmed.eq_ignore_ascii_case("warning") {
            Ok(Self::Warning)
        } else if trimmed.eq_ignore_ascii_case("info") {
            Ok(Self::Info)
        } else if trimmed.eq_ignore_ascii_case("debug") {
            Ok(Self::Debug)
        } else {
            Err(anyhow::anyhow!("Invalid upload log level: {s}"))
        }
    }
}

impl std::fmt::Display for UploadLogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::None => "none",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
        };
        write!(f, "{value}")
    }
}

impl UploadLogLevel {
    #[must_use]
    pub fn allows(self, level: UploadLogLevel) -> bool {
        self.rank() >= level.rank()
    }

    fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Error => 1,
            Self::Warning => 2,
            Self::Info => 3,
            Self::Debug => 4,
        }
    }
}

fn parse_bool_like(value: &str) -> Option<bool> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        Some(true)
    } else if trimmed.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
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
    /// Max concurrent uploads
    pub upload_max_concurrent: u32,
    /// Upload diagnostic log level
    pub upload_log_level: UploadLogLevel,
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
            upload_client_reuse_requests: 0,
            upload_max_concurrent: 1,
            upload_log_level: UploadLogLevel::default(),
            upload_pool_max_idle_per_host: 1,
            upload_pool_idle_timeout_secs: 300,
            upload_timeout_secs: 300,
            upload_local_file_uri: false,
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
        let mut config_map = HashMap::with_capacity(32);
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
            if line.starts_with('[') {
                current_section = line
                    .strip_prefix('[')
                    .and_then(|section| section.strip_suffix(']'))
                    .unwrap_or("")
                    .to_string();
                continue;
            }

            // Parse key=value pairs
            if let Some((raw_key, raw_value)) = line.split_once('=') {
                let key = raw_key.trim().to_lowercase();
                let value = raw_value.trim().to_string();

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

        if let Some(v) = config_map.get("botdebug") {
            apply_bool_field(v, &mut config.bot_debug, "botdebug");
        }

        if let Some(db) = config_map.get("database") {
            config.database.clone_from(db);
        }

        if let Some(level) = config_map.get("loglevel") {
            config.log_level.clone_from(level);
        }

        if let Some(v) = config_map.get("autoupdate") {
            apply_bool_field(v, &mut config.auto_update, "autoupdate");
        }

        if let Some(v) = config_map.get("autoretry") {
            apply_bool_field(v, &mut config.auto_retry, "autoretry");
        }

        if let Some(v) = config_map.get("maxretrytimes") {
            config.max_retry_times = parse_field(v, config.max_retry_times, "maxretrytimes");
        }

        if let Some(v) = config_map.get("downloadtimeout") {
            config.download_timeout = parse_field(v, config.download_timeout, "downloadtimeout");
        }

        if let Some(v) = config_map.get("checkmd5") {
            apply_bool_field(v, &mut config.check_md5, "checkmd5");
        }

        // Smart storage settings (v1.1.0+)
        if let Some(mode) = config_map.get("download.storage_mode") {
            match mode.parse::<StorageMode>() {
                Ok(m) => config.storage_mode = m,
                Err(e) => tracing::warn!("Invalid storage_mode '{}': {}, using default", mode, e),
            }
        }
        if let Some(v) = config_map.get("download.memory_threshold") {
            config.memory_threshold_mb =
                parse_field(v, config.memory_threshold_mb, "download.memory_threshold");
        }
        if let Some(v) = config_map.get("download.memory_buffer") {
            config.memory_buffer_mb =
                parse_field(v, config.memory_buffer_mb, "download.memory_buffer");
        }
        if let Some(v) = config_map.get("download.memory_max_file_mb") {
            config.memory_max_file_mb =
                parse_field(v, config.memory_max_file_mb, "download.memory_max_file_mb");
        }
        if let Some(v) = config_map.get("download.max_concurrent") {
            config.max_concurrent_downloads = parse_field(
                v,
                config.max_concurrent_downloads,
                "download.max_concurrent",
            );
        }
        if let Some(v) = config_map.get("download.pool_max_idle_per_host") {
            config.download_pool_max_idle_per_host = parse_field(
                v,
                config.download_pool_max_idle_per_host,
                "download.pool_max_idle_per_host",
            );
        }
        if let Some(v) = config_map.get("download.connect_timeout_secs") {
            config.download_connect_timeout_secs = parse_field(
                v,
                config.download_connect_timeout_secs,
                "download.connect_timeout_secs",
            );
        }
        if let Some(v) = config_map.get("download.chunk_size_kb") {
            config.download_chunk_size_kb =
                parse_field(v, config.download_chunk_size_kb, "download.chunk_size_kb");
        }
        if let Some(mode) = config_map.get("download.cover_mode") {
            match mode.parse::<CoverMode>() {
                Ok(m) => config.cover_mode = m,
                Err(e) => tracing::warn!("Invalid cover_mode '{}': {}, using default", mode, e),
            }
        }

        if let Some(v) = config_map.get("upload.client_reuse_requests") {
            config.upload_client_reuse_requests = parse_field(
                v,
                config.upload_client_reuse_requests,
                "upload.client_reuse_requests",
            );
        }
        if let Some(v) = config_map.get("upload.max_concurrent") {
            config.upload_max_concurrent =
                parse_field(v, config.upload_max_concurrent, "upload.max_concurrent");
        }
        if let Some(level) = config_map.get("upload.log_level") {
            match level.parse::<UploadLogLevel>() {
                Ok(parsed) => config.upload_log_level = parsed,
                Err(e) => {
                    tracing::warn!("Invalid upload.log_level '{}': {}, using default", level, e);
                }
            }
        }
        if let Some(v) = config_map.get("upload.pool_max_idle_per_host") {
            config.upload_pool_max_idle_per_host = parse_field(
                v,
                config.upload_pool_max_idle_per_host,
                "upload.pool_max_idle_per_host",
            );
        }
        if let Some(v) = config_map.get("upload.pool_idle_timeout_secs") {
            config.upload_pool_idle_timeout_secs = parse_field(
                v,
                config.upload_pool_idle_timeout_secs,
                "upload.pool_idle_timeout_secs",
            );
        }
        if let Some(v) = config_map.get("upload.timeout_secs") {
            config.upload_timeout_secs =
                parse_field(v, config.upload_timeout_secs, "upload.timeout_secs");
        }
        if let Some(v) = config_map.get("upload.local_file_uri") {
            apply_bool_field(
                v,
                &mut config.upload_local_file_uri,
                "upload.local_file_uri",
            );
        }

        if let Some(v) = config_map.get("maintenance.memory_release_interval_requests") {
            config.memory_release_interval_requests = parse_field(
                v,
                config.memory_release_interval_requests,
                "maintenance.memory_release_interval_requests",
            );
        }
        if let Some(v) = config_map.get("maintenance.db_analyze_interval_requests") {
            config.db_analyze_interval_requests = parse_field(
                v,
                config.db_analyze_interval_requests,
                "maintenance.db_analyze_interval_requests",
            );
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Config, CoverMode, UploadLogLevel};

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
    fn upload_defaults_use_reuse_settings() {
        let config = Config::default();
        assert_eq!(config.upload_client_reuse_requests, 0);
        assert_eq!(config.upload_max_concurrent, 1);
        assert_eq!(config.upload_pool_max_idle_per_host, 1);
        assert_eq!(config.upload_pool_idle_timeout_secs, 300);
        assert_eq!(config.upload_timeout_secs, 300);
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

    #[test]
    fn invalid_numeric_config_keeps_defaults() {
        let default_config = Config::default();
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path = std::env::temp_dir().join(format!("music163bot_config_{temp_name}.ini"));
        let content = "bot.token=token\n\
download.memory_max_file_mb=not-a-number\n\
maintenance.memory_release_interval_requests=bad\n\
maintenance.db_analyze_interval_requests=bad\n";

        std::fs::write(&temp_path, content).expect("write temp config");

        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

        std::fs::remove_file(&temp_path).expect("remove temp config");

        assert_eq!(loaded.memory_max_file_mb, default_config.memory_max_file_mb);
        assert_eq!(
            loaded.memory_release_interval_requests,
            default_config.memory_release_interval_requests
        );
        assert_eq!(
            loaded.db_analyze_interval_requests,
            default_config.db_analyze_interval_requests
        );
    }

    #[test]
    fn upload_pool_config_parses() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_upload_pool_{temp_name}.ini"));
        let content = "bot.token=token\n\
upload.pool_max_idle_per_host=2\n\
upload.pool_idle_timeout_secs=120\n";

        std::fs::write(&temp_path, content).expect("write temp config");

        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

        std::fs::remove_file(&temp_path).expect("remove temp config");

        assert_eq!(loaded.upload_pool_max_idle_per_host, 2);
        assert_eq!(loaded.upload_pool_idle_timeout_secs, 120);
    }

    #[test]
    fn upload_log_level_defaults_to_info() {
        let config = Config::default();
        assert_eq!(config.upload_log_level, UploadLogLevel::Info);
    }

    #[test]
    fn upload_log_level_parses_values() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_upload_log_{temp_name}.ini"));
        let content = "bot.token=token\n\
upload.log_level=warn\n";

        std::fs::write(&temp_path, content).expect("write temp config");

        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(loaded.upload_log_level, UploadLogLevel::Warning);
    }

    #[test]
    fn upload_max_concurrent_parses() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_upload_limit_{temp_name}.ini"));
        let content = "bot.token=token\n\
upload.max_concurrent=6\n";

        std::fs::write(&temp_path, content).expect("write temp config");

        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(loaded.upload_max_concurrent, 6);
    }

    #[test]
    fn upload_client_reuse_requests_allows_zero() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_upload_reuse_{temp_name}.ini"));
        let content = "bot.token=token\n\
upload.client_reuse_requests=0\n";

        std::fs::write(&temp_path, content).expect("write temp config");

        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(loaded.upload_client_reuse_requests, 0);
    }

    #[test]
    fn upload_pool_max_idle_falls_back_to_default() {
        let default_config = Config::default();
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_upload_pool_bad_{temp_name}.ini"));
        let content = "bot.token=token\n\
upload.pool_max_idle_per_host=not-a-number\n";

        std::fs::write(&temp_path, content).expect("write temp config");

        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(
            loaded.upload_pool_max_idle_per_host,
            default_config.upload_pool_max_idle_per_host
        );
    }

    #[test]
    fn upload_local_file_uri_defaults_false() {
        let config = Config::default();
        assert!(!config.upload_local_file_uri);
    }

    #[test]
    fn upload_local_file_uri_parses_true() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path = std::env::temp_dir().join(format!("music163bot_local_uri_{temp_name}.ini"));
        let content = "bot.token=token\nupload.local_file_uri=true\n";
        std::fs::write(&temp_path, content).expect("write temp config");
        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");
        let _ = std::fs::remove_file(&temp_path);
        assert!(loaded.upload_local_file_uri);
    }

    #[test]
    fn config_bool_parsing_handles_common_values() {
        assert_eq!(super::parse_bool_like("true"), Some(true));
        assert_eq!(super::parse_bool_like("TRUE"), Some(true));
        assert_eq!(super::parse_bool_like(" false "), Some(false));
        assert_eq!(super::parse_bool_like("invalid"), None);
    }

    #[test]
    fn cover_mode_parses_mixed_case() {
        use super::CoverMode;
        assert_eq!(
            "ThUmBnAiL".parse::<CoverMode>().unwrap(),
            CoverMode::Thumbnail
        );
        assert_eq!(
            "ORIGINAL".parse::<CoverMode>().unwrap(),
            CoverMode::Original
        );
        assert_eq!("BoTh".parse::<CoverMode>().unwrap(), CoverMode::Both);
        assert!("invalid".parse::<CoverMode>().is_err());
    }

    #[test]
    fn storage_mode_parses_mixed_case() {
        use super::StorageMode;
        assert_eq!(
            "HyBrId".parse::<StorageMode>().unwrap(),
            StorageMode::Hybrid
        );
        assert_eq!("DISK".parse::<StorageMode>().unwrap(), StorageMode::Disk);
        assert_eq!(
            "Memory".parse::<StorageMode>().unwrap(),
            StorageMode::Memory
        );
        assert!("bogus".parse::<StorageMode>().is_err());
    }

    #[test]
    fn upload_log_level_parses_mixed_case() {
        assert_eq!(
            "WaRn".parse::<UploadLogLevel>().unwrap(),
            UploadLogLevel::Warning
        );
        assert_eq!(
            "WARNING".parse::<UploadLogLevel>().unwrap(),
            UploadLogLevel::Warning
        );
        assert_eq!(
            "  Debug  ".parse::<UploadLogLevel>().unwrap(),
            UploadLogLevel::Debug
        );
        assert_eq!(
            "NONE".parse::<UploadLogLevel>().unwrap(),
            UploadLogLevel::None
        );
        assert!("garbage".parse::<UploadLogLevel>().is_err());
    }

    #[test]
    fn ini_section_with_non_ascii_name_does_not_panic() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_non_ascii_section_{temp_name}.ini"));
        let content = "[音乐]\n\
unused=1\n\
[bot]\n\
token=token\n";

        std::fs::write(&temp_path, content).expect("write temp config");

        let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(loaded.bot_token, "token");
    }

    #[test]
    fn parse_field_returns_parsed_value() {
        let result: u32 = super::parse_field("42", 0, "test_key");
        assert_eq!(result, 42);
    }

    #[test]
    fn parse_field_returns_default_on_invalid() {
        let result: u32 = super::parse_field("not_a_number", 99, "test_key");
        assert_eq!(result, 99);
    }

    #[test]
    fn parse_bool_field_updates_target() {
        let mut target = false;
        super::apply_bool_field("true", &mut target, "test_key");
        assert!(target);
    }

    #[test]
    fn parse_bool_field_keeps_default_on_invalid() {
        let mut target = true;
        super::apply_bool_field("banana", &mut target, "test_key");
        assert!(target); // unchanged
    }
}
