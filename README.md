# Music163bot-Rust

[![Build and Release](https://github.com/Lemonawa/music163bot-rust/actions/workflows/build.yml/badge.svg)](https://github.com/Lemonawa/music163bot-rust/actions/workflows/build.yml)
[![CI](https://github.com/Lemonawa/music163bot-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/Lemonawa/music163bot-rust/actions/workflows/ci.yml)
[![CodeQL Security Scan](https://github.com/Lemonawa/music163bot-rust/actions/workflows/codeql.yml/badge.svg)](https://github.com/Lemonawa/music163bot-rust/actions/workflows/codeql.yml)
[![License: WTFPL](https://img.shields.io/badge/License-WTFPL-brightgreen.svg)](http://www.wtfpl.net/about/)

A Telegram bot that downloads and shares tracks from NetEase Cloud Music. Rust rewrite of [Music163bot-Go](https://github.com/XiaoMengXinX/Music163bot-Go).

Send a NetEase share link in any chat with the bot, and it sends back the audio file with embedded cover art and metadata. Also supports inline search, keyword search, lyrics, and batch downloads of playlists/albums.

## Supported links

```
https://music.163.com/song?id=xxxxx
https://music.163.com/#/song?id=xxxxx
https://music.163.com/playlist?id=xxxxx
https://music.163.com/album?id=xxxxx
https://163cn.tv/xxxxx
https://163cn.link/xxxxx
```

## Requirements

- Rust 1.85+ (edition 2024)
- SQLite3

## Install

Grab a binary from [Releases](https://github.com/Lemonawa/music163bot-rust/releases), or build it:

```bash
git clone https://github.com/Lemonawa/music163bot-rust.git
cd music163bot-rust
cargo build --release
# binary at target/release/music163bot-rust
```

## Configuration

```bash
cp config.ini.example config.ini
```

Minimal config — just set `bot.token` and you're good:

```ini
[bot]
token = YOUR_BOT_TOKEN

[database]
url = ./data/music_bot.db
```

### Full config reference

**[bot]**

| Key | Default | Description |
|-----|---------|-------------|
| `token` | (required) | Telegram bot token |
| `api` | `https://api.telegram.org` | Telegram API endpoint |
| `admin` | (none) | Comma-separated admin user IDs |
| `default_language` | `zh` | Fallback reply language when a chat has no `/lang` override and auto-detection does not apply (group chats). Must match a locale under `locales/` (`zh`, `en`). Private chats auto-detect from the user's Telegram client language. |

**[music]**

| Key | Default | Description |
|-----|---------|-------------|
| `api` | `https://music.163.com` | NetEase API endpoint |
| `music_u` | (none) | MUSIC_U cookie for paid/lossless tracks |

**[database]**

| Key | Default | Description |
|-----|---------|-------------|
| `url` | `./data/music_bot.db` | SQLite file path |

**[download]**

| Key | Default | Description |
|-----|---------|-------------|
| `dir` | `./downloads` | Cache directory |
| `storage_mode` | `disk` | `disk`, `memory`, or `hybrid` |
| `cover_mode` | `thumbnail` | `thumbnail`, `original`, or `both` |
| `max_concurrent` | `4` | Parallel downloads |
| `max_batch_tracks` | `20` | Max tracks per playlist/album request |
| `memory_threshold` | `100` | MB, hybrid mode threshold |
| `memory_buffer` | `100` | MB, memory safety buffer |
| `memory_max_file_mb` | `100` | MB, hard cap per file in memory mode |
| `max_disk_download_mb` | `2000` | MB, hard cap on total bytes streamed to disk per download |
| `connect_timeout_secs` | `10` | Download connection timeout |
| `chunk_size_kb` | `256` | Download chunk size |
| `pool_max_idle_per_host` | `2` | Download pool idle connections |

**[upload]**

| Key | Default | Description |
|-----|---------|-------------|
| `client_reuse_requests` | `0` | Rebuild upload client every N requests (0 = never) |
| `max_concurrent` | `1` | Parallel uploads |
| `timeout_secs` | `300` | Upload timeout |
| `pool_max_idle_per_host` | `1` | Upload pool idle connections |
| `pool_idle_timeout_secs` | `300` | Upload pool idle timeout |
| `local_file_uri` | `false` | Use local file URI (requires `telegram-bot-api --local`) |

**[maintenance]**

| Key | Default | Description |
|-----|---------|-------------|
| `memory_release_interval_requests` | `10` | Run memory release every N requests |
| `db_analyze_interval_requests` | `20` | Run SQLite ANALYZE every N requests |

**Top-level keys**

| Key | Default | Description |
|-----|---------|-------------|
| `loglevel` | `info` | Log level (trace/debug/info/warn/error) |

### Storage modes

- `disk` — writes audio files to `download.dir`. Low memory, stable.
- `memory` — processes everything in RAM. Faster, but needs more memory and a stable network.
- `hybrid` — files under `memory_threshold` MB go to memory, the rest to disk.

### Cover modes

Embedded covers are always 320x320 JPEG (Telegram requirement).

- `thumbnail` — downloads and embeds the 320x320 thumbnail
- `original` — embeds the full resolution cover
- `both` — embeds full resolution cover and generates a 320x320 thumbnail for Telegram preview

If cover download fails after 5 retries, the track is still uploaded without art.

## Running

```bash
./target/release/music163bot-rust
./target/release/music163bot-rust --config /path/to/config.ini
```

## Refreshing capped-quality caches (`refresh_hires`)

Every cached track is stored as a Telegram `file_id`; the bot re-forwards that copy on repeat requests instead of re-downloading. If a track was first fetched while the bot was capped at a lower quality tier (e.g. 16-bit `lossless` FLAC before the hires fix), the bot keeps sending that lower-quality copy forever. `refresh_hires` finds those rows so the bot re-fetches them at the current (hires-capable) candidate order the next time they are requested.

It probes the **same endpoint the bot uses** — `/eapi/song/enhance/player/url/v1` with `level=hires` (authenticated with your `MUSIC_U` cookie) — and compares the **served file size** (bytes) against the cached file size. A row is flagged for refresh **only** when the server returns a materially larger file (≥15% by default), meaning a genuinely higher-resolution download exists. This is ground truth, not catalog metadata — earlier versions that trusted the catalog's `sq`/`hr` bitrate fields produced false positives because those fields are nominal labels (e.g. `1411000` = CD PCM rate) that don't match the actual downloadable file.

**A valid `music_u` cookie is REQUIRED.** Supply it via `--music-u`, the `MUSIC_U` environment variable, or `--config <bot config.ini>` (reads `[music] music_u`).

**Grab the binary** from the [CI artifacts](https://github.com/Lemonawa/music163bot-rust/actions/workflows/ci.yml) (`refresh_hires-ci-linux-x86_64-*`) or a [Release](https://github.com/Lemonawa/music163bot-rust/releases) (`refresh_hires-*`). No Rust toolchain needed.

```bash
# Dry run — prints the refresh candidates, deletes nothing.
# Pass your music_u cookie (required for the tool to probe hires tier).
./refresh_hires --db ./data/music_bot.db --music-u "00B0..."

# Or read the cookie from the bot's config.ini (the [music] section).
./refresh_hires --db ./data/music_bot.db --config config.ini

# Apply — backs up the database to music_bot.db.bak, then deletes candidate
# rows in a single transaction. The bot re-downloads them on next request.
./refresh_hires --db ./data/music_bot.db --music-u "..." --apply

# Optional: reclaim SQLite space after deletion.
sqlite3 ./data/music_bot.db "VACUUM;"
```

Tunable flags: `--concurrency` (batch requests, default 3), `--batch-size` (ids per request, default 20), `--max-cached-bitrate` (probe ceiling, default 1_500_000 — covers all lossless-tier FLAC), `--min-ratio` (served/cached size ratio for upgrade, default 1.15 = 15% larger). Run `./refresh_hires --help` for the full list.

**Caveat — this probes the *download endpoint*, not the catalog.** A flagged row means the server would serve a genuinely larger file at `level=hires` than what you have cached. Whether your `music_u` cookie can actually *download* that tier depends on your VIP level; NetEase silently downgrades when it cannot serve it. The tool accounts for this by comparing served file sizes (not catalog bitrates), so a downgrade that yields the same-sized file is correctly NOT flagged. However, a refresh may still re-cache at a lower tier than the served size promised if the download itself is downgraded. This is harmless (the copy is never worse than before), but some flagged rows may not actually improve after a refresh.

## Bot commands

Set via `/setcommands` in @BotFather:

```
start - Start the bot or parse a song ID
music - Download/share music (keyword or ID)
netease - Same as /music
search - Search NetEase Cloud Music
lyric - Get song lyrics
lang - Set reply language (group admins only)
status - Cache and runtime stats (admin)
about - Version info
rmcache - Remove cache for a track (admin)
clearallcache - Clear all cache (admin, requires confirmation)
help - Usage help
```

Command menus are registered automatically at bot startup via
`setMyCommands`, localized per compiled locale (`locales/*.yml`,
`cmd_desc.*` keys); users' Telegram clients show the matching language when
they type `/`.

## License

[WTFPL](LICENSE)

## Credits

- [Music163bot-Go](https://github.com/XiaoMengXinX/Music163bot-Go)
