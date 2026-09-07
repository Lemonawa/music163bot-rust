//! The Telegram bot runtime: update intake, command handlers, and the
//! download/upload pipeline. `run` is the entry point; each submodule owns
//! one concern (see module docs).

use crate::config::Config;
use crate::error::Result;

mod about;
mod admin;
mod collection_flow;
mod commands;
mod core_flow;
mod download_flow;
mod entry;
mod help_entry;
mod intake;
mod lang_command;
mod maintenance;
mod music_ui;
mod permits;
mod replies;
mod support;
mod tagging;
mod target_resolution;
mod upload_client;
mod wiring;

/// Boot the bot: wiring, command registration, and the long-poll loop.
///
/// # Errors
/// Returns an error if startup (config, database, Telegram API) fails.
pub async fn run(config: Config) -> Result<()> {
    entry::run(config).await
}

#[cfg(test)]
mod tests;
