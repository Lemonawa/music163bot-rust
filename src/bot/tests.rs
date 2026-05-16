use super::*;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::Config;
use crate::config::CoverMode;
use crate::telegram::TelegramBot as Bot;
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
        super::PERF_STAGE_SELECT_URL,
        super::PERF_STAGE_PRE_UPLOAD_PATH,
    ]
}

mod command_ui;
mod concurrency;
mod scheduling;
mod telegram;
mod upload;
