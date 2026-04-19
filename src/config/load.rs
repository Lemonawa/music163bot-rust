use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

use anyhow::Result;

use super::{Config, CoverMode, StorageMode, apply_bool_field, parse_admin_list, parse_field};

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
            config.bot_admin = parse_admin_list(admins);
            tracing::info!("Loaded bot admins: {:?}", config.bot_admin);
        } else if let Some(admins) = config_map.get("bot.admin") {
            // Support alternative config key "bot.admin"
            config.bot_admin = parse_admin_list(admins);
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
        if let Some(v) = config_map.get("download.max_batch_tracks") {
            config.max_batch_download_tracks = parse_field(
                v,
                config.max_batch_download_tracks,
                "download.max_batch_tracks",
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
