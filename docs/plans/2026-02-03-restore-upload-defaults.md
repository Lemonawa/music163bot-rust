# Restore Upload Defaults Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Restore upload client default behavior to match v1.1.13 while keeping new configuration options available.

**Architecture:** Keep upload pool config fields in Config, but default to v1.1.13 behavior by not setting idle timeout unless explicitly configured. Keep pool_max_idle_per_host default at 0 and ensure conditional application for idle timeout in upload client builder.

**Tech Stack:** Rust, reqwest, teloxide

---

### Task 1: Add failing test for idle timeout default behavior

**Files:**
- Modify: `src/bot.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn upload_pool_idle_timeout_disabled_when_zero() {
    assert!(!super::should_set_upload_pool_idle_timeout(0));
    assert!(super::should_set_upload_pool_idle_timeout(60));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test`
Expected: FAIL with missing function `should_set_upload_pool_idle_timeout`

### Task 2: Implement conditional idle timeout application

**Files:**
- Modify: `src/bot.rs`

**Step 1: Write minimal implementation**

```rust
fn should_set_upload_pool_idle_timeout(secs: u64) -> bool {
    secs > 0
}
```

**Step 2: Apply condition in upload client builder**

```rust
let mut client_builder = reqwest::Client::builder()
    .use_rustls_tls()
    .timeout(std::time::Duration::from_secs(state.config.upload_timeout_secs))
    .pool_max_idle_per_host(state.config.upload_pool_max_idle_per_host)
    .no_gzip()
    .user_agent("Go-http-client/2.0")
    .default_headers(reqwest::header::HeaderMap::new());

if should_set_upload_pool_idle_timeout(state.config.upload_pool_idle_timeout_secs) {
    client_builder = client_builder.pool_idle_timeout(std::time::Duration::from_secs(
        state.config.upload_pool_idle_timeout_secs,
    ));
}

let client = build_reqwest_client(client_builder)?;
```

**Step 3: Run test to verify it passes**

Run: `cargo test`
Expected: PASS

### Task 3: Restore config defaults and parsing behavior

**Files:**
- Modify: `src/config.rs`

**Step 1: Change default value**

```rust
upload_pool_idle_timeout_secs: 0,
```

**Step 2: Preserve defaults on parse errors**

```rust
if let Some(timeout) = config_map.get("upload.pool_idle_timeout_secs") {
    config.upload_pool_idle_timeout_secs =
        timeout.parse().unwrap_or(config.upload_pool_idle_timeout_secs);
}
```

### Task 4: Update config example

**Files:**
- Modify: `config.ini.example`

**Step 1: Add upload settings section**

```ini
[upload]
client_reuse_requests = 50
pool_max_idle_per_host = 0
pool_idle_timeout_secs = 0
timeout_secs = 300
```

### Task 5: Verify and commit

**Step 1: Run tests**

Run: `cargo test`
Expected: PASS

**Step 2: Commit**

```bash
git add src/bot.rs src/config.rs config.ini.example
git commit -m "fix: restore upload client defaults"
```
