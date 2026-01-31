# AGENTS.md - Agent Instructions for music163bot-rust

A Rust-based Telegram bot for NetEase Cloud Music (网易云音乐). Downloads, shares, and searches songs with smart caching.

## Build Commands

```bash
# Development build
cargo build

# Release build (optimized for production - 5.6MB binary)
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
# Run clippy (linter - configured in main.rs with strict settings)
cargo clippy

# Run clippy with warnings as errors (CI style)
cargo clippy -- -D warnings

# Format code
cargo fmt

# Check formatting without modifying
cargo fmt -- --check
```

## Test Commands

**Note: This project currently has NO tests configured.** Use compilation as verification.

```bash
# Run tests (none exist currently)
cargo test

# Run a specific test (when tests are added)
cargo test test_name

# Run tests in a specific module
cargo test module_name::
```

## Git Workflow

This project follows a frequent-commit workflow to enable easy rollback and tracking:

### Commit Strategy
- **Every change gets its own commit** - After any file modification, run `cargo check` and `cargo clippy`, then commit immediately
- This creates a detailed history where each commit represents a single logical change
- Makes it easy to revert specific changes without affecting others

### Commit Message Format
Use conventional commits format: `<type>: <description>`

Types:
- `fix:` Bug fixes
- `feat:` New features
- `docs:` Documentation changes
- `chore:` Maintenance tasks, tool updates
- `perf:` Performance improvements

Examples:
- `fix: add thumbnail dimensions to inline search results`
- `docs: add max_concurrent config option`
- `chore: bump version to 1.1.11`

### Push Policy
- **Avoid pushing to remote** unless explicitly requested
- Keep all work local until user decides to publish
- This prevents accidental remote updates and gives user full control over when to push

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
- Log errors with `tracing::error!()` before returning

Example:
```rust
use crate::error::{BotError, Result};

pub async fn fetch_data() -> Result<Data> {
    let response = client.get(url).await?;
    let data = response.json::<Data>().await?;
    Ok(data)
}
```

### Async Patterns
- Use `async fn` for I/O operations
- Use `tokio::spawn()` for concurrent tasks
- Use `tokio::join!()` for parallel awaits
- Use `tokio::time::timeout()` for timeouts

Example:
```rust
let (result1, result2) = tokio::join!(task1, task2);
let handle = tokio::spawn(async move { process_data().await });
```

### Clippy Configuration
- Configured in `main.rs` with `#![warn(clippy::all, clippy::pedantic)]`
- Extensive allow list for acceptable patterns (main.rs lines 3-15)
- **DO NOT** use `#[allow(...)]` unless matching existing patterns

### Logging
- Use `tracing::info!()`, `tracing::warn!()`, `tracing::error!()`
- Use `tracing::debug!()` for verbose debugging
- Include context (e.g., music_id, file sizes)

### Comments
- Use `///` for public API documentation
- Use `//` for inline comments
- Comments in Chinese or English both acceptable

### Structs and Serialization
- Use `#[derive(Debug, Clone, Serialize, Deserialize)]`
- Use `#[serde(rename = "...")]` for API field mapping
- Use `#[serde(alias = "...")]` for backward compatibility

## Project Structure

```
src/
├── main.rs           # Entry point, clippy config, jemalloc
├── bot.rs            # Telegram bot handlers (largest file)
├── music_api.rs      # NetEase API client
├── audio_buffer.rs   # Audio download/storage (smart storage)
├── database.rs       # SQLite operations (WAL mode enabled)
├── config.rs         # INI configuration parsing
├── error.rs          # Error types (thiserror)
├── memory.rs         # Memory management (jemalloc)
└── utils.rs          # Helper functions
```

## Dependencies

- **tokio** - Async runtime
- **teloxide** - Telegram bot framework
- **reqwest** - HTTP client (connection pool tuned)
- **sqlx** - Async SQLite (WAL mode)
- **serde** - Serialization
- **tracing** - Structured logging
- **anyhow/thiserror** - Error handling
- **id3/metaflac** - Audio metadata
- **tikv-jemallocator** - Memory allocator

## CI/CD

GitHub Actions builds for Linux (x86_64, aarch64), macOS (x86_64, aarch64), Windows (x86_64).
Release builds are created automatically on tag push.

## License

WTFPL - Do What The F*ck You Want To Public License
