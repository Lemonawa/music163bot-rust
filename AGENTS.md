# Repository Guidelines

## Agent skills

### Issue tracker

Use the `gh` CLI for GitHub Issues in `Lemonawa/music163bot-rust`. See [issue tracker conventions](docs/agents/issue-tracker.md).

### Domain docs

This repository has one domain context. Read [CONTEXT.md](CONTEXT.md) and the relevant decisions in [docs/adr/](docs/adr/) before changing domain behavior. See [domain documentation conventions](docs/agents/domain.md).

## Project Structure & Module Organization

- `src/main.rs` is the bot CLI entry point. `src/lib.rs` exposes the shared library used by the bot and the `refresh_hires` maintenance binary in `src/bin/refresh_hires.rs`.
- `bot`, `music_api`, `audio_buffer`, `config`, and `database` each use a root module file and a subdirectory under `src/`. The Telegram client lives in `src/telegram/`. Shared support lives in `src/error.rs`, `src/memory.rs`, and `src/utils.rs`.
- `config.ini.example` documents runtime settings. User-facing translations in `locales/` are embedded at compile time.

## Build, Test, and Development Commands

Use stable Rust with the `rustfmt` and `clippy` components, as CI does.

- `cargo build --release` builds both binaries.
- `cargo run --release --bin music163bot-rust -- --config config.ini` runs the bot with an explicit config file.
- See the [refresh_hires guide](README.md#refreshing-capped-quality-caches-refresh_hires) for maintenance commands and options.
- `cargo zigbuild --release --target x86_64-unknown-linux-gnu` is an option for local macOS-to-Linux cross-compilation with Zig and `cargo-zigbuild` installed.

## Coding Style & Naming Conventions

Use Rust 2024 conventions and `rustfmt` defaults (4-space indentation). Prefer import grouping as `std`, external crates, then `crate::...`. Naming: `snake_case` for functions/variables/modules, `PascalCase` for types/traits, `UPPER_SNAKE_CASE` for constants. Prefer `crate::error::Result<T>` and `?` over `unwrap()`/`expect()` in production paths.

## Testing Guidelines

Keep tests close to the implementation, use behavior-focused names, and use `#[tokio::test]` for async paths. Existing test modules include `src/bot/tests.rs` and `src/music_api/tests/`.

The mock HTTP helpers `write_json_response`, `find_byte_sequence`, and `parse_content_length` exist in both `src/bot/tests/telegram.rs` and `src/music_api/tests.rs`. Extract a shared `#[cfg(test)]` module if a third consumer needs them. Mock HTTP tests bind loopback ports. If sandboxing blocks the bind, rerun the suite with escalation.

- `cargo test --lib bot::tests::`
- `cargo test --lib config::tests::upload_max_concurrent_parses -- --exact --nocapture`

## Commit & Pull Request Guidelines

Use Conventional Commits (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`). Keep commits small and single-purpose. In PRs, describe the intent, key changes, verification commands, and any config or database impact.

## Agent Workflow

After each major work round (for example, a feature batch or bugfix batch), and before each `git push`, run the standard Rust verification sequence:

```bash
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
```

For Codex, request sandbox escalation before running commands that access the network, including `cargo update`, `curl`, `git clone`, and `git push`. Run normally after approval and avoid using `--offline` as a default workaround.

## CI/CD

- [CI](.github/workflows/ci.yml) runs formatting, Clippy, tests, and a Linux x86-64 release build on relevant pushes and pull requests to `main` or on manual dispatch.
- [Build and Release](.github/workflows/build.yml) runs `cargo build --release --target` for Linux, macOS, and Windows on `v*` tag pushes or manual dispatch.
- [Security Audit](.github/workflows/audit.yml) runs `cargo audit` daily, on changes to its inputs, or on manual dispatch.
- [CodeQL](.github/workflows/codeql.yml) scans Rust on pushes and pull requests to `main` and on a weekly schedule.
