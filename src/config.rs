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
    /// Maximum concurrent downloads (lower = less memory, higher = more throughput)
    pub max_concurrent_downloads: u32,
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
            max_concurrent_downloads: 3, // 从 10 减少到 3，减少内存峰值
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
        if let Some(concurrent) = config_map.get("download.max_concurrent") {
            config.max_concurrent_downloads = concurrent.parse().unwrap_or(3);
        }

        // Validate required fields
        if config.bot_token.is_empty() {
            return Err(anyhow::anyhow!("BOT_TOKEN is required"));
        }

        Ok(config)
    }
}
