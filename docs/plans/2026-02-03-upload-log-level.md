# Upload Log Level Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a dedicated `upload.log_level` config to control upload diagnostic logging without changing global logging.

**Architecture:** Introduce a small `UploadLogLevel` enum in `src/config.rs` with parsing and threshold logic, then gate new upload diagnostics in `src/bot.rs` via a helper that checks the configured level.

**Tech Stack:** Rust, `tracing`, existing config parser in `src/config.rs`.

### Task 1: Add UploadLogLevel Parsing + Defaults

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn upload_log_level_defaults_to_info() {
    let config = Config::default();
    assert_eq!(config.upload_log_level, UploadLogLevel::Info);
}

#[test]
fn upload_log_level_parses_values() {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_upload_log_{temp_name}.ini"));
    let content = "bot.token=token\nupload.log_level=warn\n";

    std::fs::write(&temp_path, content).expect("write temp config");
    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");
    let _ = std::fs::remove_file(&temp_path);

    assert_eq!(loaded.upload_log_level, UploadLogLevel::Warning);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test config::tests::upload_log_level_defaults_to_info`
Expected: FAIL (missing field/enum)

**Step 3: Write minimal implementation**

- Add `UploadLogLevel` enum with `FromStr` and `Display`.
- Add `upload_log_level` field to `Config` with default `Info`.
- Parse `upload.log_level`, accept `NONE/ERROR/WARNING/WARN/INFO/DEBUG` (case-insensitive), warn and keep default on invalid.

**Step 4: Run test to verify it passes**

Run: `cargo test config::tests::upload_log_level_defaults_to_info`
Expected: PASS

**Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add upload log level config"
```

### Task 2: Gate Upload Diagnostic Logs

**Files:**
- Modify: `src/bot.rs`
- Test: `src/bot.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn upload_log_level_allows_thresholds() {
    assert!(UploadLogLevel::Info.allows(UploadLogLevel::Error));
    assert!(UploadLogLevel::Info.allows(UploadLogLevel::Warning));
    assert!(UploadLogLevel::Info.allows(UploadLogLevel::Info));
    assert!(!UploadLogLevel::Info.allows(UploadLogLevel::Debug));
    assert!(!UploadLogLevel::None.allows(UploadLogLevel::Error));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test bot::tests::upload_log_level_allows_thresholds`
Expected: FAIL (missing method)

**Step 3: Write minimal implementation**

- Add `allows` method on `UploadLogLevel`.
- Add helper in `src/bot.rs` to emit upload diagnostics only when allowed.
- Add diagnostics around upload client creation/reuse (no secrets).

**Step 4: Run test to verify it passes**

Run: `cargo test bot::tests::upload_log_level_allows_thresholds`
Expected: PASS

**Step 5: Commit**

```bash
git add src/bot.rs src/config.rs
git commit -m "feat: gate upload diagnostics by log level"
```

### Task 3: Document Config + A/B Suggestion

**Files:**
- Modify: `config.ini.example`

**Step 1: Update config example**

- Add `upload.log_level` with allowed values + note about global `loglevel` still applies.
- Add comment for optional A/B test: `client_reuse_requests=10`, `pool_max_idle_per_host=1`.

**Step 2: Commit**

```bash
git add config.ini.example
git commit -m "docs: document upload log level"
```

### Task 4: Bump Version to 1.1.15

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock` (if updated by cargo)

**Step 1: Bump version**

- Update `Cargo.toml` to `1.1.15`.
- If `Cargo.lock` updates, include it.

**Step 2: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 1.1.15"
```

