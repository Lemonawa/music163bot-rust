use super::*;

// Test compatibility: names the suites reference as `super::<name>` now live
// in their concern modules rather than the bot root.
#[cfg(test)]
use super::about::{BUILD_GIT_COMMIT, build_about_text};
#[cfg(test)]
use super::collection_flow::cover_download_failure_notice;
#[cfg(test)]
use super::core_flow::rate_limit_retry_delay_secs;
#[cfg(test)]
use super::download_flow::{
    PostUploadDbAction, classify_post_upload_db_result, max_download_size_bytes,
    resolve_cover_policy, should_download_cover,
};
#[cfg(test)]
use super::entry::{
    StatusTextParams, build_status_text, format_speed_line, format_uptime, percentile_95,
};
#[cfg(test)]
use super::intake::{
    MessageTaskRoute, classify_message_task, is_clearallcache_confirm, is_command_text,
    is_official_telegram_api, should_spawn_message_task,
};
#[cfg(test)]
use super::lang_command::{
    bot_commands_for_locale, build_lang_keyboard, resolve_chat_language_for,
    resolve_inline_language_for,
};
#[cfg(test)]
use super::maintenance::collect_maintenance_signals;
#[cfg(test)]
use super::music_ui::{
    append_search_result_line, build_music_url, build_program_url, cached_music_link_target,
    clearallcache_confirmation_prompt, exceeds_batch_download_limit, rmcache_usage_prompt,
};
#[cfg(test)]
use super::permits::{acquire_download_leader, acquire_download_permit};
#[cfg(test)]
use super::support::{
    CLEARALLCACHE_CONFIRM_WINDOW, build_caption, format_bitrate_kbps, prune_expired_confirmations,
};
#[cfg(test)]
use super::tagging::{apply_tags_in_blocking, format_perf, should_refresh_upload_client};
#[cfg(test)]
use super::target_resolution::{ParsedMusicTarget, parse_direct_music_target};
#[cfg(test)]
use super::upload_client::{
    UploadFileTarget, extract_file_id_from_response, maybe_local_file_uri,
    parse_telegram_api_response, run_upload_prewarm, select_local_upload_target,
};
#[cfg(test)]
use super::wiring::{
    AudioFormat, BotState, CACHE_PRUNE_INTERVAL_REQUESTS, CacheSnapshot, InflightClaim,
    InflightDownloads, InflightEntry, MaintenanceCounters, MaintenanceSignal, MusicLinkTarget,
    ResourceSnapshot, RuntimeMetrics, SpeedSnapshot, UploadClientState, UploadCounters,
    format_perf_stage_line, set_inflight_wait_hook, upload_topology_label,
};
#[cfg(test)]
use std::collections::VecDeque;

use crate::config::CoverMode;
use crate::config::{Config, ConfigFlags};
use crate::telegram::TelegramBot as Bot;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

struct BufferWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

struct BufferGuard {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for BufferGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut buffer = self
            .buffer
            .lock()
            .map_err(|_| std::io::Error::other("lock poisoned"))?;
        buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = BufferGuard;

    fn make_writer(&'a self) -> Self::Writer {
        BufferGuard {
            buffer: Arc::clone(&self.buffer),
        }
    }
}

fn capture_logs<F>(max_level: tracing::Level, action: F) -> String
where
    F: FnOnce(),
{
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_writer(BufferWriter {
            buffer: Arc::clone(&buffer),
        })
        .with_ansi(false)
        .with_max_level(max_level)
        .finish();

    tracing::subscriber::with_default(subscriber, action);

    let buffer = buffer
        .lock()
        .expect("log buffer lock should succeed")
        .clone();
    String::from_utf8_lossy(&buffer).to_string()
}

fn create_temp_file() -> PathBuf {
    let filename = format!("music163bot_local_uri_{}", Uuid::new_v4());
    let path = std::env::temp_dir().join(filename);
    fs::write(&path, b"ok").expect("write temp file");
    path
}

fn critical_path_stage_labels() -> [&'static str; 2] {
    [
        super::wiring::PERF_STAGE_SELECT_URL,
        super::wiring::PERF_STAGE_PRE_UPLOAD_PATH,
    ]
}

mod command_ui;
mod concurrency;
mod lang_ui;
mod scheduling;
mod telegram;
mod upload;
