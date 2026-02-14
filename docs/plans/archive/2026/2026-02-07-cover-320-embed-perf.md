# Cover 320 Embed Performance Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 保证音质不变、封面固定 320x320 JPEG，其他尽量追求性能；默认 cover_mode=thumbnail 且仍嵌入封面。

**Architecture:** 封面下载/处理只走一次 `download_album_art_data()`（内部 resize 到 320），同一份 bytes 同时用于“内嵌封面”和“Telegram 缩略图”，移除原图下载路径。音频下载与封面下载仍并行。

**Tech Stack:** Rust, tokio, reqwest, image, metaflac/id3, teloxide.

---
### Task 1: 用测试锁定“Thumbnail 仍嵌入封面”的新策略

**Files:**
- Modify: `src/bot.rs` (tests module)
- Modify: `src/music_api.rs` (tests module)

**Step 1: 写测试**

```rust
#[test]
fn cover_policy_embeds_for_thumbnail_mode() {
    let policy = resolve_cover_policy(CoverMode::Thumbnail);
    assert!(policy.embed_cover);
    assert!(policy.download_thumbnail);
    assert!(!policy.download_original);
}
```

```rust
#[test]
fn thumbnail_resize_is_320_square_jpeg() {
    let out = resize_album_art_to_thumbnail(&png_bytes).expect("thumbnail bytes");
    let img = image::load_from_memory(&out).expect("decode");
    assert_eq!(img.width(), 320);
    assert_eq!(img.height(), 320);
}
```

**Step 2: 跑测试确认（可能一项已通过）**

Run: `cargo test bot::tests::cover_policy_embeds_for_thumbnail_mode -- --exact`
Expected: FAIL（改逻辑前）

Run: `cargo test music_api::tests::thumbnail_resize_is_320_square_jpeg -- --exact`
Expected: PASS（若当前已是 320）

**Step 3: 最小实现**

- 更新 `resolve_cover_policy`：`embed_cover = download_original || download_thumbnail`

**Step 4: 复测**

同上两个 test，期望 PASS。

**Step 5: Commit**

`git add src/bot.rs src/music_api.rs`

`git commit -m "perf: embed cover in thumbnail mode and lock 320px resize"`

---
### Task 2: 统一封面下载路径为 320 JPEG

**Files:**
- Modify: `src/bot.rs` (download_and_send_music 中 artwork_future)

**Step 1: 写测试（纯逻辑，避免网络）**

新增一个小 helper（如 `should_download_cover(policy)`），验证：只要 `embed_cover || download_thumbnail` 就需要下载 320。

**Step 2: 跑测试确认失败**

Run: `cargo test bot::tests::cover_policy_requires_download_when_embed_or_thumbnail -- --exact`
Expected: FAIL

**Step 3: 最小实现**

- artwork_future 逻辑改为：
  - 若 `embed_cover || download_thumbnail`：调用 `download_album_art_data(pic_url)` 一次，得到 320 JPEG bytes
  - 该 bytes 同时用于：
    - `apply_tags_in_blocking` 的 artwork_data（嵌入封面）
    - `ThumbnailBuffer::new`（缩略图）
  - 移除 `download_album_art_original` 和本地 resize 的路径

**Step 4: 复测**

Run: `cargo test bot::tests::cover_policy_requires_download_when_embed_or_thumbnail -- --exact`
Expected: PASS

**Step 5: Commit**

`git add src/bot.rs`

`git commit -m "perf: reuse 320px cover for embed + thumbnail"`

---
### Task 3: 文档与配置说明

**Files:**
- Modify: `README.md`
- Modify: `config.ini.example`

**Step 1: 更新说明**

- 默认 `cover_mode=thumbnail`
- 内嵌封面固定 320x320 JPEG（Telegram 规范）
- 若需高分辨率封面，未来可通过配置扩展

**Step 2: Commit**

`git add README.md config.ini.example`

`git commit -m "docs: clarify 320px embedded cover behavior"`

---
### Task 4: Disk 模式下载路径使用流式 copy

**Files:**
- Modify: `src/bot.rs`
- Modify: `src/audio_buffer.rs`

**Step 1: 写测试（非网络，逻辑测试）**

为 `AudioBuffer` 增加一个小 helper（例如 `is_disk()`），并测试在 Disk/Memory 两种模式下分支正确。目标是确保后续的“Disk 走流式 copy”逻辑可覆盖。

**Step 2: 跑测试确认失败**

Run: `cargo test audio_buffer::tests::audio_buffer_is_disk -- --exact`
Expected: FAIL

**Step 3: 最小实现**

- 为 `AudioBuffer` 增加 `is_disk()` 方法
- 在下载路径中，当 `AudioBuffer::Disk` 时：
  - 使用 `tokio_util::io::StreamReader` + `tokio::io::copy` 直接写入 file
  - 用 copy 返回值作为 `downloaded` 字节数
- Memory 模式保持现有 buffer 写入逻辑

**Step 4: 复测**

Run: `cargo test audio_buffer::tests::audio_buffer_is_disk -- --exact`
Expected: PASS

**Step 5: Commit**

`git add src/bot.rs src/audio_buffer.rs`

`git commit -m "perf: stream disk downloads with tokio io copy"`

---
### Task 5: 封面 bytes 复用，减少 clone

**Files:**
- Modify: `src/audio_buffer.rs`
- Modify: `src/bot.rs`

**Step 1: 写测试**

为 `ThumbnailBuffer::Memory` 增加一个测试，验证 `Bytes` 复用路径可用：

```rust
#[test]
fn thumbnail_buffer_memory_bytes_roundtrip() {
    let data = bytes::Bytes::from_static(b"abc");
    let buf = ThumbnailBuffer::from_bytes(data.clone());
    assert_eq!(buf.get_data().unwrap_or_default(), b"abc");
}
```

**Step 2: 跑测试确认失败**

Run: `cargo test audio_buffer::tests::thumbnail_buffer_memory_bytes_roundtrip -- --exact`
Expected: FAIL

**Step 3: 最小实现**

- `ThumbnailBuffer::Memory` 改为存 `Bytes`
- 新增 `from_bytes` 构造
- `get_data()` 对 `Bytes` 做 `to_vec()` 保持现有接口
- `raw_send_file` 里对 thumbnail 使用 `bytes.clone()`（O(1)）
- `download_and_send_music` 里把 320 封面 bytes 用 `Bytes` 复用给：
  - `apply_tags_in_blocking`（改为 `Option<Bytes>` 参数）
  - `ThumbnailBuffer::new`（改为接收 `Bytes` 或新构造）

**Step 4: 复测**

Run: `cargo test audio_buffer::tests::thumbnail_buffer_memory_bytes_roundtrip -- --exact`
Expected: PASS

**Step 5: Commit**

`git add src/audio_buffer.rs src/bot.rs`

`git commit -m "perf: reuse cover bytes across embed and thumbnail"`

---
### Task 6: 验证

Run:
- `cargo fmt`
- `cargo clippy -- -D warnings`
- `cargo test`
- `cargo zigbuild --release --target x86_64-unknown-linux-gnu`
