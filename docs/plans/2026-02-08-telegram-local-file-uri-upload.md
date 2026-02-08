# Telegram Local File URI Upload Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add optional `file://` uploads for local telegram-bot-api with a config toggle defaulting to off.

**Architecture:** Add a config flag (`upload.local_file_uri`) and a small helper that builds `file://` URIs from disk paths only when enabled and not targeting the official API. Use that helper in raw upload paths (sendAudio and sendDocument), falling back to existing multipart uploads when a URI cannot be built.

**Tech Stack:** Rust 2024, teloxide, reqwest, tokio, serde.

---

### Task 0: Worktree + plan file

**Files:**
- Create: `docs/plans/2026-02-08-telegram-local-file-uri-upload.md`

**Step 1: Create isolated worktree**

Run: `git worktree add ../music163bot-rust-local-file-uri`
Expected: worktree created at the new path

**Step 2: Save this plan**

Create `docs/plans/2026-02-08-telegram-local-file-uri-upload.md` with the full plan contents.

**Step 3: Commit plan (optional)**

Run:
```
git add docs/plans/2026-02-08-telegram-local-file-uri-upload.md
git commit -m "docs: add local file uri upload plan"
```

---

### Task 1: Add config flag + tests

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`

**Step 1: Write failing tests**

Add to `#[cfg(test)] mod tests` in `src/config.rs`:
```rust
#[test]
fn upload_local_file_uri_defaults_false() {
    let config = Config::default();
    assert!(!config.upload_local_file_uri);
}

#[test]
fn upload_local_file_uri_parses_true() {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_local_uri_{temp_name}.ini"));
    let content = "bot.token=token\nupload.local_file_uri=true\n";
    std::fs::write(&temp_path, content).expect("write temp config");
    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");
    let _ = std::fs::remove_file(&temp_path);
    assert!(loaded.upload_local_file_uri);
}
```

**Step 2: Run tests to verify failure**

Run: `cargo test config::tests::upload_local_file_uri_defaults_false config::tests::upload_local_file_uri_parses_true -- --exact`
Expected: FAIL (field missing / not parsed)

**Step 3: Implement config flag**

In `src/config.rs`:
- Add `pub upload_local_file_uri: bool` to `Config` near other upload fields.
- In `Default`, set `upload_local_file_uri: false`.
- In `load`, parse `upload.local_file_uri` using `parse_bool_like`, warn on invalid.

**Step 4: Run tests to verify pass**

Run: `cargo test config::tests::upload_local_file_uri_defaults_false config::tests::upload_local_file_uri_parses_true -- --exact`
Expected: PASS

**Step 5: Commit (optional)**

```
git add src/config.rs
git commit -m "feat: add upload local file uri config toggle"
```

---

### Task 2: Add local file URI helper + tests

**Files:**
- Modify: `src/bot.rs`
- Test: `src/bot.rs`

**Step 1: Write failing tests**

Add to `#[cfg(test)] mod tests` in `src/bot.rs`:
```rust
#[test]
fn local_file_uri_disabled_by_default() {
    let config = Config::default();
    let path = std::path::Path::new("/tmp/test.mp3");
    assert!(super::maybe_local_file_uri(&config, "http://127.0.0.1:8081/botTOKEN/", path)
        .is_none());
}

#[test]
fn local_file_uri_skips_official_api() {
    let mut config = Config::default();
    config.upload_local_file_uri = true;
    let path = std::path::Path::new("/tmp/test.mp3");
    assert!(super::maybe_local_file_uri(&config, "https://api.telegram.org/botTOKEN/", path)
        .is_none());
}

#[test]
fn local_file_uri_builds_from_existing_path() {
    let mut config = Config::default();
    config.upload_local_file_uri = true;

    let temp_name = format!(
        "music163bot_local_uri_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let temp_path = std::env::temp_dir().join(temp_name);
    std::fs::write(&temp_path, b"123").expect("write temp file");

    let uri = super::maybe_local_file_uri(
        &config,
        "http://127.0.0.1:8081/botTOKEN/",
        &temp_path,
    )
    .expect("file uri");
    let _ = std::fs::remove_file(&temp_path);

    assert!(uri.starts_with("file://"));
}

#[test]
fn local_file_uri_returns_none_for_missing_path() {
    let mut config = Config::default();
    config.upload_local_file_uri = true;
    let missing = std::env::temp_dir().join("missing_music163bot_local_uri");
    assert!(super::maybe_local_file_uri(&config, "http://127.0.0.1:8081/botTOKEN/", &missing)
        .is_none());
}
```

**Step 2: Run tests to verify failure**

Run: `cargo test bot::tests::local_file_uri_disabled_by_default bot::tests::local_file_uri_skips_official_api bot::tests::local_file_uri_builds_from_existing_path bot::tests::local_file_uri_returns_none_for_missing_path -- --exact`
Expected: FAIL (helpers missing)

**Step 3: Implement helper functions**

Add near other helpers in `src/bot.rs`:
```rust
fn is_official_telegram_api(api_base_url: &str) -> bool {
    reqwest::Url::parse(api_base_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.eq_ignore_ascii_case("api.telegram.org")))
        .unwrap_or(false)
}

fn local_file_uri_from_path(path: &std::path::Path) -> Option<String> {
    let absolute = std::fs::canonicalize(path).ok()?;
    reqwest::Url::from_file_path(absolute).ok().map(|u| u.to_string())
}

fn maybe_local_file_uri(
    config: &Config,
    api_base_url: &str,
    path: &std::path::Path,
) -> Option<String> {
    if !config.upload_local_file_uri {
        return None;
    }
    if is_official_telegram_api(api_base_url) {
        tracing::warn!(
            "local_file_uri enabled but using official Telegram API; falling back to multipart"
        );
        return None;
    }
    local_file_uri_from_path(path)
}
```

**Step 4: Run tests to verify pass**

Run: `cargo test bot::tests::local_file_uri_disabled_by_default bot::tests::local_file_uri_skips_official_api bot::tests::local_file_uri_builds_from_existing_path bot::tests::local_file_uri_returns_none_for_missing_path -- --exact`
Expected: PASS

**Step 5: Commit (optional)**

```
git add src/bot.rs
git commit -m "feat: add local file uri helper for uploads"
```

---

### Task 3: Use file:// for audio + thumbnail in raw upload

**Files:**
- Modify: `src/bot.rs`

**Step 1: Write failing test**

Add a helper to select upload source and tests for it in `src/bot.rs`:
```rust
#[derive(Debug, PartialEq, Eq)]
enum UploadFileTarget {
    LocalUri(String),
    Multipart,
}

fn select_local_upload_target(
    config: &Config,
    api_base_url: &str,
    path: &std::path::Path,
) -> UploadFileTarget {
    maybe_local_file_uri(config, api_base_url, path)
        .map(UploadFileTarget::LocalUri)
        .unwrap_or(UploadFileTarget::Multipart)
}

#[test]
fn upload_target_defaults_to_multipart() {
    let config = Config::default();
    let path = std::path::Path::new("/tmp/test.mp3");
    assert_eq!(
        super::select_local_upload_target(&config, "http://127.0.0.1:8081/botTOKEN/", path),
        super::UploadFileTarget::Multipart
    );
}

#[test]
fn upload_target_uses_local_uri_when_enabled() {
    let mut config = Config::default();
    config.upload_local_file_uri = true;

    let temp_name = format!(
        "music163bot_local_uri_target_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let temp_path = std::env::temp_dir().join(temp_name);
    std::fs::write(&temp_path, b"123").expect("write temp file");

    let target = super::select_local_upload_target(
        &config,
        "http://127.0.0.1:8081/botTOKEN/",
        &temp_path,
    );
    let _ = std::fs::remove_file(&temp_path);

    match target {
        super::UploadFileTarget::LocalUri(uri) => assert!(uri.starts_with("file://")),
        super::UploadFileTarget::Multipart => panic!("expected local uri"),
    }
}
```

**Step 2: Run current tests to verify baseline**

Run: `cargo test bot::tests::local_file_uri_* -- --exact`
Expected: PASS

**Step 3: Implement raw upload changes**

Update `raw_send_file` signature to accept config:
```rust
async fn raw_send_file(
    client: &reqwest::Client,
    api_base_url: &str,
    config: &Config,
    audio_buffer: &AudioBuffer,
    audio_bytes: Option<&Bytes>,
    file_size: u64,
    params: &RawUploadParams<'_>,
) -> Result<serde_json::Value> {
```

Inside `raw_send_file`, use local file URI when possible:
```rust
let mut form = reqwest::multipart::Form::new()
    .text("chat_id", params.chat_id.to_string())
    .text("caption", params.caption.to_owned());

let audio_uri = match audio_buffer {
    AudioBuffer::Disk { path, .. } => maybe_local_file_uri(config, api_base_url, path),
    AudioBuffer::Memory { .. } => None,
};

if let Some(uri) = audio_uri {
    form = form.text("audio", uri);
} else {
    // existing file_part logic
    form = form.part("audio", file_part);
}
```

For thumbnail:
```rust
if let Some(thumb) = params.thumbnail {
    match thumb {
        ThumbnailBuffer::Disk { path } => {
            if let Some(uri) = maybe_local_file_uri(config, api_base_url, path) {
                form = form.text("thumbnail", uri);
            } else {
                // existing Part logic
            }
        }
        ThumbnailBuffer::Memory { .. } => {
            // existing Part logic
        }
    }
}
```

Update call site in `download_and_send_music`:
```rust
let upload_result = raw_send_file(
    &raw_client,
    &api_base_url,
    &state.config,
    &audio_buffer,
    audio_bytes.as_ref(),
    file_size,
    &params,
)
.await;
```

**Step 4: Run tests**

Run: `cargo test bot::tests::local_file_uri_* -- --exact`
Expected: PASS

**Step 5: Commit (optional)**

```
git add src/bot.rs
git commit -m "feat: enable local file uri for raw audio uploads"
```

---

### Task 4: Add raw sendDocument for lyrics

**Files:**
- Modify: `src/bot.rs`

**Step 1: Write failing test**

Reuse `select_local_upload_target` tests added in Task 3 (no new unit-test surface here).

**Step 2: Implement raw_send_document**

Add near `raw_send_file`:
```rust
struct RawDocumentParams<'a> {
    chat_id: i64,
    reply_to_message_id: i32,
    caption: Option<&'a str>,
}

async fn raw_send_document(
    client: &reqwest::Client,
    api_base_url: &str,
    config: &Config,
    document_path: &std::path::Path,
    params: &RawDocumentParams<'_>,
) -> Result<serde_json::Value> {
    let mut form = reqwest::multipart::Form::new()
        .text("chat_id", params.chat_id.to_string());

    let reply_params = serde_json::json!({ "message_id": params.reply_to_message_id });
    form = form.text("reply_parameters", reply_params.to_string());

    if let Some(caption) = params.caption {
        form = form.text("caption", caption.to_string());
    }

    if let Some(uri) = maybe_local_file_uri(config, api_base_url, document_path) {
        form = form.text("document", uri);
    } else {
        let file = tokio::fs::File::open(document_path).await
            .map_err(|e| BotError::Other(anyhow::anyhow!("Failed to open document: {e}")))?;
        let len = file.metadata().await
            .map_err(|e| BotError::Other(anyhow::anyhow!("Failed to stat document: {e}")))?
            .len();
        let stream = ReaderStream::with_capacity(file, RAW_UPLOAD_CHUNK_SIZE);
        let body = reqwest::Body::wrap_stream(stream);
        let filename = document_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file.lrc");
        let part = reqwest::multipart::Part::stream_with_length(body, len)
            .file_name(filename.to_string())
            .mime_str("text/plain")?;
        form = form.part("document", part);
    }

    let url = format!("{api_base_url}sendDocument");
    // Same status+json handling as raw_send_file
}
```

**Step 3: Wire lyrics upload to raw_send_document**

In `handle_lyric_command` after writing the file:
```rust
let (_upload_bot, raw_client, api_base_url) = acquire_upload_client(state).await?;
let _upload_permit = acquire_upload_permit(&state.upload_semaphore).await?;
let params = RawDocumentParams {
    chat_id: msg.chat.id.0,
    reply_to_message_id: msg.id.0,
    caption: None,
};
raw_send_document(
    &raw_client,
    &api_base_url,
    &state.config,
    std::path::Path::new(&lrc_path),
    &params,
)
.await?;
```

**Step 4: Run tests**

Run: `cargo test bot::tests::local_file_uri_* -- --exact`
Expected: PASS

**Step 5: Commit (optional)**

```
git add src/bot.rs
git commit -m "feat: use raw sendDocument with optional file uri"
```

---

### Task 5: Update docs and example config

**Files:**
- Modify: `config.ini.example`
- Modify: `README.md`

**Step 1: Update config example**

Add under `[upload]` in `config.ini.example`:
```ini
# Local API (--local) uses file:// to upload local files
# Default off; only disk files are eligible, memory data still uses multipart
local_file_uri = false
```

**Step 2: Update README**

Add a short note in upload config section:
- `upload.local_file_uri` description and that it requires `telegram-bot-api --local`.

**Step 3: Commit (optional)**

```
git add config.ini.example README.md
git commit -m "docs: document local file uri upload toggle"
```

---

### Task 6: Verification

**Step 1: Run focused tests**

Run: `cargo test local_file_uri -- --exact`
Expected: PASS

**Step 2: Run broader checks**

Run: `cargo test`
Expected: PASS
