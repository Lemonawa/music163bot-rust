# Performance Optimization Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve concurrent cache performance, reduce config parsing boilerplate, and optimize database queries.

**Architecture:** Three independent phases that can be executed in parallel by separate subagents. Phase 1 replaces `std::sync::Mutex<HashMap>` caches with `DashMap` for lock-free concurrent reads. Phase 2 extracts generic config parsing helpers to eliminate ~100 lines of boilerplate. Phase 4 replaces `SELECT *` with explicit column lists and adds a throttled memory stats refresh.

**Tech Stack:** Rust 2024 edition, dashmap 6.1.0, tokio, sqlx, sysinfo

**New dependency:** `dashmap = "6.1.0"` in `Cargo.toml`

---

## Phase 1: DashMap Cache Replacement (subagent A)

**Summary:** Replace the three `std::sync::Mutex<HashMap<K, TimedCacheEntry<V>>>` fields in `MusicApi` with `DashMap<K, TimedCacheEntry<V>>`. This eliminates mutex contention on cache reads (the hot path) and removes the `lock_or_recover` helper.

### Task 1.1: Add dashmap dependency

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add dashmap to Cargo.toml**

In `Cargo.toml`, after the `# Utilities` section (around line 29), add `dashmap`:

```toml
dashmap = "6.1.0"
```

Place it in the Utilities group, alphabetically (after `chrono`, before `md5`).

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles without errors

**Step 3: Commit**

```
chore: add dashmap 6.1.0 dependency
```

---

### Task 1.2: Replace cache fields and remove lock_or_recover

**Files:**
- Modify: `src/music_api.rs:1,21-29,175-180,182-235`

**Step 1: Update imports**

Replace:
```rust
use std::collections::HashMap;
```
With:
```rust
use std::collections::HashMap;

use dashmap::DashMap;
```

`HashMap` is still used for `params` in request building, so keep both.

**Step 2: Replace struct fields**

In the `MusicApi` struct (lines 21-29), replace the three cache fields:

```rust
// OLD
song_detail_cache: std::sync::Mutex<HashMap<u64, TimedCacheEntry<SongDetail>>>,
song_url_cache: std::sync::Mutex<HashMap<(u64, u64), TimedCacheEntry<SongUrl>>>,
song_lyric_cache: std::sync::Mutex<HashMap<u64, TimedCacheEntry<String>>>,
```

With:

```rust
// NEW
song_detail_cache: DashMap<u64, TimedCacheEntry<SongDetail>>,
song_url_cache: DashMap<(u64, u64), TimedCacheEntry<SongUrl>>,
song_lyric_cache: DashMap<u64, TimedCacheEntry<String>>,
```

**Step 3: Delete `lock_or_recover` function**

Remove the entire function (lines 175-180):

```rust
fn lock_or_recover<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
```

**Step 4: Update constructor `new_with_options`**

In `new_with_options` (around line 227-234), replace:

```rust
song_detail_cache: std::sync::Mutex::new(HashMap::new()),
song_url_cache: std::sync::Mutex::new(HashMap::new()),
song_lyric_cache: std::sync::Mutex::new(HashMap::new()),
```

With:

```rust
song_detail_cache: DashMap::new(),
song_url_cache: DashMap::new(),
song_lyric_cache: DashMap::new(),
```

**Step 5: Verify it compiles (expect cache method errors)**

Run: `cargo check`
Expected: errors in cache getter/setter methods (next task fixes them)

---

### Task 1.3: Rewrite cache getter/setter methods

**Files:**
- Modify: `src/music_api.rs:237-291`

**Step 1: Rewrite all six cache methods**

Replace the current implementations with DashMap equivalents:

```rust
fn get_cached_song_detail(&self, song_id: u64) -> Option<SongDetail> {
    let now = Instant::now();
    let entry = self.song_detail_cache.get(&song_id)?;
    if entry.is_fresh_at(now) {
        Some(entry.value.clone())
    } else {
        drop(entry);
        self.song_detail_cache.remove(&song_id);
        None
    }
}

fn cache_song_detail(&self, song_id: u64, detail: SongDetail) {
    self.song_detail_cache
        .insert(song_id, TimedCacheEntry::new(detail, SONG_DETAIL_CACHE_TTL));
}

fn get_cached_song_url(&self, song_id: u64, br: u64) -> Option<SongUrl> {
    let key = song_url_cache_key(song_id, br);
    let now = Instant::now();
    let entry = self.song_url_cache.get(&key)?;
    if entry.is_fresh_at(now) {
        Some(entry.value.clone())
    } else {
        drop(entry);
        self.song_url_cache.remove(&key);
        None
    }
}

fn cache_song_url(&self, song_id: u64, br: u64, song_url: SongUrl) {
    let key = song_url_cache_key(song_id, br);
    self.song_url_cache
        .insert(key, TimedCacheEntry::new(song_url, SONG_URL_CACHE_TTL));
}

fn get_cached_song_lyric(&self, song_id: u64) -> Option<String> {
    let now = Instant::now();
    let entry = self.song_lyric_cache.get(&song_id)?;
    if entry.is_fresh_at(now) {
        Some(entry.value.clone())
    } else {
        drop(entry);
        self.song_lyric_cache.remove(&song_id);
        None
    }
}

fn cache_song_lyric(&self, song_id: u64, lyric: String) {
    self.song_lyric_cache
        .insert(song_id, TimedCacheEntry::new(lyric, SONG_LYRIC_CACHE_TTL));
}
```

Key difference from old code: DashMap's `.get()` returns an `Option<Ref<K,V>>` which holds a read lock on the shard. We must `drop(entry)` before calling `.remove()` to avoid deadlock (remove takes a write lock on the same shard).

**Step 2: Verify it compiles and tests pass**

Run: `cargo check && cargo test music_api::tests::`
Expected: all existing tests pass

**Step 3: Commit**

```
perf: replace Mutex<HashMap> caches with DashMap for lock-free reads
```

---

### Task 1.4: Write tests for DashMap cache behavior

**Files:**
- Modify: `src/music_api.rs` (test module at end of file)

**Step 1: Write the test for concurrent cache access**

Add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn dashmap_cache_insert_and_retrieve() {
    let api = MusicApi::new(None, "http://localhost".to_string());

    // Cache a song detail
    let detail = super::SongDetail {
        id: 42,
        name: "Test Song".to_string(),
        dt: Some(180_000),
        ar: None,
        al: None,
    };
    api.cache_song_detail(42, detail.clone());

    // Should be retrievable
    let cached = api.get_cached_song_detail(42);
    assert!(cached.is_some());
    assert_eq!(cached.unwrap().name, "Test Song");

    // Unknown ID should return None
    assert!(api.get_cached_song_detail(999).is_none());
}

#[test]
fn dashmap_cache_url_keyed_by_bitrate() {
    let api = MusicApi::new(None, "http://localhost".to_string());

    let url_320 = super::SongUrl {
        id: 1,
        url: "http://example.com/320".to_string(),
        br: 320_000,
        size: 1000,
        md5: String::new(),
        format: "mp3".to_string(),
    };
    let url_128 = super::SongUrl {
        id: 1,
        url: "http://example.com/128".to_string(),
        br: 128_000,
        size: 500,
        md5: String::new(),
        format: "mp3".to_string(),
    };

    api.cache_song_url(1, 320_000, url_320);
    api.cache_song_url(1, 128_000, url_128);

    let cached_320 = api.get_cached_song_url(1, 320_000).unwrap();
    assert_eq!(cached_320.url, "http://example.com/320");

    let cached_128 = api.get_cached_song_url(1, 128_000).unwrap();
    assert_eq!(cached_128.url, "http://example.com/128");

    // Different song ID should not match
    assert!(api.get_cached_song_url(2, 320_000).is_none());
}
```

**Step 2: Run the new tests**

Run: `cargo test music_api::tests::dashmap -- --nocapture`
Expected: PASS

**Step 3: Run full validation**

Run: `cargo fmt -- --check && cargo clippy -- -D warnings && cargo test`
Expected: all pass

**Step 4: Commit**

```
test: add DashMap cache insert/retrieve tests
```

---

## Phase 2: Config Parsing Helpers (subagent B)

**Summary:** Extract two generic helper functions (`parse_field` and `parse_bool_field`) to eliminate repeated parsing boilerplate in `Config::load`. Reduces ~100 lines without changing any behavior.

### Task 2.1: Add generic parsing helpers

**Files:**
- Modify: `src/config.rs:124-134`

**Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `src/config.rs`:

```rust
#[test]
fn parse_field_returns_parsed_value() {
    let result: u32 = super::parse_field("42", 0, "test_key");
    assert_eq!(result, 42);
}

#[test]
fn parse_field_returns_default_on_invalid() {
    let result: u32 = super::parse_field("not_a_number", 99, "test_key");
    assert_eq!(result, 99);
}

#[test]
fn parse_bool_field_updates_target() {
    let mut target = false;
    super::apply_bool_field("true", &mut target, "test_key");
    assert!(target);
}

#[test]
fn parse_bool_field_keeps_default_on_invalid() {
    let mut target = true;
    super::apply_bool_field("banana", &mut target, "test_key");
    assert!(target); // unchanged
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test config::tests::parse_field -- --nocapture`
Expected: FAIL (functions don't exist yet)

**Step 3: Implement the helpers**

Add right after `parse_bool_like` (line 134):

```rust
/// Parse a string value into type T, returning `default` and logging a warning on failure.
fn parse_field<T: std::str::FromStr>(value: &str, default: T, key: &str) -> T {
    value.parse().unwrap_or_else(|_| {
        tracing::warn!("Invalid {key} '{value}', using default");
        default
    })
}

/// Parse a boolean config field, updating `target` on success and logging a warning on failure.
fn apply_bool_field(value: &str, target: &mut bool, key: &str) {
    if let Some(parsed) = parse_bool_like(value) {
        *target = parsed;
    } else {
        tracing::warn!("Invalid {key} '{value}', using default {target}");
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test config::tests::parse_field -- --nocapture && cargo test config::tests::parse_bool -- --nocapture`
Expected: PASS

**Step 5: Commit**

```
feat: add parse_field and apply_bool_field config helpers
```

---

### Task 2.2: Replace numeric parse boilerplate

**Files:**
- Modify: `src/config.rs:367-481`

**Step 1: Replace all `parse().unwrap_or(...)` patterns**

Replace each numeric parsing block with `parse_field`. Examples:

```rust
// OLD (line 367-368)
if let Some(max_retry) = config_map.get("maxretrytimes") {
    config.max_retry_times = max_retry.parse().unwrap_or(3);
}

// NEW
if let Some(v) = config_map.get("maxretrytimes") {
    config.max_retry_times = parse_field(v, config.max_retry_times, "maxretrytimes");
}
```

Apply to ALL numeric fields listed below (use `config.field_name` as default in ALL cases, never a hardcoded literal — this fixes the default-drift bug):

| Config key | Field |
|---|---|
| `maxretrytimes` | `max_retry_times` |
| `downloadtimeout` | `download_timeout` |
| `download.memory_threshold` | `memory_threshold_mb` |
| `download.memory_buffer` | `memory_buffer_mb` |
| `download.memory_max_file_mb` | `memory_max_file_mb` |
| `download.max_concurrent` | `max_concurrent_downloads` |
| `download.pool_max_idle_per_host` | `download_pool_max_idle_per_host` |
| `download.connect_timeout_secs` | `download_connect_timeout_secs` |
| `download.chunk_size_kb` | `download_chunk_size_kb` |
| `upload.client_reuse_requests` | `upload_client_reuse_requests` |
| `upload.timeout_secs` | `upload_timeout_secs` |
| `upload.pool_max_idle_per_host` | `upload_pool_max_idle_per_host` |
| `upload.pool_idle_timeout_secs` | `upload_pool_idle_timeout_secs` |
| `maintenance.memory_release_interval_requests` | `memory_release_interval_requests` |
| `maintenance.db_analyze_interval_requests` | `db_analyze_interval_requests` |

Also replace the `upload.max_concurrent` match block (lines 428-437):

```rust
// OLD
if let Some(max_concurrent) = config_map.get("upload.max_concurrent") {
    match max_concurrent.parse() {
        Ok(parsed) => config.upload_max_concurrent = parsed,
        Err(e) => tracing::warn!(...),
    }
}

// NEW
if let Some(v) = config_map.get("upload.max_concurrent") {
    config.upload_max_concurrent = parse_field(v, config.upload_max_concurrent, "upload.max_concurrent");
}
```

**Step 2: Verify tests pass**

Run: `cargo test config::tests::`
Expected: all existing + new tests pass

**Step 3: Commit**

```
refactor: use parse_field helper for numeric config values
```

---

### Task 2.3: Replace bool parse boilerplate

**Files:**
- Modify: `src/config.rs:323-385,460-470`

**Step 1: Replace all bool parsing blocks**

Replace each 8-line bool block with `apply_bool_field`. Example:

```rust
// OLD (lines 323-333)
if let Some(bot_debug_value) = config_map.get("botdebug") {
    if let Some(parsed) = parse_bool_like(bot_debug_value) {
        config.bot_debug = parsed;
    } else {
        tracing::warn!(
            "Invalid botdebug '{}', using default {}",
            bot_debug_value,
            config.bot_debug
        );
    }
}

// NEW
if let Some(v) = config_map.get("botdebug") {
    apply_bool_field(v, &mut config.bot_debug, "botdebug");
}
```

Apply to all 5 bool fields:

| Config key | Field |
|---|---|
| `botdebug` | `bot_debug` |
| `autoupdate` | `auto_update` |
| `autoretry` | `auto_retry` |
| `checkmd5` | `check_md5` |
| `upload.local_file_uri` | `upload_local_file_uri` |

**Step 2: Verify tests pass**

Run: `cargo test config::tests::`
Expected: all pass

**Step 3: Run full validation**

Run: `cargo fmt -- --check && cargo clippy -- -D warnings && cargo test`
Expected: all pass

**Step 4: Commit**

```
refactor: use apply_bool_field helper for boolean config values
```

---

## Phase 4: DB Query Optimization & Memory Stats Throttle (subagent C)

**Summary:** Two independent optimizations: (1) replace `SELECT *` with explicit column list in the hot-path DB query, and (2) add a throttle to `get_available_memory_mb()` so it doesn't call `refresh_memory()` on every invocation.

### Task 4.1: Replace SELECT * with explicit columns

**Files:**
- Modify: `src/database.rs:102-138`

**Step 1: Write a focused test**

The existing test infrastructure uses a real temp SQLite DB. Add to `src/database.rs` test module:

```rust
#[tokio::test]
async fn get_song_returns_all_mapped_fields() {
    let db = Database::new("sqlite::memory:").await.unwrap();
    let now = chrono::Utc::now();
    let song = SongInfo {
        id: 0,
        music_id: 12345,
        song_name: "Test".to_string(),
        song_artists: "Artist".to_string(),
        song_album: "Album".to_string(),
        file_ext: "mp3".to_string(),
        music_size: 5_000_000,
        pic_size: 0,
        emb_pic_size: 0,
        bit_rate: 320_000,
        duration: 180,
        file_id: Some("file_abc".to_string()),
        thumb_file_id: Some("thumb_abc".to_string()),
        from_user_id: 100,
        from_user_name: "user".to_string(),
        from_chat_id: 200,
        from_chat_name: "chat".to_string(),
        created_at: now,
        updated_at: now,
    };
    db.save_song_info(&song).await.unwrap();
    let fetched = db.get_song_by_music_id(12345).await.unwrap().unwrap();
    assert_eq!(fetched.music_id, 12345);
    assert_eq!(fetched.song_name, "Test");
    assert_eq!(fetched.file_id, Some("file_abc".to_string()));
    assert_eq!(fetched.bit_rate, 320_000);
    assert_eq!(fetched.duration, 180);
}
```

**Step 2: Run test to make sure it passes with current code**

Run: `cargo test database::tests::get_song_returns_all_mapped_fields -- --exact --nocapture`
Expected: PASS (baseline)

**Step 3: Replace `SELECT *` with explicit column list**

In `get_song_by_music_id` (line 104), replace:

```rust
let row = sqlx::query("SELECT * FROM song_infos WHERE music_id = ? LIMIT 1")
```

With:

```rust
let row = sqlx::query(
    "SELECT id, music_id, song_name, song_artists, song_album, file_ext, \
     music_size, pic_size, emb_pic_size, bit_rate, duration, file_id, \
     thumb_file_id, from_user_id, from_user_name, from_chat_id, \
     from_chat_name, created_at, updated_at \
     FROM song_infos WHERE music_id = ? LIMIT 1"
)
```

This is a behavior-preserving refactor — all columns are still fetched but now explicitly listed. This makes the query self-documenting and prevents future schema additions from silently expanding the result set.

**Step 4: Run test to verify it still passes**

Run: `cargo test database::tests::get_song_returns_all_mapped_fields -- --exact --nocapture`
Expected: PASS

**Step 5: Commit**

```
refactor: replace SELECT * with explicit column list in song query
```

---

### Task 4.2: Throttle memory stats refresh

**Files:**
- Modify: `src/audio_buffer.rs:10,19-24,205-215`

**Step 1: Write the failing test**

Add to `src/audio_buffer.rs` test module:

```rust
#[test]
fn get_available_memory_mb_returns_nonzero() {
    let mb = AudioBuffer::get_available_memory_mb();
    assert!(mb > 0, "Available memory should be > 0 MB, got {mb}");
}

#[test]
fn get_available_memory_mb_is_consistent() {
    let mb1 = AudioBuffer::get_available_memory_mb();
    let mb2 = AudioBuffer::get_available_memory_mb();
    // Within the same second, throttled calls should return similar values
    // (exact same if within throttle window)
    let diff = if mb1 > mb2 { mb1 - mb2 } else { mb2 - mb1 };
    assert!(
        diff < 1024,
        "Two rapid calls should return similar values: {mb1} vs {mb2}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test audio_buffer::tests::get_available_memory_mb -- --nocapture`
Expected: FAIL (method is currently private and not exposed for tests, or test doesn't exist yet)

**Step 3: Implement throttled memory refresh**

Replace the `SYSTEM` static and `get_available_memory_mb` with a throttled version:

```rust
use std::sync::{LazyLock, Mutex};
use std::time::Instant as StdInstant;

/// Cached System instance with last-refresh timestamp for throttling.
static SYSTEM: LazyLock<Mutex<(System, StdInstant)>> = LazyLock::new(|| {
    let mut sys = System::new();
    sys.refresh_memory();
    Mutex::new((sys, StdInstant::now()))
});

/// Minimum interval between memory refreshes (500ms)
const MEMORY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
```

Then update `get_available_memory_mb`:

```rust
/// Get available system memory in MB (throttled to avoid frequent syscalls)
fn get_available_memory_mb() -> u64 {
    if let Ok(mut guard) = SYSTEM.lock() {
        let (sys, last_refresh) = &mut *guard;
        if last_refresh.elapsed() >= MEMORY_REFRESH_INTERVAL {
            sys.refresh_memory();
            *last_refresh = StdInstant::now();
        }
        sys.available_memory() / (1024 * 1024)
    } else {
        tracing::warn!("Failed to lock SYSTEM mutex, using conservative memory estimate");
        512
    }
}
```

Note: `std::time::Instant` is already imported as `Instant` from `std::time` at the top of the file — check for conflicts. If `Instant` is already used elsewhere, use `StdInstant` alias or just use `std::time::Instant` inline. In practice, `audio_buffer.rs` does not import `std::time::Instant` (it uses `tokio::time` for async), so adding a `use std::time::Instant as StdInstant;` import is clean.

**Step 4: Run test to verify it passes**

Run: `cargo test audio_buffer::tests::get_available_memory_mb -- --nocapture`
Expected: PASS

**Step 5: Run full validation**

Run: `cargo fmt -- --check && cargo clippy -- -D warnings && cargo test`
Expected: all pass

**Step 6: Commit**

```
perf: throttle sysinfo memory refresh to 500ms intervals
```

---

## Subagent Dispatch Plan

All three phases are **fully independent** — they touch different files with no overlap:

| Subagent | Phase | Files Modified | Depends On |
|----------|-------|---------------|------------|
| A | Phase 1 (Tasks 1.1-1.4) | `Cargo.toml`, `src/music_api.rs` | None |
| B | Phase 2 (Tasks 2.1-2.3) | `src/config.rs` | None |
| C | Phase 4 (Tasks 4.1-4.2) | `src/database.rs`, `src/audio_buffer.rs` | None |

**Exception:** Task 1.1 (adding `dashmap` to `Cargo.toml`) must complete before Tasks 1.2-1.4 can compile. Within subagent A, tasks are sequential.

**Final integration step** (after all subagents complete):

```bash
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
```

All 94+ tests must pass. Then squash or keep commits as-is per preference.
