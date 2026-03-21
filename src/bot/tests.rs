use super::*;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::config::CoverMode;
use crate::config::UploadLogLevel;
use teloxide::Bot;
use uuid::Uuid;

fn create_temp_file() -> PathBuf {
    let filename = format!("music163bot_local_uri_{}", Uuid::new_v4());
    let path = std::env::temp_dir().join(filename);
    fs::write(&path, b"ok").expect("write temp file");
    path
}

fn critical_path_stage_labels() -> [&'static str; 2] {
    [
        super::PERF_STAGE_SELECT_URL,
        super::PERF_STAGE_PRE_UPLOAD_PATH,
    ]
}

mod command_ui;
mod concurrency;
mod scheduling;
mod telegram;
mod upload;
