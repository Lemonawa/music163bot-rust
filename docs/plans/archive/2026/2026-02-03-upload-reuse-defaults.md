# Upload Reuse Defaults Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Update upload defaults and example config to recommended reuse settings, and include log level entries.

**Architecture:** Adjust `Config::default` for upload reuse parameters and update tests to match; then update `config.ini.example` to reflect the new defaults and add `loglevel` + `upload.log_level` entries.

**Tech Stack:** Rust, `tracing`, existing config parser in `src/config.rs`.

### Task 1: Update Upload Defaults + Tests

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn upload_defaults_use_reuse_settings() {
    let config = Config::default();
    assert_eq!(config.upload_client_reuse_requests, 10);
    assert_eq!(config.upload_pool_max_idle_per_host, 1);
    assert_eq!(config.upload_pool_idle_timeout_secs, 60);
    assert_eq!(config.upload_timeout_secs, 300);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test config::tests::upload_defaults_use_reuse_settings`
Expected: FAIL (defaults still old)

**Step 3: Write minimal implementation**

- Update `Config::default` to set:
  - `upload_client_reuse_requests = 10`
  - `upload_pool_max_idle_per_host = 1`
  - `upload_pool_idle_timeout_secs = 60`
  - `upload_timeout_secs = 300` (unchanged)
- Update any existing default-related tests (rename as needed).

**Step 4: Run test to verify it passes**

Run: `cargo test config::tests::upload_defaults_use_reuse_settings`
Expected: PASS

**Step 5: Commit**

```bash
git add src/config.rs
git commit -m "perf: tune upload defaults"
```

### Task 2: Update Example Config Defaults

**Files:**
- Modify: `config.ini.example`

**Step 1: Update config example**

- Add top-level `loglevel = info` with a short comment.
- Set `[upload]` defaults to recommended reuse values:
  - `client_reuse_requests = 10`
  - `pool_max_idle_per_host = 1`
  - `pool_idle_timeout_secs = 60`
  - `timeout_secs = 300`
- Keep `upload.log_level = INFO` with its explanation.

**Step 2: Commit**

```bash
git add config.ini.example
git commit -m "docs: refresh upload defaults example"
```

