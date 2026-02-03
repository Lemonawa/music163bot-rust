# First Download Performance Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Reduce first-download end-to-end latency by parallelizing API calls, minimizing redundant I/O, and improving upload connection reuse while keeping behavior stable.

**Architecture:** Rework the first-download flow in `process_music` to fetch song detail and a download URL concurrently, then only request FLAC when needed. Cache file size values to avoid repeated disk metadata I/O. Make upload connection pool settings configurable to tune reuse for faster uploads. Add stage timing logs to measure the improvements.

**Tech Stack:** Rust, tokio, reqwest, teloxide, sqlx, tracing.

---

### Task 1: Add perf formatting helper (TDD)

**Files:**
- Modify: `src/bot.rs`
- Test: `src/bot.rs` (unit tests module)

**Step 1: Write the failing test**

```rust
#[test]
fn perf_timer_formats_label_and_duration() {
    let label = "fetch_url";
    let formatted = format_perf(label, std::time::Duration::from_millis(12));
    assert!(formatted.contains("fetch_url"));
    assert!(formatted.contains("12"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test perf_timer_formats_label_and_duration`

Expected: FAIL with "cannot find function `format_perf`"

**Step 3: Write minimal implementation**

```rust
fn format_perf(label: &str, duration: std::time::Duration) -> String {
    format!("[{label}] {}ms", duration.as_millis())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test perf_timer_formats_label_and_duration`

Expected: PASS

**Step 5: Commit**

```bash
git add src/bot.rs
git commit -m "chore: add perf formatting helper"
```

---

### Task 2: Parallelize fetching song detail + download URL (TDD)

**Files:**
- Modify: `src/bot.rs`
- Test: `src/bot.rs` (unit tests module)

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn fetch_detail_and_url_in_parallel() {
    let (detail, url) = tokio::join!(async { 1 }, async { 2 });
    assert_eq!(detail, 1);
    assert_eq!(url, 2);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test fetch_detail_and_url_in_parallel`

Expected: FAIL if tokio test attribute not imported

**Step 3: Write minimal implementation**

- Add `use tokio;` in the tests module if needed.
- In `process_music`, replace the sequential `get_song_detail` + `get_song_url` with parallel `tokio::join!` for `get_song_detail` + a first URL fetch (320kbps or 128kbps fallback).
- Only request FLAC (999kbps) after the parallel fetch when `music_u` is present; if it fails, use the initial URL.

**Step 4: Run test to verify it passes**

Run: `cargo test fetch_detail_and_url_in_parallel`

Expected: PASS

**Step 5: Commit**

```bash
git add src/bot.rs
git commit -m "perf: parallelize song detail and url fetch"
```

---

### Task 3: Cache file size to reduce redundant I/O (TDD)

**Files:**
- Modify: `src/bot.rs`
- Test: `src/bot.rs` (unit tests module)

**Step 1: Write the failing test**

```rust
#[test]
fn cached_file_size_is_reused() {
    let size = cached_size(1024);
    assert_eq!(size, 1024);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test cached_file_size_is_reused`

Expected: FAIL with "cannot find function `cached_size`"

**Step 3: Write minimal implementation**

- Add a small `cached_size` helper for testability.
- In `download_and_send_music`, call `audio_buffer.size().await` once and reuse the cached value for logging, bitrate calculation, and upload throughput.

**Step 4: Run test to verify it passes**

Run: `cargo test cached_file_size_is_reused`

Expected: PASS

**Step 5: Commit**

```bash
git add src/bot.rs
git commit -m "perf: reuse computed file size to reduce I/O"
```

---

### Task 4: Make upload connection pool configurable (TDD)

**Files:**
- Modify: `src/config.rs`
- Modify: `src/bot.rs`
- Test: `src/config.rs` (unit tests module)

**Step 1: Write the failing test**

```rust
#[test]
fn upload_pool_config_parses() {
    let temp_path = std::env::temp_dir().join("music163bot_upload_pool.ini");
    let content = "bot.token=token\n\
upload.pool_max_idle_per_host=2\n\
upload.pool_idle_timeout_secs=120\n";
    std::fs::write(&temp_path, content).expect("write temp config");
    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");
    let _ = std::fs::remove_file(&temp_path);
    assert_eq!(loaded.upload_pool_max_idle_per_host, 2);
    assert_eq!(loaded.upload_pool_idle_timeout_secs, 120);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test upload_pool_config_parses`

Expected: FAIL because new fields are missing

**Step 3: Write minimal implementation**

- Add `upload_pool_max_idle_per_host` and `upload_pool_idle_timeout_secs` to `Config` with defaults.
- Parse `upload.pool_max_idle_per_host` and `upload.pool_idle_timeout_secs` in `Config::load`.
- Use these values in upload client builder: `pool_max_idle_per_host` + `pool_idle_timeout`.

**Step 4: Run test to verify it passes**

Run: `cargo test upload_pool_config_parses`

Expected: PASS

**Step 5: Commit**

```bash
git add src/config.rs src/bot.rs
git commit -m "perf: make upload connection pool configurable"
```

---

### Task 5: Add stage timing logs for first download (TDD)

**Files:**
- Modify: `src/bot.rs`
- Test: `src/bot.rs` (unit tests module)

**Step 1: Write the failing test**

```rust
#[test]
fn perf_log_includes_stage_label() {
    let s = format_perf("download", std::time::Duration::from_millis(50));
    assert!(s.contains("download"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test perf_log_includes_stage_label`

Expected: FAIL if format helper not wired

**Step 3: Write minimal implementation**

- In `process_music` and `download_and_send_music`, add stage timers and `tracing::info!` logs for:
  - `fetch_detail`
  - `fetch_url`
  - `download_audio`
  - `process_tags`
  - `upload_audio`

**Step 4: Run test to verify it passes**

Run: `cargo test perf_log_includes_stage_label`

Expected: PASS

**Step 5: Commit**

```bash
git add src/bot.rs
git commit -m "chore: add stage timing logs for first download"
```

---

### Task 6: Full verification

**Step 1:** `cargo check`

**Step 2:** `cargo clippy`

**Step 3:** `cargo test`

**Step 4:** Commit any final fixes (separate commits if needed)
