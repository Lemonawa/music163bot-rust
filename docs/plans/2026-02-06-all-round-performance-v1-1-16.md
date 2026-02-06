# All-Round Performance Optimization v1.1.16 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve end-to-end latency, throughput, memory behavior, and startup/build efficiency across the whole bot without changing user-visible behavior.

**Architecture:** Apply low-risk optimizations in hot paths first (message handling, DB status query, HTTP request policy), then reduce I/O/memory overhead (cover pipeline, cleanup syscalls, allocator pressure), and finally trim startup/build/runtime footprint (config parsing micro-optimizations and dependency feature tuning). Keep behavior stable and verify with focused unit tests plus full suite checks.

**Tech Stack:** Rust, tokio, reqwest, teloxide, sqlx/sqlite, tracing.

---

### Task 1: Bound message processing concurrency

**Files:**
- Modify: `src/bot.rs`
- Test: `src/bot.rs`

**Step 1: Write the failing test**

Add tests for a helper that determines whether to spawn work and limits in-flight tasks.

**Step 2: Run test to verify it fails**

Run: `cargo test spawn_gate -- --nocapture`

Expected: FAIL with missing helper / wrong behavior.

**Step 3: Write minimal implementation**

- Add `message_task_semaphore` to `BotState`.
- Add helper to classify text messages that need processing.
- In `handle_message`, acquire permit before spawning and drop permit at task end.

**Step 4: Run test to verify it passes**

Run: `cargo test spawn_gate -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/bot.rs
git commit -m "perf: bound message task concurrency"
```

---

### Task 2: Optimize status query and indexing

**Files:**
- Modify: `src/database.rs`
- Modify: `src/bot.rs`
- Test: `src/database.rs`

**Step 1: Write the failing test**

Add tests that verify a combined status count query returns `(total, user, chat)` correctly.

**Step 2: Run test to verify it fails**

Run: `cargo test status_counts -- --nocapture`

Expected: FAIL due to missing API.

**Step 3: Write minimal implementation**

- Add indexes on `from_user_id` and `from_chat_id` during DB init.
- Add `count_status_stats(user_id, chat_id)` returning all counts in one query.
- Update `/status` handler to call one DB API instead of three queries.

**Step 4: Run test to verify it passes**

Run: `cargo test status_counts -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/database.rs src/bot.rs
git commit -m "perf: reduce status query roundtrips and add indexes"
```

---

### Task 3: Move maintenance operations off request hot path

**Files:**
- Modify: `src/bot.rs`
- Modify: `src/database.rs`
- Test: `src/bot.rs`

**Step 1: Write the failing test**

Add tests for maintenance scheduling helper(s) to ensure request path only enqueues work.

**Step 2: Run test to verify it fails**

Run: `cargo test maintenance_scheduler -- --nocapture`

Expected: FAIL due to missing helper/background scheduling.

**Step 3: Write minimal implementation**

- Add background maintenance worker task in `run`.
- Replace in-request `database.analyze()` and `force_memory_release()` with non-blocking notifications.
- Replace full `ANALYZE` with `PRAGMA optimize` in periodic path.

**Step 4: Run test to verify it passes**

Run: `cargo test maintenance_scheduler -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/bot.rs src/database.rs
git commit -m "perf: offload maintenance work to background task"
```

---

### Task 4: Harden HTTP request policy and fast share-link resolve

**Files:**
- Modify: `src/music_api.rs`
- Modify: `src/config.rs`
- Modify: `src/bot.rs`
- Test: `src/music_api.rs`

**Step 1: Write the failing test**

Add tests for host rewrite helper and lightweight share-link resolve helper behavior.

**Step 2: Run test to verify it fails**

Run: `cargo test request_policy -- --nocapture`

Expected: FAIL due to missing helpers.

**Step 3: Write minimal implementation**

- Add global request timeout from config to `MusicApi` client builder.
- Add `error_for_status` checks before JSON parsing on API endpoints.
- Add lightweight redirect-resolving method used by `handle_music_url`.
- Keep behavior-compatible fallback paths.

**Step 4: Run test to verify it passes**

Run: `cargo test request_policy -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/music_api.rs src/config.rs src/bot.rs
git commit -m "perf: improve HTTP timeout/status handling and share link resolve"
```

---

### Task 5: Eliminate duplicate cover downloads in both mode

**Files:**
- Modify: `src/music_api.rs`
- Modify: `src/bot.rs`
- Test: `src/music_api.rs`

**Step 1: Write the failing test**

Add tests for thumbnail transform helper using in-memory image bytes.

**Step 2: Run test to verify it fails**

Run: `cargo test thumbnail_transform -- --nocapture`

Expected: FAIL with missing helper.

**Step 3: Write minimal implementation**

- Add helper to convert original image bytes into 320x320 JPEG thumbnail.
- In `CoverMode::Both`, download original once and derive thumbnail locally.

**Step 4: Run test to verify it passes**

Run: `cargo test thumbnail_transform -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/music_api.rs src/bot.rs
git commit -m "perf: reuse single cover download for embedding and thumbnail"
```

---

### Task 6: Reduce filesystem and memory overhead in buffers/utilities

**Files:**
- Modify: `src/audio_buffer.rs`
- Modify: `src/utils.rs`
- Test: `src/utils.rs`

**Step 1: Write the failing test**

Add tests for direct numeric parse fast path and directory creation idempotence.

**Step 2: Run test to verify it fails**

Run: `cargo test parse_music_id -- --nocapture`

Expected: FAIL due to current behavior/perf helpers missing.

**Step 3: Write minimal implementation**

- Remove eager `shrink_to_fit()` on hot metadata paths.
- Remove `exists()` pre-check before file delete in cleanup paths.
- Use single parse for numeric `music_id` fast path.
- Use unconditional `create_dir_all` in `ensure_dir`.

**Step 4: Run test to verify it passes**

Run: `cargo test parse_music_id -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/audio_buffer.rs src/utils.rs
git commit -m "perf: trim buffer and fs overhead in hot paths"
```

---

### Task 7: Startup/build/runtime footprint improvements

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/config.rs`
- Test: `src/config.rs`

**Step 1: Write the failing test**

Add tests for config boolean parsing and ensure defaults remain unchanged.

**Step 2: Run test to verify it fails**

Run: `cargo test config_bool_parsing -- --nocapture`

Expected: FAIL due to helper absence.

**Step 3: Write minimal implementation**

- Use ASCII case-insensitive checks to avoid `to_lowercase()` allocations for booleans.
- Remove unused direct dependency `config`.
- Reduce `tokio` features from `full` to required set.

**Step 4: Run test to verify it passes**

Run: `cargo test config_bool_parsing -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add Cargo.toml src/config.rs
git commit -m "perf: reduce startup allocations and dependency footprint"
```

---

### Task 8: Version bump and full verification

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Step 1:** Bump version to `1.1.16`.

**Step 2:** Run `cargo fmt`.

**Step 3:** Run `cargo check`.

**Step 4:** Run `cargo clippy -- -D warnings`.

**Step 5:** Run `cargo test`.

**Step 6:** Summarize metrics and changes.
