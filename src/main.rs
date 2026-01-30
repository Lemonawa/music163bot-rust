#![warn(clippy::all, clippy::pedantic)]
// Allow certain pedantic warnings that are acceptable for this codebase
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::struct_excessive_bools,
    clippy::doc_markdown,
    clippy::needless_pass_by_value
)]

// Use jemalloc with tuning for better memory return to OS
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

pub mod audio_buffer;
pub mod bot;
pub mod config;
pub mod database;
pub mod error;
pub mod memory;
pub mod music_api;
pub mod utils;

use anyhow::Result;
use clap::Parser;
use config::Config;
use tracing::info;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Configuration file path
    #[arg(short, long, default_value = "config.ini")]
    config: String,

    /// Disable update checks
    #[arg(long)]
    no_update: bool,

    /// Disable MD5 verification
    #[arg(long)]
    no_md5_check: bool,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Setup logging
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&args.log_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let subscriber = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_target(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    info!("Music163bot-Rust starting...");

    // Load configuration
    let config = Config::load(&args.config)?;
    info!("Configuration loaded from {}", args.config);

    // Start the bot
    bot::run(config).await?;

    Ok(())
}
