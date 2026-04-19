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
    clippy::needless_pass_by_value,
    clippy::too_many_arguments
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

const DEFAULT_LOG_LEVEL_SPEC: &str = "info";

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Configuration file path
    #[arg(short, long, default_value = "config.ini")]
    config: String,

    /// Override global log level (trace/debug/info/warn/error)
    #[arg(long)]
    log_level: Option<String>,
}

#[must_use]
fn resolve_log_level_spec(
    cli_log_level: Option<&str>,
    env_log_level: Option<&str>,
    config_log_level: &str,
) -> String {
    for (source, candidate) in [
        ("--log-level", cli_log_level),
        ("RUST_LOG", env_log_level),
        ("config.loglevel", Some(config_log_level)),
        ("default", Some(DEFAULT_LOG_LEVEL_SPEC)),
    ] {
        let Some(raw) = candidate else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if EnvFilter::try_new(trimmed).is_ok() {
            return trimmed.to_owned();
        }
        if source != "default" {
            eprintln!("Invalid {source} value '{trimmed}', trying next source");
        }
    }
    DEFAULT_LOG_LEVEL_SPEC.to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    let env_log_level = std::env::var("RUST_LOG").ok();
    let log_level_spec = resolve_log_level_spec(
        args.log_level.as_deref(),
        env_log_level.as_deref(),
        &config.log_level,
    );

    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::new(log_level_spec))
        .with_target(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    info!("Music163bot-Rust starting...");
    info!("Configuration loaded from {}", args.config);

    // Start the bot
    bot::run(config).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_log_level_spec;

    #[test]
    fn resolve_log_level_prefers_cli() {
        let resolved = resolve_log_level_spec(Some("warn"), Some("info"), "debug");
        assert_eq!(resolved, "warn");
    }

    #[test]
    fn resolve_log_level_falls_back_to_env() {
        let resolved = resolve_log_level_spec(None, Some("error"), "");
        assert_eq!(resolved, "error");
    }

    #[test]
    fn resolve_log_level_prefers_env_over_config() {
        let resolved = resolve_log_level_spec(None, Some("error"), "warn");
        assert_eq!(resolved, "error");
    }

    #[test]
    fn resolve_log_level_falls_back_to_config() {
        let resolved = resolve_log_level_spec(None, None, "warn");
        assert_eq!(resolved, "warn");
    }

    #[test]
    fn resolve_log_level_skips_empty_values() {
        let resolved = resolve_log_level_spec(Some("   "), Some(""), "warn");
        assert_eq!(resolved, "warn");
    }
}
