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

## Bot commands

Set via `/setcommands` in @BotFather:

```
start - Start the bot or parse a song ID
music - Download/share music (keyword or ID)
netease - Same as /music
search - Search NetEase Cloud Music
lyric - Get song lyrics
status - Cache and runtime stats (admin)
about - Version info
rmcache - Remove cache for a track (admin)
clearallcache - Clear all cache (admin, requires confirmation)
help - Usage help
```

## License

[WTFPL](LICENSE)

## Credits

- [Music163bot-Go](https://github.com/XiaoMengXinX/Music163bot-Go)
