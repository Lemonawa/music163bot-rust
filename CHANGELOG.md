# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/2.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.24] - Unreleased

Two fixes plus a large internal restructuring since 1.1.23. For operators:
the INI config surface is unchanged — drop-in binary upgrade. For anyone
building on the source: see the first Changed entry.

### Fixed

- eapi song-url responses with `null` `br`/`size`/`type` fields (emitted when
  a song is unavailable at the requested quality) failed deserialization for
  the whole request. `SongUrl` now treats null as 0 and proceeds down the
  normal fallback chain, with a regression test. This restores the behavior
  refresh_hires shipped with in v1.1.21 (22d4163); the hardening was silently
  lost when the tool switched from hand-copied DTOs to the library.
- Telegram 429 responses dropped the server-provided `parameters.retry_after`,
  so retry pacing was inferred by regex-scraping the rendered error text.
  The hint is now parsed and read typed (429-only), making waits accurate and
  immune to non-429 errors that merely mention "retry after".

### Changed

- **For source-level consumers**: the crate now has a lib target
  (`src/lib.rs`); `main.rs` is a thin binary adapter. Maintenance binaries
  must `use music163bot_rust::…` instead of `#[path]`-including library
  files or hand-copying domain logic (ADR-0002).
  `MusicApi::get_song_detail_and_best_url(song_id)` no longer takes a
  bitrate-candidates argument (the ladder is chosen internally), and the
  `music_u` field is now private.
- Config's 30+ fields are grouped by reader into `storage` / `transfer` /
  `maintenance` (serde-flattened). The INI key surface and serialized form
  are unchanged; existing config files load as-is.
- Dead config keys `maxretrytimes` / `downloadtimeout` are now explicitly
  accepted-and-ignored (with a debug log note). Nothing has consumed them
  since the retry/timeout reworks.
- `cargo run` now requires an explicit `--bin music163bot-rust` (Cargo no
  longer infers a default binary once a lib target exists).

### Removed

- Dead code: `src/test_helpers.rs` (never wired into the module tree),
  `join_futures`, `should_set_upload_pool_idle_timeout`, `is_timeout_error`,
  the byte-identical `is_spawnable_command_text`, and the `get_upload_bot`
  accessor.

### Internal (no change to how the bot is used)

- The download pipeline's parameter relay collapsed into a single
  `DownloadCtx`; the Chat Language is resolved once at entry instead of
  three times inside the retry loop.
- The Chat Language seam takes `&BotState` instead of four raw parameters.
- Error rendering unified behind `BotError::sanitized_chain()`, removing 47
  duplicated spellings (one of which was missing the redaction half).
- `bot.rs` shrank from a 144-line / 112-name re-export hub to 37 lines;
  `upload.rs`/`support.rs` split by concern into replies / admin /
  maintenance / permits / music_ui / intake / tagging / upload_client.
- refresh_hires dropped ~150 lines of hand-copied domain logic (eapi
  request/decode ladder, DTOs) in favor of the library's
  `MusicApi::get_served_sizes_batch`.

## [1.1.23] - 2026-08-22

### Fixed

- Silently ignore non-song NetEase share pages instead of erroring.

### Added

- Localized bot command menus; zh/en bilingual support with per-chat
  language (see ADR-0001).

## [1.1.22] - 2026-05-16

### Fixed

- Replaced teloxide with a hand-rolled Telegram layer; unique download
  paths, saturating size caps, and non-fatal cache-save failures.

## [1.1.21] - 2026-05-11

### Added

- `refresh_hires` maintenance tool: classifies refreshable cache rows by the
  eapi-served file size (ground truth, not catalog metadata); dry run by
  default, `--apply` backs up the database before deleting.

[Unreleased]: https://github.com/Lemonawa/music163bot-rust/compare/v1.1.24...HEAD
[1.1.24]: https://github.com/Lemonawa/music163bot-rust/compare/v1.1.23...v1.1.24
[1.1.23]: https://github.com/Lemonawa/music163bot-rust/compare/v1.1.22...v1.1.23
[1.1.22]: https://github.com/Lemonawa/music163bot-rust/compare/v1.1.21...v1.1.22
[1.1.21]: https://github.com/Lemonawa/music163bot-rust/releases/tag/v1.1.21
