# AGENTS.md - Agent Instructions for music163bot-rust

Rust Telegram bot for NetEase Cloud Music link parsing, search, download, upload, and cache management.

## Rule Files (Cursor/Copilot)
- `.cursor/rules/`: not present
- `.cursorrules`: not present
- `.github/copilot-instructions.md`: not present
- This `AGENTS.md` is the primary instruction source for agents in this repo.

## Build Commands
```bash
# Fast compile check
cargo check
# Debug / release build
cargo build
cargo build --release
# Local cross-build (preferred for Linux amd64 artifact on macOS)
cargo zigbuild --release --target x86_64-unknown-linux-gnu
# Run bot with explicit config
cargo run --release -- --config config.ini
# CI cross-target builds
cargo build --release --target x86_64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-pc-windows-msvc
```

## Lint and Format Commands
```bash
# Auto-format
cargo fmt
# Verify formatting
cargo fmt -- --check
# Lint
cargo clippy
# CI lint gate
cargo clippy -- -D warnings
```

## Test Commands
```bash
# Run all tests
cargo test
# List all tests
cargo test -- --list
# Run one exact test (recommended single-test form)
cargo test config::tests::upload_max_concurrent_parses -- --exact --nocapture
# Run a module test group
cargo test config::tests::
cargo test bot::tests::
# Run by substring
cargo test upload_max_concurrent
```

Project uses unit tests in `src/*.rs` (`#[cfg(test)]` modules), not a separate `tests/` directory.

## Recommended Validation Sequence
```bash
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
# After local checks pass, produce Linux amd64 binary
cargo zigbuild --release --target x86_64-unknown-linux-gnu
```

## Git Workflow
- Prefer small, logical commits.
- Opportunistically create atomic commits for each independent change.
- Use conventional commit prefixes: `feat:`, `fix:`, `perf:`, `docs:`, `chore:`.
- Keep work local by default; do not push unless explicitly requested.
- Do not rewrite published history unless explicitly requested.

## Code Style Guidelines

### Rust Edition and Lints
- Crate uses Rust `edition = "2024"`.
- Global lint policy in `src/main.rs`: `#![warn(clippy::all, clippy::pedantic)]`.
- A small allow-list exists in `src/main.rs`; follow it.
- Avoid adding new `#[allow(...)]` unless it matches existing project patterns.

### Import Ordering
- Group imports with blank lines between groups:
  1) standard library (`std::...`)
  2) external crates (`tokio`, `serde`, `reqwest`, etc.)
  3) internal modules (`crate::...`)
- Keep import blocks stable and sorted within each group.

### Formatting and Structure
- Use default `rustfmt` style (4-space indentation, trailing commas in multiline literals).
- Keep functions focused; extract helpers for repeated logic.
- Use `#[must_use]` for pure helpers returning important values.

### Naming Conventions
- Functions/variables/modules: `snake_case`.
- Types/structs/enums/traits: `PascalCase`.
- Constants/statics: `UPPER_SNAKE_CASE`.
- Test names should describe behavior, not implementation details.

### Types, Serde, and Data Models
- Use explicit structs for API and DB payloads.
- Common derives: `Debug`, `Clone`, `Serialize`, `Deserialize`, `PartialEq`, `Eq` as needed.
- Use `#[serde(rename = "...")]` for field mapping.
- Use `#[serde(alias = "...")]` for backward-compatible input parsing.

### Error Handling
- Domain error type: `BotError` in `src/error.rs`.
- Internal result alias: `crate::error::Result<T>`.
- Use `anyhow::Result` + `Context` when richer propagation context is helpful.
- Prefer `?` for propagation.
- Avoid `unwrap()`/`expect()` in production paths; acceptable in tests/static init.
- Log meaningful context (`music_id`, URL, file path, sizes) before fallback/return.

### Async and Concurrency
- Use `async fn` for I/O-bound operations.
- Prefer `tokio::join!` for independent awaits.
- Use `tokio::spawn` for background workers that should not block handlers.
- Guard high-volume work with semaphores (download/upload/message task limits).
- Use `tokio::time::timeout` for operations that can stall.

### Logging
- Use `tracing::{debug, info, warn, error}` consistently.
- Keep `info` operator-meaningful, move noisy details to `debug`.
- Include actionable context in warnings and errors.

### Config Parsing
- Start from `Config::default()` and override parsed file values.
- For invalid numeric/boolean values, keep defaults and continue safely.
- Validate required fields (`bot.token`) and fail fast if missing.

### Database and SQLx
- Use parameterized queries with `.bind(...)`.
- Never interpolate user input into SQL strings.
- Keep SQLite WAL-oriented settings unless a measured reason requires change.
- Preserve `music_id` upsert semantics unless behavior change is intentional.

### Testing Practices
- Keep unit tests in `#[cfg(test)] mod tests` near end of source files.
- Use `#[tokio::test]` for async cases.
- Use unique temp file/DB names and clean up artifacts.
- Add focused regression tests for bug fixes and behavior-preserving refactors.

## Project Layout
```text
src/
|- main.rs         # entry point, clippy policy, allocator setup
|- bot.rs          # Telegram handlers and orchestration
|- music_api.rs    # NetEase API client and media helpers
|- audio_buffer.rs # disk/memory/hybrid buffering and cleanup
|- database.rs     # SQLite schema and queries
|- config.rs       # INI parsing and defaults
|- error.rs        # BotError and Result alias
|- memory.rs       # memory release helpers
`- utils.rs        # shared utilities
```

CI builds release binaries for Linux/macOS/Windows targets; performance helper script: `scripts/perf_compare.py`.
