# Repository Guidelines

## Project Structure & Module Organization
Core application code is in `src/`. Entry/runtime setup is in `src/main.rs`. Major domains are split into root files plus submodules: `src/bot.rs` + `src/bot/`, `src/music_api.rs` + `src/music_api/`, `src/audio_buffer.rs` + `src/audio_buffer/`, `src/config.rs` + `src/config/`, and `src/database.rs` + `src/database/`. Common support code lives in `src/error.rs`, `src/memory.rs`, `src/utils.rs`, and `src/test_helpers.rs` (`#[cfg(test)]` mock HTTP utilities). Tests are colocated with source files (for example `src/bot/tests.rs`, `src/music_api/tests/*.rs`) rather than a top-level `tests/` directory.

## Build, Test, and Development Commands
- `cargo check`: fast compile validation while iterating.
- `cargo build --release`: optimized build artifact.
- `cargo run --release -- --config config.ini`: run the bot with explicit config.
- `cargo fmt -- --check`: formatting gate.
- `cargo clippy -- -D warnings`: strict lint gate.
- `cargo test`: run all unit/async tests.
- `cargo zigbuild --release --target x86_64-unknown-linux-gnu`: recommended macOS→Linux cross-compilation build (requires `cargo install cargo-zigbuild`).

Recommended verification sequence before merge (mirrors CI): `cargo fmt -- --check && cargo check && cargo clippy -- -D warnings && cargo test`.

## Coding Style & Naming Conventions
Use Rust 2024 conventions and `rustfmt` defaults (4-space indentation). Prefer import grouping as `std`, external crates, then `crate::...`. Naming: `snake_case` for functions/variables/modules, `PascalCase` for types/traits, `UPPER_SNAKE_CASE` for constants. Prefer `crate::error::Result<T>` and `?` over `unwrap()`/`expect()` in production paths.

## Testing Guidelines
Use focused, behavior-named tests close to the implementation. Use `#[tokio::test]` for async paths. Helpful commands:
- `cargo test bot::tests::`
- `cargo test config::tests::upload_max_concurrent_parses -- --exact --nocapture`

## Commit & Pull Request Guidelines
Use Conventional Commits (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`). Keep commits small and single-purpose. PRs should include: intent, key changes, validation commands run, and any config/database impact.

## Agent Workflow
After each major work round (for example, a feature batch or bugfix batch), and before each `git push`, run the standard Rust verification sequence:

```bash
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
```

For Codex: any command that involves network access (for example `cargo update`, `pip install`, `curl`, `git clone`) must request sandbox escalation permission first, then execute normally after approval. Do not force `--offline` as a default workaround.

## CI/CD
Four GitHub Actions workflows run on push/PR to `main`:
- `.github/workflows/ci.yml` — format check, clippy, tests, and release build (x86_64-unknown-linux-gnu)
- `.github/workflows/build.yml` — multi-platform release builds with `cargo zigbuild`
- `.github/workflows/audit.yml` — dependency vulnerability scanning
- `.github/workflows/codeql.yml` — CodeQL security analysis
