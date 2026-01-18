# Music163bot-Rust
[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2FLemonawa%2Fmusic163bot-rust.svg?type=shield)](https://app.fossa.com/projects/git%2Bgithub.com%2FLemonawa%2Fmusic163bot-rust?ref=badge_shield)


一个用 Rust 语言重写的网易云音乐 Telegram 机器人，基于 [Music163bot-Go](https://github.com/XiaoMengXinX/Music163bot-Go) 项目。

## 功能特性

- 🎵 **链接解析**: 支持解析网易云音乐分享链接。
- 📱 **Inline 模式**: 支持在任何聊天中使用 `@botname` 搜索并分享音乐（带封面预览）。
- 🔍 **关键词搜索**: 支持私聊中使用 `/search` 搜索音乐。
- 📁 **完善缓存**: 自动缓存歌曲，支持 FLAC 无损格式。
- 🎤 **歌词获取**: 支持获取歌曲歌词。
- 🖼️ **封面嵌入**: 自动为下载的音乐文件嵌入 ID3/FLAC 封面。
- 📊 **统计信息**: 查看缓存占用和用户统计。
- 🚀 **智能存储**: 支持磁盘/内存/混合模式，优化下载性能和资源占用（v1.1.0+）。
- ⚡ **高性能**: 基于 Tokio 异步运行时，响应迅速。

## 支持的链接格式

- `https://music.163.com/song?id=xxxxx`
- `https://music.163.com/#/song?id=xxxxx`
- `https://163cn.tv/xxxxx`
- `https://163cn.link/xxxxx`

## 安装和使用

### 前置要求

- Rust 1.70+ 
- SQLite3

### 下载

从 [Releases](https://github.com/Lemonawa/music163bot-rust/releases) 页面下载最新的预编译版本，或者从源码构建。

### 构建

```bash
git clone https://github.com/Lemonawa/music163bot-rust.git
cd music163bot-rust
cargo build --release
```

构建完成后，可执行文件位于 `target/release/music163bot-rust`。

### 配置

1. 复制配置文件模板：
   ```bash
   cp config.ini.example config.ini
   ```

2. 编辑 `config.ini` 配置文件：
    - 在 `[bot]` 部分设置你的 `bot_token`。
    - 可选：在 `[music]` 部分设置 `music_u` cookie 来访问付费歌曲。
    - 调整 `cache_dir` 和 `database` 路径。
    - （v1.1.0+）在 `[download]` 部分配置存储模式。

### 存储模式配置 (v1.1.0+)

在 `[download]` 部分设置存储模式：

- `disk`: 传统磁盘文件（稳定，低内存）
- `memory`: 内存处理（更快，减少磁盘I/O）
- `hybrid`: 智能选择（推荐，小文件用内存）

可选参数：

- `memory_threshold`: 混合模式阈值（默认 100MB）
- `memory_buffer`: 内存安全缓冲区（默认 100MB）

示例配置：

```ini
[download]
storage_mode = hybrid
memory_threshold = 100
memory_buffer = 100
```

### 运行

```bash
# 使用发布版本运行
./target/release/music163bot-rust

# 指定配置文件
./target/release/music163bot-rust --config /path/to/config.ini
```

## 机器人命令设置

请在 `@BotFather` 中使用 `/setcommands` 设置以下列表：

```text
start - 开始使用机器人或解析歌曲 ID
music - 下载/分享网易云音乐 (支持搜索关键词或 ID)
netease - 下载/分享网易云音乐 (等同于 /music)
search - 搜索网易云音乐
lyric - 获取歌曲歌词
status - 查看机器人运行状态和缓存信息
about - 关于机器人
rmcache - [管理员] 清理指定音乐的缓存
help - 显示详细使用帮助
```

## 技术栈

- **tokio** - 异步运行时
- **teloxide** - Telegram Bot 框架
- **reqwest** - HTTP 客户端
- **sqlx** - 异步 SQL 工具
- **id3 / metaflac** - 音乐标签处理

## License

[WTFPL License](LICENSE)


[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2FLemonawa%2Fmusic163bot-rust.svg?type=large)](https://app.fossa.com/projects/git%2Bgithub.com%2FLemonawa%2Fmusic163bot-rust?ref=badge_large)

## 致谢

- [Music163bot-Go](https://github.com/XiaoMengXinX/Music163bot-Go) - 原项目参考
- 网易云音乐 API 相关项目