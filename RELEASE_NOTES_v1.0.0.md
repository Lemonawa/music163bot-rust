# Release Notes - v1.0.0

We're excited to announce the first stable release of Music163bot-Rust, a high-performance Telegram bot for NetEase Cloud Music built with Rust!

## What's New

This is the first production-ready release, featuring:

### Core Features
- **Link Parsing**: Parse and download songs from NetEase Cloud Music share links
- **Inline Mode**: Search and share music directly in any chat using `@botname` with cover preview
- **Search Functionality**: Use `/search` command to find songs by keywords in private chats
- **Smart Caching**: Automatic song caching with support for FLAC lossless format
- **Lyrics Support**: Fetch and display song lyrics
- **Metadata Embedding**: Automatically embed album covers into downloaded music files (ID3/FLAC)
- **Statistics**: View cache usage and user statistics
- **High Performance**: Built on Tokio async runtime for fast and responsive operation

### Supported Link Formats
- `https://music.163.com/song?id=xxxxx`
- `https://music.163.com/#/song?id=xxxxx`
- `https://163cn.tv/xxxxx`
- `https://163cn.link/xxxxx`

### Technical Highlights
- Fully asynchronous architecture using Tokio
- Efficient HTTP handling with reqwest
- SQLite database with sqlx for caching and statistics
- Robust error handling with anyhow and thiserror
- Comprehensive logging with tracing
- Support for premium songs with MUSIC_U cookie configuration
- **Code Quality**: Passes all clippy pedantic lints for maximum Rust code quality and safety

## Installation

Download the pre-built binary from the [Releases](https://github.com/Lemonawa/music163bot-rust/releases) page, or build from source:

```bash
git clone https://github.com/Lemonawa/music163bot-rust.git
cd music163bot-rust
cargo build --release
```

The compiled binary will be available at `target/release/music163bot-rust`.

## Configuration

1. Copy the example config file:
   ```bash
   cp config.ini.example config.ini
   ```

2. Edit `config.ini` and set your Telegram bot token

3. Run the bot:
   ```bash
   ./target/release/music163bot-rust
   ```

## Requirements

- Rust 1.70+ (for building from source)
- SQLite3

## Acknowledgments

This project is a Rust rewrite of [Music163bot-Go](https://github.com/XiaoMengXinX/Music163bot-Go).

## License

[WTFPL License](LICENSE)

---

Enjoy your music! 🎵
