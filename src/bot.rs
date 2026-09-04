use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::Context;
use bytes::Bytes;
use dashmap::DashMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, get_current_pid};
use tokio::sync::{Mutex, Notify};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::audio_buffer::{AudioBuffer, ThumbnailBuffer};
use crate::config::{Config, CoverMode};
use crate::database::{Database, SongInfo};
use crate::error::{BotError, Result};
use crate::music_api::{MusicApi, ProgramMainTrack, format_artists};
use crate::telegram::{
    CallbackQuery, ChatId, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InlineQuery,
    InlineQueryResult, InlineQueryResultArticle, InputFile, InputMessageContent,
    InputMessageContentText, MaybeInaccessibleMessage, Message, MessageId, ParseMode,
    ReplyParameters, ResponseResult, TelegramBot, Update,
};
use crate::utils::{
    MusicCollectionTarget, build_http_client, bytes_to_mb_f64, clean_filename, ensure_dir,
    extract_first_trusted_music_share_url, extract_retry_after_seconds, format_error_chain,
    i64_to_u32_saturating, is_known_non_song_share_url, parse_music_collection_target,
    parse_music_id, parse_music_program_id, sanitize_sensitive_text, throughput_mbps, u64_to_f64,
    u64_to_i64_saturating, update_peak,
};

/// Type alias so submodules can keep using `Bot` without renaming everywhere.
type Bot = TelegramBot;

mod about;
mod collection_flow;
mod commands;
mod core_flow;
mod download_flow;
mod entry;
mod help_entry;
mod lang_command;
mod support;
mod target_resolution;
mod telegram_api;
mod upload;
mod upload_document;
mod wiring;

use about::handle_about_command;
use collection_flow::{
    cover_download_failure_notice, download_cover_assets, process_music_collection,
};
use commands::{handle_music_url, handle_search_command};
use core_flow::{process_music, process_music_with_context, process_program};
use download_flow::{DownloadAndSendParams, download_and_send_music};
use entry::{
    StatusTextParams, build_status_text, format_speed_line, format_uptime,
    parse_inline_query_keyword, percentile_95, resolve_cover_policy,
    sample_current_process_memory_mb, sample_resource_snapshot, should_download_cover,
};
use help_entry::{handle_help_command, handle_music_command};
use lang_command::{
    handle_lang_callback, handle_lang_command, register_bot_commands, resolve_inline,
    resolve_message,
};

#[cfg(test)]
use lang_command::bot_commands_for_locale;

#[cfg(test)]
use lang_command::build_lang_keyboard;
use support::{
    build_caption, handle_callback, handle_clearallcache_command,
    handle_clearallcache_confirm_command, handle_inline_query, handle_lyric_command,
    handle_rmcache_command, handle_status_command,
};
use target_resolution::{
    dispatch_parsed_music_target, parse_direct_music_target, parse_song_id_or_search_first_result,
};
use telegram_api::{
    RawSendFileArgs, acquire_download_permit, acquire_upload_client, acquire_upload_permit,
    acquire_upload_permit_owned, extract_file_id_from_response, raw_send_file, run_upload_prewarm,
    send_raw_upload_form,
};
use upload::{
    MessageTaskRoute, PERF_STAGE_PRE_UPLOAD_PATH, PERF_STAGE_SELECT_URL, RAW_UPLOAD_CHUNK_SIZE,
    RawDocumentParams, RawUploadParams, UploadBotBundle, UploadFileTarget, acquire_download_leader,
    append_search_result_line, apply_tags_in_blocking, cached_music_link_target,
    classify_message_task, cleanup_audio_buffer, cleanup_thumbnail_buffer,
    clearallcache_confirmation_prompt, collect_maintenance_signals,
    create_music_keyboard_for_target, delete_status_message_resilient,
    edit_status_message_resilient, ensure_admin, exceeds_batch_download_limit, get_upload_bot,
    is_clearallcache_confirm, is_official_telegram_api, join_futures, log_perf, maintenance_worker,
    parse_api_url, require_command_args_or_reply, rmcache_usage_prompt, select_local_upload_target,
    send_reply_html, send_reply_message, send_reply_text, should_log_command,
    should_refresh_upload_client, should_remove_song_cache_after_partial_failure,
    should_set_upload_pool_idle_timeout, should_spawn_message_task, url_bitrate_candidates,
};
use upload_document::raw_send_document_bytes;
use wiring::{
    AudioFormat, BotState, CACHE_PRUNE_INTERVAL_REQUESTS, CacheSnapshot, InflightClaim,
    InflightDownloads, InflightLeaderGuard, MAINTENANCE_QUEUE_CAPACITY, MaintenanceCounters,
    MaintenanceSignal, MusicLinkTarget, PERF_STAGE_CACHE_LOOKUP, PERF_STAGE_COVER_DOWNLOAD,
    PERF_STAGE_DB_SAVE, PERF_STAGE_DOWNLOAD_AUDIO, PERF_STAGE_E2E_TOTAL,
    PERF_STAGE_SINGLEFLIGHT_WAIT, PERF_STAGE_TAG_PROCESS, PERF_STAGE_UPLOAD_CLIENT_ACQUIRE,
    PERF_STAGE_UPLOAD_PERMIT_WAIT, PERF_STAGE_UPLOAD_SEND, PerfTraceContext, ResourceSnapshot,
    RuntimeMetrics, STATUS_RESOURCE_CACHE, STATUS_RESOURCE_REFRESH_INTERVAL, SpeedSnapshot,
    UploadClientState, UploadCounters, build_perf_trace_context, lock_unpoisoned,
};

#[cfg(test)]
use about::{BUILD_GIT_COMMIT, build_about_text};
#[cfg(test)]
use collection_flow::collection_retry_delay_seconds;
#[cfg(test)]
use download_flow::{PostUploadDbAction, classify_post_upload_db_result, max_download_size_bytes};
#[cfg(test)]
use entry::{CoverPolicy, parse_command_and_args, parse_start_music_id};
#[cfg(test)]
use support::{CLEARALLCACHE_CONFIRM_WINDOW, format_bitrate_kbps, prune_expired_confirmations};
#[cfg(test)]
use target_resolution::ParsedMusicTarget;
#[cfg(test)]
use telegram_api::{parse_telegram_api_response, redact_bot_token_in_error_message};
#[cfg(test)]
use upload::{
    build_music_url, build_program_url, format_perf, is_command_text, is_spawnable_command_text,
    maybe_local_file_uri,
};
#[cfg(test)]
use wiring::{
    InflightEntry, format_perf_stage_line, set_inflight_wait_hook, upload_topology_label,
};

pub(crate) async fn run(config: Config) -> Result<()> {
    entry::run(config).await
}

#[cfg(test)]
mod tests;
