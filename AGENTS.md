# Repository Guidelines

## Project Structure & Module Organization
Core code lives in `src/`:
- `main.rs`: entry point, global lint policy, runtime setup.
- `bot.rs`: Telegram handlers and orchestration.
- `music_api.rs`, `audio_buffer.rs`, `database.rs`: API, buffering, and persistence paths.
- `config.rs`, `error.rs`, `memory.rs`, `utils.rs`: config parsing, error model, helpers.

Tests are colocated in each module with `#[cfg(test)]`; this repo does **not** use a top-level `tests/` directory. Utility scripts are in `scripts/` (for example `scripts/perf_compare.py`).

## Build, Test, and Development Commands
- `cargo check`: fast compile validation during iteration.
- `cargo build` / `cargo build --release`: debug or optimized builds.
- `cargo run --release -- --config config.ini`: run the bot with explicit config.
- `cargo fmt -- --check`: formatting gate used in CI.
- `cargo clippy -- -D warnings`: strict lint gate.
- `cargo test`: run full unit/integration test set.
- `cargo zigbuild --release --target x86_64-unknown-linux-gnu`: preferred macOS-to-Linux artifact build.

Recommended pre-merge sequence: `cargo fmt -- --check && cargo check && cargo clippy -- -D warnings && cargo test`.

## Coding Style & Naming Conventions
Use Rust 2024 edition conventions and default `rustfmt` formatting (4-space indentation). Keep imports grouped in this order: `std`, external crates, then `crate::...`. Naming rules:
- `snake_case`: functions, modules, variables.
- `PascalCase`: structs, enums, traits.
- `UPPER_SNAKE_CASE`: constants/statics.

Prefer `crate::error::Result<T>` / `BotError` for domain errors, propagate with `?`, and avoid `unwrap()`/`expect()` in production code.

## Testing Guidelines
Write focused tests near the related source file, using `#[tokio::test]` for async paths. Name tests by behavior (not implementation detail), and add regression tests for bug fixes. Helpful commands:
- Exact test: `cargo test config::tests::upload_max_concurrent_parses -- --exact --nocapture`
- Module scope: `cargo test bot::tests::`

## Commit & Pull Request Guidelines
Follow conventional commit style seen in history: `feat:`, `fix:`, `perf:`, `docs:`, `style:`, `test:`, `chore:` (scopes are fine, e.g. `chore(deps): ...`). Keep commits small and atomic.

PRs should include:
- What changed and why.
- Verification commands run (for example `cargo test`, `cargo clippy -- -D warnings`).
- Any config/database impact and rollback notes when relevant.
