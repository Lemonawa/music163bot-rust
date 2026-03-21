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

mod about;
mod collection_flow;
mod commands;
mod core_flow;
mod download_flow;
mod entry;
mod help_entry;
mod support;
mod target_resolution;
mod telegram_api;
mod upload;
mod upload_document;
mod wiring;

use about::*;
use collection_flow::*;
use commands::*;
use core_flow::*;
use download_flow::*;
use entry::*;
use help_entry::*;
use support::*;
use target_resolution::*;
use telegram_api::*;
use upload::*;
use upload_document::*;
use wiring::*;

pub(crate) async fn run(config: Config) -> Result<()> {
    entry::run(config).await
}

#[cfg(test)]
mod tests;
