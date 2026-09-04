use std::collections::HashMap;

use anyhow::Result;

use super::{Config, CoverMode, StorageMode, apply_bool_field, parse_admin_list, parse_field};
use crate::config::parse_ini_text;

impl Config {
    /// # Errors
    /// Returns an error if the config file cannot be read or contains invalid values.
    pub fn load(config_path: &str) -> Result<Self> {
        let mut config = Config::default();

        if !std::path::Path::new(config_path).exists() {
            return Err(anyhow::anyhow!("Config file not found: {config_path}"));
        }

        let config_map = parse_ini_file(config_path)?;

        Self::load_core_fields(&mut config, &config_map);
        Self::load_download_fields(&mut config, &config_map);
        Self::load_upload_fields(&mut config, &config_map);
        Self::load_maintenance_fields(&mut config, &config_map);

        if config.bot_token.is_empty() {
            return Err(anyhow::anyhow!("BOT_TOKEN is required"));
        }

        Ok(config)
    }

    fn load_core_fields(config: &mut Config, config_map: &HashMap<String, String>) {
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
            config.bot_admin = parse_admin_list(admins);
            tracing::info!("Loaded bot admins (from bot.admin): {:?}", config.bot_admin);
        }
        if let Some(db) = config_map.get("database") {
            config.database.clone_from(db);
        }
        if let Some(lang) = config_map.get("bot.default_language") {
            let normalized = lang.trim().to_ascii_lowercase();
            if crate::i18n::is_supported_locale(&normalized) {
                config.default_language = normalized;
            } else {
                tracing::warn!(
                    "Invalid bot.default_language '{lang}' (not in locales/), using default '{}'",
                    config.default_language
                );
            }
        }
        let database_explicit =
            config_map.contains_key("database.url") || config_map.contains_key("database");
        warn_on_legacy_database_path(&config.database, database_explicit);
        if let Some(level) = config_map.get("loglevel") {
            config.log_level.clone_from(level);
        }
        if let Some(v) = config_map.get("autoupdate") {
            apply_bool_field(v, &mut config.flags.behavior.auto_update, "autoupdate");
        }
        if let Some(v) = config_map.get("autoretry") {
            apply_bool_field(v, &mut config.flags.behavior.auto_retry, "autoretry");
        }
        if let Some(v) = config_map.get("maxretrytimes") {
            config.max_retry_times = parse_field(v, config.max_retry_times, "maxretrytimes");
        }
        if let Some(v) = config_map.get("downloadtimeout") {
            config.download_timeout = parse_field(v, config.download_timeout, "downloadtimeout");
        }
        if let Some(v) = config_map.get("checkmd5") {
            apply_bool_field(v, &mut config.flags.behavior.check_md5, "checkmd5");
        }
    }

    fn load_download_fields(config: &mut Config, config_map: &HashMap<String, String>) {
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
        if let Some(v) = config_map.get("download.max_disk_download_mb") {
            config.max_disk_download_mb = parse_field(
                v,
                config.max_disk_download_mb,
                "download.max_disk_download_mb",
            );
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
    }

    fn load_upload_fields(config: &mut Config, config_map: &HashMap<String, String>) {
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
                &mut config.flags.upload.upload_local_file_uri,
                "upload.local_file_uri",
            );
        }
    }

    fn load_maintenance_fields(config: &mut Config, config_map: &HashMap<String, String>) {
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
    }
}

const LEGACY_DATABASE_PATH: &str = "cache.db";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LegacyDbWarning {
    None,
    LegacyOnly,
    LegacyAndExplicit,
}

pub(super) fn classify_legacy_database_state(
    configured_path_exists: bool,
    legacy_path_exists: bool,
    configured_explicitly: bool,
) -> LegacyDbWarning {
    if configured_path_exists || !legacy_path_exists {
        return LegacyDbWarning::None;
    }
    if configured_explicitly {
        LegacyDbWarning::LegacyAndExplicit
    } else {
        LegacyDbWarning::LegacyOnly
    }
}

fn warn_on_legacy_database_path(configured: &str, configured_explicitly: bool) {
    let configured_path_exists = std::path::Path::new(configured).exists();
    let legacy_path_exists = std::path::Path::new(LEGACY_DATABASE_PATH).exists();

    match classify_legacy_database_state(
        configured_path_exists,
        legacy_path_exists,
        configured_explicitly,
    ) {
        LegacyDbWarning::None => {}
        LegacyDbWarning::LegacyAndExplicit => {
            tracing::warn!(
                "database.url '{configured}' does not exist yet, but a legacy \
                 '{LEGACY_DATABASE_PATH}' was found in the working directory. Move it to \
                 '{configured}' to keep your existing data."
            );
        }
        LegacyDbWarning::LegacyOnly => {
            tracing::warn!(
                "database default changed to '{configured}'; a legacy '{LEGACY_DATABASE_PATH}' \
                 was found in the working directory. Either move it to '{configured}' or set \
                 'database.url = {LEGACY_DATABASE_PATH}' in config.ini to keep using it."
            );
        }
    }
}

fn parse_ini_file(path: &str) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_ini_text(&content))
}
