//! Library crate for music163bot-rust: NetEase Cloud Music API client,
//! Telegram bot runtime, config, database, and audio buffering.
//!
//! The binary (`src/main.rs`) is a thin adapter: CLI parsing, logging setup,
//! then `bot::run`. Maintenance tools (`src/bin/*`) link against this library
//! instead of duplicating domain logic.

// Compile-time embedded translations from locales/*.yml. Must live at the
// crate root: rust-i18n's `t!` macro references `crate::_rust_i18n_t`.
rust_i18n::i18n!("locales", fallback = "zh");

pub mod audio_buffer;
pub mod bot;
pub mod config;
pub mod database;
pub mod error;
pub mod i18n;
pub mod memory;
pub mod music_api;
pub mod telegram;
pub mod utils;
