# Parallel Performance Optimization Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Reduce end-to-end request latency and CPU/memory overhead by applying all identified safe performance optimizations across bot, API, buffering, DB, and utility layers.

**Architecture:** Execute independent optimizations as parallel workstreams (one subagent per file domain) to maximize throughput and minimize merge conflicts. Each workstream is test-first and commit-by-commit. Integrate only after all streams pass local validation and baseline-vs-new perf checks.

**Tech Stack:** Rust 2024, tokio, reqwest, teloxide, sqlx/sqlite, dashmap, bytes, sysinfo

---

## Preflight (Single Agent, Sequential)

### Task 0.1: Capture baseline before any optimization

**Files:**
- Modify: `docs/perf/2026-02-11-parallel-optimization-baseline.md`
- Reference: `scripts/perf_compare.py`

**Step 1: Create baseline note file**

Create `docs/perf/2026-02-11-parallel-optimization-baseline.md` with placeholders for:
- upload latency (`[pre_upload_path]`, `[upload_audio]`)
- lyric command latency
- memory mode vs disk mode throughput

**Step 2: Run baseline checks**

Run:
`cargo check && cargo test`

Expected: PASS.

**Step 3: Record baseline command outputs**

Run:
`python scripts/perf_compare.py`

Expected: baseline comparison output generated (or script-specific warning if input data missing, still documented).

**Step 4: Commit baseline doc**

```bash
git add docs/perf/2026-02-11-parallel-optimization-baseline.md
git commit -m "docs: add performance optimization baseline record"
```

---

## Workstream A (Subagent A): `src/bot.rs` Hot Path

### Task A.1: Parallelize lyric+detail fetch in lyric command

**Files:**
- Modify: `src/bot.rs:2676`
- Test: `src/bot.rs` (`#[cfg(test)]` module)

**Step 1: Write a focused regression test for lyric flow helper**

Add a small helper-level test (new helper function) that verifies both futures are awaited together and errors are propagated correctly.

**Step 2: Run test to verify failure (helper not implemented)**

Run:
`cargo test bot::tests::lyric_parallel_fetch -- --nocapture`

Expected: FAIL.

**Step 3: Implement parallel fetch helper and use it**

In `handle_lyric_command`, replace sequential:

```rust
let lyric = state.music_api.get_song_lyric(music_id).await?;
let song_detail = state.music_api.get_song_detail(music_id).await?;
```

with parallel join:

```rust
let (lyric_result, detail_result) = tokio::join!(
    state.music_api.get_song_lyric(music_id),
    state.music_api.get_song_detail(music_id)
);
```

Then keep existing error messages/behavior.

**Step 4: Run target tests**

Run:
`cargo test bot::tests::lyric_parallel_fetch -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/bot.rs
git commit -m "perf: parallelize lyric and song detail fetching"
```

---

### Task A.2: Parallelize lyric upload client and permit acquisition

**Files:**
- Modify: `src/bot.rs:2765`
- Test: `src/bot.rs` (`#[cfg(test)]` module)

**Step 1: Add failing helper test for upload resource acquisition**

Write a test for a new helper that returns `(client_bundle, permit)` and validates both branches propagate errors correctly.

**Step 2: Run test to verify failure**

Run:
`cargo test bot::tests::lyric_upload_resource_parallel -- --nocapture`

Expected: FAIL.

**Step 3: Implement parallel acquisition in lyric path**

Replace sequential:

```rust
let bundle = acquire_upload_client(state).await?;
let permit = acquire_upload_permit(&state.upload_semaphore).await?;
```

with:

```rust
let (bundle_result, permit_result) = tokio::join!(
    acquire_upload_client(state),
    acquire_upload_permit(&state.upload_semaphore)
);
```

Then preserve existing cleanup/error text.

**Step 4: Run target tests**

Run:
`cargo test bot::tests::lyric_upload_resource_parallel -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/bot.rs
git commit -m "perf: parallelize lyric upload client and permit acquisition"
```

---

### Task A.3: Bot micro-allocations cleanup

**Files:**
- Modify: `src/bot.rs:670,758,810,1029,2751`
- Test: existing bot tests

**Step 1: Write/extend tests for behavior stability**

Add tests for helper-level behavior where needed (e.g. invalid file-id detection function).

**Step 2: Apply low-risk micro-optimizations**

- replace `format!("{e}")` with `e.to_string()` and reuse the string once
- compute artists string once and pass into `download_and_send_music`
- in lyric flow use `lyric.len() as u64` instead of `metadata()` after write

**Step 3: Run focused tests**

Run:
`cargo test bot::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/bot.rs
git commit -m "perf: reduce bot hot-path string allocations"
```

---

## Workstream B (Subagent B): `src/music_api.rs` Request/Parsing Path

### Task B.1: Cache EAPI cookie identity parts

**Files:**
- Modify: `src/music_api.rs:23-31,177-230,285-307`
- Test: `src/music_api.rs` tests

**Step 1: Add failing tests for cookie stability semantics**

Add tests that verify:
- per-`MusicApi` device id is stable across repeated `build_eapi_cookie()` calls
- `MUSIC_U` branch remains correct

**Step 2: Run test to verify failure**

Run:
`cargo test music_api::tests::eapi_cookie -- --nocapture`

Expected: FAIL.

**Step 3: Implement cached fields**

Add fields to `MusicApi`:

```rust
eapi_device_id: String,
music_cookie: String,
```

Initialize once in constructor and reuse in `build_eapi_cookie()`.

**Step 4: Run target tests**

Run:
`cargo test music_api::tests::eapi_cookie -- --nocapture`

Expected: PASS.

**Step 5: Commit**

```bash
git add src/music_api.rs
git commit -m "perf: cache eapi cookie identity fields"
```

---

### Task B.2: Reuse static headers and cookie helper

**Files:**
- Modify: `src/music_api.rs:656-723`
- Test: `src/music_api.rs` tests

**Step 1: Add/extend tests for request rewrite/header helper behavior**

Keep host rewrite tests and add helper unit tests for optional cookie injection.

**Step 2: Implement shared header builders**

Add helper methods/static header maps for:
- media audio download headers
- image download headers

and a small cookie helper to avoid repeated `format!("MUSIC_U={music_u}")`.

**Step 3: Run tests**

Run:
`cargo test music_api::tests::request_policy -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/music_api.rs
git commit -m "perf: reuse request headers and cookie helpers in music api"
```

---

### Task B.3: Reduce search/rewrite allocations

**Files:**
- Modify: `src/music_api.rs:617-643,782-787,1039`
- Test: `src/music_api.rs` tests

**Step 1: Add failing tests for URL rewrite + artist formatting**

Existing rewrite tests already cover behavior; add edge case tests and a test for `format_artists` output parity.

**Step 2: Implement optimization changes**

- parse `search_songs` response from bytes where possible (avoid extra text allocation)
- replace chained `.replace()` in `rewrite_media_url()` with single-pass mapping
- replace manual `format_artists` loop with join-based implementation (same output)

**Step 3: Run tests**

Run:
`cargo test music_api::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/music_api.rs
git commit -m "perf: reduce search and url rewrite allocations"
```

---

### Task B.4 (Optional, high-risk): Parallel fallback bitrate probing

**Files:**
- Modify: `src/music_api.rs:511-580`
- Test: `src/music_api.rs` tests

**Step 1: Add deterministic unit tests for fallback ordering/selection**

**Step 2: Implement bounded parallel probing (max 2)**

Use controlled parallelism only for fallback candidates, preserve current logging and error semantics.

**Step 3: Run tests**

Run:
`cargo test music_api::tests::fallback -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/music_api.rs
git commit -m "perf: probe fallback bitrates with bounded parallelism"
```

---

## Workstream C (Subagent C): `src/audio_buffer.rs` Memory and Copy Path

### Task C.1: Remove unused memory capacity field

**Files:**
- Modify: `src/audio_buffer.rs:42-46` and tests at `src/audio_buffer.rs:930+`

**Step 1: Write failing compile-level test update**

Update one test constructor to reflect intended `Memory` shape (without `capacity`) and run tests to fail on remaining call sites.

**Step 2: Remove field and update all constructors/tests**

Replace:

```rust
Memory { data, filename, capacity }
```

with:

```rust
Memory { data, filename }
```

**Step 3: Run target tests**

Run:
`cargo test audio_buffer::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/audio_buffer.rs
git commit -m "perf: remove unused memory buffer capacity field"
```

---

### Task C.2: Optimize MP3/FLAC in-memory rebuild allocation

**Files:**
- Modify: `src/audio_buffer.rs:326-350,447-477`
- Test: existing tagging tests in `src/audio_buffer.rs`

**Step 1: Add failing test for allocation-safe behavior parity**

Reuse existing byte-equivalence tests and add one case that ensures resulting payload is unchanged for same input.

**Step 2: Implement preallocation/in-place strategy**

- for MP3 prepend path, use `reserve` + `copy_within` where possible
- for FLAC rebuild, preallocate `new_data` with expected final capacity before write/append

**Step 3: Run tests**

Run:
`cargo test audio_buffer::tests::mp3_tagging_is_byte_identical_for_same_input -- --exact --nocapture`

Run:
`cargo test audio_buffer::tests::flac_tagging_keeps_equivalent_metadata_and_audio_payload -- --exact --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/audio_buffer.rs
git commit -m "perf: reduce reallocations in in-memory tag rebuild"
```

---

### Task C.3: Track disk bytes written to avoid metadata stat on size()

**Files:**
- Modify: `src/audio_buffer.rs:36-40,225-264`
- Test: `src/audio_buffer.rs` tests

**Step 1: Add failing unit test**

Add a test verifying `size()` returns expected value after sequential writes without requiring filesystem metadata.

**Step 2: Implement byte counter in `Disk` variant**

Add `written_bytes: u64`, increment during writes/stream copy paths, return cached size when available.

**Step 3: Run tests**

Run:
`cargo test audio_buffer::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/audio_buffer.rs
git commit -m "perf: cache disk buffer size during writes"
```

---

## Workstream D (Subagent D): `src/database.rs` Query/Pool Path

### Task D.1: Configure sqlite pool options explicitly

**Files:**
- Modify: `src/database.rs:46-57`
- Test: `src/database.rs` tests

**Step 1: Add a focused initialization test**

Add test to ensure DB init still succeeds with explicit pool configuration.

**Step 2: Implement pool options**

Use `SqlitePoolOptions` with explicit `max_connections`/`min_connections` suitable for SQLite workload.

**Step 3: Run tests**

Run:
`cargo test database::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/database.rs
git commit -m "perf: configure sqlite pool bounds explicitly"
```

---

### Task D.2: Reduce timestamp decode overhead and add helpful index

**Files:**
- Modify: `src/database.rs:103-139,87-97,309-314`
- Test: `src/database.rs` tests

**Step 1: Add/extend mapping tests for timestamp fields**

Ensure `created_at`/`updated_at` continue to parse as expected.

**Step 2: Implement optimization changes**

- switch to direct `chrono` decode if supported by current SQLx row mapping path
- keep fallback parser only if required
- add index for frequent status dimensions (evaluate `(from_user_id, from_chat_id)`)

**Step 3: Run tests**

Run:
`cargo test database::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/database.rs
git commit -m "perf: optimize sqlite timestamp decode and status indexes"
```

---

## Workstream E (Subagent E): `src/config.rs` + `src/utils.rs` Micro Allocations

### Task E.1: Remove enum parser lowercase allocations

**Files:**
- Modify: `src/config.rs:34-58,80-93`
- Test: `src/config.rs` tests

**Step 1: Add parser case-insensitive tests**

Add tests for mixed-case values (`"HyBrId"`, `"WaRn"`, `"OrIgInAl"`).

**Step 2: Replace `to_lowercase()` matching**

Use `eq_ignore_ascii_case` branches in `FromStr` impls.

**Step 3: Run tests**

Run:
`cargo test config::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/config.rs
git commit -m "perf: avoid lowercase allocations in config enum parsers"
```

---

### Task E.2: Optimize utility allocation hot spots

**Files:**
- Modify: `src/utils.rs:21-51,82-86,97-112,185-188`
- Test: `src/utils.rs` tests

**Step 1: Add failing tests for behavioral parity**

Add tests for:
- `extract_first_url` return behavior
- `clean_filename` edge cases
- timeout detection behavior

**Step 2: Implement micro-optimizations**

- reduce repeated parsing in canonical id extraction
- optimize `clean_filename` to one-pass preallocated string build
- replace expensive timeout detection stringification path where possible

**Step 3: Run tests**

Run:
`cargo test utils::tests:: -- --nocapture`

Expected: PASS.

**Step 4: Commit**

```bash
git add src/utils.rs
git commit -m "perf: reduce allocations in utility parsing helpers"
```

---

## Workstream F (Subagent F, Optional): Build/Binary Tuning

### Task F.1: Trim dependency features with no runtime benefit

**Files:**
- Modify: `Cargo.toml:57,67`
- Test: full validation

**Step 1: Confirm required image formats from real traffic/logs**

If only jpeg/png needed in production, remove unused image features.

**Step 2: Remove jemalloc profiling/stats features if unused**

Change:

```toml
tikv-jemallocator = { version = "0.6", features = ["profiling", "stats"] }
```

to:

```toml
tikv-jemallocator = { version = "0.6" }
```

**Step 3: Validate**

Run:
`cargo check && cargo test`

Expected: PASS.

**Step 4: Commit**

```bash
git add Cargo.toml
git commit -m "perf: trim nonessential dependency features"
```

---

## Full Optimization Inventory (All Identified Points)

| ID | File | Optimization Point | Priority | Planned In |
|---|---|---|---|---|
| P01 | `src/bot.rs` | lyric lyric/detail parallel fetch | High | A.1 |
| P02 | `src/bot.rs` | lyric upload client+permit parallel acquire | High | A.2 |
| P03 | `src/bot.rs` | avoid `format!("{e}")` hot-path allocation | Medium | A.3 |
| P04 | `src/bot.rs` | reuse formatted artists string | Medium | A.3 |
| P05 | `src/bot.rs` | avoid lyric `metadata()` syscall after write | Medium | A.3 |
| P06 | `src/music_api.rs` | cache eapi device id/cookie core pieces | High | B.1 |
| P07 | `src/music_api.rs` | reuse static request headers | High | B.2 |
| P08 | `src/music_api.rs` | reduce repeated `MUSIC_U` string formatting | Medium | B.2 |
| P09 | `src/music_api.rs` | parse search response with fewer allocations | Medium | B.3 |
| P10 | `src/music_api.rs` | single-pass media host rewrite | Medium | B.3 |
| P11 | `src/music_api.rs` | optimize artist join construction | Low | B.3 |
| P12 | `src/music_api.rs` | bounded parallel fallback bitrate probe | Optional/High-risk | B.4 |
| P13 | `src/audio_buffer.rs` | remove unused `capacity` field | Medium | C.1 |
| P14 | `src/audio_buffer.rs` | reduce MP3 prepend reallocations | High | C.2 |
| P15 | `src/audio_buffer.rs` | preallocate FLAC rebuild buffer | Medium | C.2 |
| P16 | `src/audio_buffer.rs` | cache disk written size | Medium | C.3 |
| P17 | `src/database.rs` | explicit sqlite pool bounds | Medium | D.1 |
| P18 | `src/database.rs` | reduce timestamp decode overhead | Medium | D.2 |
| P19 | `src/database.rs` | add status-focused covering index | Low/Medium | D.2 |
| P20 | `src/config.rs` | remove `to_lowercase()` allocations in parsers | Medium | E.1 |
| P21 | `src/config.rs` | pre-size config HashMap where beneficial | Low | E.1 |
| P22 | `src/utils.rs` | optimize canonical ID extraction path | Low | E.2 |
| P23 | `src/utils.rs` | one-pass `clean_filename` | Medium | E.2 |
| P24 | `src/utils.rs` | reduce timeout-error string allocation | Low | E.2 |
| P25 | `Cargo.toml` | trim nonessential dependency features | Optional | F.1 |

---

## Parallel Subagent Dispatch Matrix

| Subagent | Scope | Files | Can Run In Parallel With |
|---|---|---|---|
| A | Bot hot path | `src/bot.rs` | B, C, D, E, F |
| B | Music API | `src/music_api.rs` | A, C, D, E, F |
| C | Audio buffer | `src/audio_buffer.rs` | A, B, D, E, F |
| D | Database | `src/database.rs` | A, B, C, E, F |
| E | Config + utils | `src/config.rs`, `src/utils.rs` | A, B, C, D, F |
| F | Cargo/deps (optional) | `Cargo.toml` | A, B, C, D, E |

No overlapping file edits in A/B/C/D. E only touches config/utils. F only touches Cargo.

---

## Suggested Subagent Prompts (Copy-Paste)

1. **Subagent A prompt**
   `Implement Tasks A.1-A.3 from docs/plans/2026-02-11-parallel-performance-optimization.md with TDD and atomic commits. Modify only src/bot.rs.`

2. **Subagent B prompt**
   `Implement Tasks B.1-B.3 (and B.4 only if safe) from docs/plans/2026-02-11-parallel-performance-optimization.md with TDD and atomic commits. Modify only src/music_api.rs.`

3. **Subagent C prompt**
   `Implement Tasks C.1-C.3 from docs/plans/2026-02-11-parallel-performance-optimization.md with TDD and atomic commits. Modify only src/audio_buffer.rs.`

4. **Subagent D prompt**
   `Implement Tasks D.1-D.2 from docs/plans/2026-02-11-parallel-performance-optimization.md with TDD and atomic commits. Modify only src/database.rs.`

5. **Subagent E prompt**
   `Implement Tasks E.1-E.2 from docs/plans/2026-02-11-parallel-performance-optimization.md with TDD and atomic commits. Modify only src/config.rs and src/utils.rs.`

6. **Subagent F prompt (optional)**
   `Implement Task F.1 from docs/plans/2026-02-11-parallel-performance-optimization.md and validate no functionality regressions. Modify only Cargo.toml.`

---

## Integration and Verification (Single Agent, Sequential)

### Task I.1: Merge and resolve conflicts

Run each stream merge/cherry-pick in this order:
1) D, 2) E, 3) B, 4) C, 5) A, 6) F(optional)

Reason: minimizes conflict risk in shared utility imports and config defaults.

### Task I.2: Full validation gate

Run:

```bash
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
```

Expected: all pass.

### Task I.3: Performance verification

Run the same baseline commands used in Task 0.1 and record delta in:
- `docs/perf/2026-02-11-parallel-optimization-final.md`

Expected outcome:
- improved or equal `[pre_upload_path]` and lyric command latency
- no regression in correctness tests

### Task I.4: Final commit

```bash
git add docs/perf/2026-02-11-parallel-optimization-final.md
git commit -m "perf: integrate parallel optimization workstreams with validation"
```

---

Plan complete and saved to `docs/plans/2026-02-11-parallel-performance-optimization.md`. Two execution options:

**1. Subagent-Driven (this session)** - dispatch one fresh subagent per workstream (A-F), review after each stream, then integrate.

**2. Parallel Session (separate)** - open a dedicated execution session and run the plan with explicit checkpoints.

Which approach?
