# AGENTS.md - Agent Instructions for music163bot-rust

## Project Overview

A Rust-based Telegram bot for NetEase Cloud Music (网易云音乐). Downloads, shares, and searches songs with smart caching.

## Build Commands

```bash
# Development build
cargo build

# Release build (optimized for production)
cargo build --release

# Run with custom config
cargo run --release -- --config config.ini

# Quick check (faster than build, checks types only)
cargo check

# Build specific target
cargo build --release --target x86_64-unknown-linux-gnu
```

## Lint Commands

```bash
# Run clippy (linter - configured in main.rs)
cargo clippy

# Run clippy with warnings as errors (CI style)
cargo clippy -- -D warnings

# Format code (uses default rustfmt settings)
cargo fmt

# Check formatting without modifying
cargo fmt -- --check
```

## Test Commands

**Note: This project currently has NO tests configured.**

```bash
# Run tests (none exist currently)
cargo test

# Run a specific test (when tests are added)
cargo test test_name

# Run tests in a specific module
cargo test module_name::
```

## Code Style Guidelines

### Imports Ordering
1. Standard library (`std::`)
2. External crates (e.g., `tokio::`, `serde::`)
3. Internal modules (`crate::`)

Example:
```rust
use std::collections::HashMap;
use std::io::Cursor;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::{BotError, Result};
use crate::config::Config;
```

### Naming Conventions
- **Functions/variables**: `snake_case` (e.g., `download_file`, `music_id`)
- **Types/structs/enums**: `PascalCase` (e.g., `SongDetail`, `StorageMode`)
- **Constants/statics**: `UPPER_SNAKE_CASE` (e.g., `SONG_REGEX`)
- **Modules**: `snake_case` (e.g., `music_api`, `audio_buffer`)

### Error Handling
- Use `thiserror` for custom error types (see `src/error.rs`)
- Use `anyhow` for general error propagation
- Prefer `?` operator over explicit match/unwrap
- Log errors with `tracing::error!()` before returning when appropriate

Example:
```rust
use crate::error::{BotError, Result};

pub async fn fetch_data() -> Result<Data> {
    let response = client.get(url).await?; // anyhow::Error auto-converts
    let data = response.json::<Data>().await?;
    Ok(data)
}
```

### Async/Await Patterns
- Always use `async fn` for I/O operations
- Use `tokio::spawn()` for concurrent tasks
- Use `tokio::join!()` for parallel awaits
- Use `tokio::time::timeout()` for timeouts

Example:
```rust
let (result1, result2) = tokio::join!(task1, task2);

let handle = tokio::spawn(async move {
    process_data().await
});
```

### Clippy Configuration
Clippy is configured in `main.rs` with these settings:
- `#![warn(clippy::all, clippy::pedantic)]` - Enable all lints
- Extensive allow list for acceptable patterns (see main.rs lines 3-15)

**DO NOT suppress warnings with `#[allow(...)]` unless matching existing patterns.**

### Logging
- Use `tracing::info!()`, `tracing::warn!()`, `tracing::error!()` for structured logging
- Use `tracing::debug!()` for verbose debugging
- Include context in log messages (e.g., music_id, file sizes)

### Comments
- Use `///` for public API documentation
- Use `//` for inline comments
- Comments can be in Chinese or English (project uses both)

### Structs and Serialization
- Use `#[derive(Debug, Clone, Serialize, Deserialize)]` for data structures
- Use `#[serde(rename = "...")]` for API field mapping
- Use `#[serde(alias = "...")]` for backward compatibility

Example:
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SongDetail {
    pub id: u64,
    pub name: String,
    #[serde(alias = "duration")]
    pub dt: Option<u64>,
}
```

## Project Structure

```
src/
├── main.rs           # Entry point, clippy config
├── bot.rs            # Telegram bot handlers (largest file)
├── music_api.rs      # NetEase API client
├── audio_buffer.rs   # Audio download/storage abstraction
├── database.rs       # SQLite operations
├── database2.rs      # Additional DB utilities
├── config.rs         # Configuration parsing (INI format)
├── error.rs          # Error types
├── memory.rs         # Memory management utilities
└── utils.rs          # Helper functions
```

## Dependencies to Know

- **tokio** - Async runtime (always use `tokio::main`)
- **teloxide** - Telegram bot framework
- **reqwest** - HTTP client
- **sqlx** - Async SQLite
- **serde** - Serialization
- **tracing** - Logging
- **anyhow/thiserror** - Error handling
- **id3/metaflac** - Audio metadata

## CI/CD

GitHub Actions builds for:
- Linux (x86_64, aarch64)
- macOS (x86_64, aarch64)
- Windows (x86_64)

## License

WTFPL - Do What The F*ck You Want To Public License
