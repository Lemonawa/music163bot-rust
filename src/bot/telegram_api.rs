use super::*;

pub(super) async fn send_raw_upload_form(
    client: &reqwest::Client,
    url: &str,
    form: reqwest::multipart::Form,
    method: &str,
) -> Result<serde_json::Value> {
    let resp = client.post(url).multipart(form).send().await.map_err(|e| {
        BotError::Other(anyhow::anyhow!(
            "Raw upload request failed: {}",
            redact_bot_token_in_error_message(&e.to_string())
        ))
    })?;

    let status = resp.status();
    let body = resp.text().await.map_err(|e| {
        BotError::Other(anyhow::anyhow!(
            "Failed to read upload response: {}",
            sanitize_sensitive_text(&e.to_string())
        ))
    })?;
    parse_telegram_api_response(&body, status, method)
}

/// Upload a file via raw reqwest multipart with pre-computed Content-Length
/// and 256 KiB streaming chunks — bypasses teloxide's 8 KiB FramedRead + chunked encoding.
pub(super) async fn raw_send_file(
    client: &reqwest::Client,
    api_base_url: &str,
    config: &Config,
    is_official_api: bool,
    audio_buffer: &AudioBuffer,
    audio_bytes: Option<&Bytes>,
    file_size: u64,
    params: &RawUploadParams<'_>,
) -> Result<serde_json::Value> {
    let filename = audio_buffer.filename().to_owned();
    let mime_type = mime_for_filename(&filename);

    let mut form = reqwest::multipart::Form::new()
        .text("chat_id", params.chat_id.to_string())
        .text("caption", params.caption.to_owned());

    let audio_target = match audio_buffer {
        AudioBuffer::Disk { path, .. } => {
            select_local_upload_target(config, is_official_api, path).await
        }
        AudioBuffer::Memory { .. } => UploadFileTarget::Multipart,
    };

    match audio_target {
        UploadFileTarget::LocalUri(uri) => {
            form = form.text("audio", uri);
        }
        UploadFileTarget::Multipart => {
            // Build the file part with known length for Content-Length header
            let file_part = if let Some(bytes) = audio_bytes {
                // Memory mode: Bytes::clone() is O(1) — atomic refcount, no memcpy
                reqwest::multipart::Part::stream_with_length(bytes.clone(), file_size)
                    .file_name(filename.clone())
                    .mime_str(mime_type)?
            } else if let AudioBuffer::Disk { path, .. } = audio_buffer {
                let file = tokio::fs::File::open(path).await.map_err(|e| {
                    BotError::Other(anyhow::anyhow!("Failed to open file for upload: {e}"))
                })?;
                let stream = ReaderStream::with_capacity(file, RAW_UPLOAD_CHUNK_SIZE);
                let body = reqwest::Body::wrap_stream(stream);
                reqwest::multipart::Part::stream_with_length(body, file_size)
                    .file_name(filename.clone())
                    .mime_str(mime_type)?
            } else {
                return Err(BotError::Other(anyhow::anyhow!(
                    "Memory buffer without pre-shared Bytes"
                )));
            };
            form = form.part("audio", file_part);
        }
    }

    // reply_parameters as JSON
    let reply_params = format!(r#"{{"message_id":{}}}"#, params.reply_to_message_id);
    form = form.text("reply_parameters", reply_params);

    // reply_markup as JSON
    if let Some(ref markup_json) = params.reply_markup_json {
        form = form.text("reply_markup", markup_json.clone());
    }

    if let Some(title) = params.title {
        form = form.text("title", title.to_owned());
    }
    if let Some(performer) = params.performer {
        form = form.text("performer", performer.to_owned());
    }
    if let Some(duration) = params.duration {
        form = form.text("duration", duration.to_string());
    }

    // Attach thumbnail if available
    if let Some(thumb) = params.thumbnail {
        match thumb {
            ThumbnailBuffer::Memory { data } => {
                let len = data.len() as u64;
                let thumb_part = reqwest::multipart::Part::stream_with_length(data.clone(), len)
                    .file_name("thumb.jpg")
                    .mime_str("image/jpeg")?;
                form = form.part("thumbnail", thumb_part);
            }
            ThumbnailBuffer::Disk { path } => {
                match select_local_upload_target(config, is_official_api, path).await {
                    UploadFileTarget::LocalUri(uri) => {
                        form = form.text("thumbnail", uri);
                    }
                    UploadFileTarget::Multipart => {
                        let file = tokio::fs::File::open(path).await.map_err(|e| {
                            BotError::Other(anyhow::anyhow!("Failed to open thumbnail: {e}"))
                        })?;
                        let len = file
                            .metadata()
                            .await
                            .map_err(|e| {
                                BotError::Other(anyhow::anyhow!("Failed to stat thumbnail: {e}"))
                            })?
                            .len();
                        let stream = ReaderStream::with_capacity(file, RAW_UPLOAD_CHUNK_SIZE);
                        let body = reqwest::Body::wrap_stream(stream);
                        let thumb_part = reqwest::multipart::Part::stream_with_length(body, len)
                            .file_name("thumb.jpg")
                            .mime_str("image/jpeg")?;
                        form = form.part("thumbnail", thumb_part);
                    }
                }
            }
        }
    }

    let url = format!("{api_base_url}sendAudio");
    send_raw_upload_form(client, &url, form, "sendAudio").await
}

pub(super) fn redact_bot_token_in_error_message(message: &str) -> String {
    sanitize_sensitive_text(message)
}

pub(super) fn parse_telegram_api_response(
    body: &str,
    status: reqwest::StatusCode,
    method: &str,
) -> Result<serde_json::Value> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Upload response parse error: {e}. Body omitted for safety.");
        BotError::Other(anyhow::anyhow!("Failed to parse upload response: {e}"))
    })?;

    if !status.is_success() || json.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let description = json
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        let sanitized_description = sanitize_sensitive_text(description);
        tracing::error!("Telegram API error ({status}): {sanitized_description} [method={method}]",);
        return Err(BotError::Other(anyhow::anyhow!(
            "Telegram API error: {sanitized_description} (HTTP {status})",
        )));
    }

    Ok(json)
}

/// Extract file_id from a raw Telegram API sendAudio response.
pub(super) fn extract_file_id_from_response(json: &serde_json::Value) -> Option<String> {
    let result = json.get("result")?;
    result
        .get("audio")
        .and_then(|a| a.get("file_id"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Map filename extension to MIME type string.
pub(super) fn mime_for_filename(filename: &str) -> &'static str {
    let path = std::path::Path::new(filename);
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("flac") => "audio/flac",
        Some(ext) if ext.eq_ignore_ascii_case("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

pub(super) fn build_upload_bot(config: &Config) -> Result<UploadBotBundle> {
    // API URL must match teloxide's internal format: base URL without "/bot" suffix
    // teloxide automatically appends "bot<TOKEN>/" to the path
    let api_url_str = if !config.bot_api.is_empty() && config.bot_api != "https://api.telegram.org"
    {
        let base = config.bot_api.trim_end_matches("/bot");
        format!("{base}/")
    } else {
        "https://api.telegram.org/".to_string()
    };

    let api_url = match parse_api_url(&api_url_str) {
        Ok(url) => url,
        Err(e) => {
            tracing::warn!(
                "Invalid upload API URL '{}': {}. Using default.",
                sanitize_sensitive_text(&api_url_str),
                sanitize_sensitive_text(&e.to_string())
            );
            match parse_api_url("https://api.telegram.org/") {
                Ok(url) => url,
                Err(err) => {
                    tracing::error!(
                        "Failed to parse fallback API URL: {}",
                        sanitize_sensitive_text(&err.to_string())
                    );
                    return Err(BotError::Other(anyhow::anyhow!(
                        "failed to parse fallback API URL"
                    )));
                }
            }
        }
    };

    if api_url_str != "https://api.telegram.org/" {
        tracing::info!(
            "Using custom API for upload: {}",
            sanitize_sensitive_text(api_url.as_str())
        );
    }

    let mut client_builder = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(std::time::Duration::from_secs(config.upload_timeout_secs))
        .pool_max_idle_per_host(config.upload_pool_max_idle_per_host)
        .tcp_nodelay(true)
        .no_gzip()
        .user_agent("Go-http-client/2.0")
        .default_headers(reqwest::header::HeaderMap::new());

    if should_set_upload_pool_idle_timeout(config.upload_pool_idle_timeout_secs) {
        client_builder = client_builder.pool_idle_timeout(std::time::Duration::from_secs(
            config.upload_pool_idle_timeout_secs,
        ));
    }

    if upload_log_enabled(config, UploadLogLevel::Debug) {
        tracing::debug!(
            "Upload diag: client settings pool_max_idle_per_host={}, pool_idle_timeout_secs={}, timeout_secs={}, api_url={}",
            config.upload_pool_max_idle_per_host,
            config.upload_pool_idle_timeout_secs,
            config.upload_timeout_secs,
            sanitize_sensitive_text(api_url.as_str())
        );
    }

    let client = build_http_client(client_builder)?;
    let bot = Bot::with_client(&config.bot_token, client.clone()).set_api_url(api_url);

    // Build full API base URL for raw requests: "{base}bot{token}/"
    let raw_api_base = format!("{}bot{}/", api_url_str, config.bot_token);

    Ok(UploadBotBundle {
        bot,
        raw_client: client,
        api_base_url: raw_api_base,
    })
}

pub(super) async fn acquire_upload_client(
    state: &Arc<BotState>,
) -> Result<(Bot, reqwest::Client, String)> {
    let reuse_limit = state.config.upload_client_reuse_requests;

    let (reason, reuse_count_before) = {
        let mut upload_state = state.upload_client_state.lock().await;

        if !should_refresh_upload_client(&upload_state, reuse_limit) {
            if upload_log_enabled(&state.config, UploadLogLevel::Debug) {
                tracing::debug!(
                    "Upload diag: reusing client (reuse_count: {}, reuse_limit: {})",
                    upload_state.reuse_count,
                    reuse_limit
                );
            }
            return checkout_upload_client(&mut upload_state, &state.config);
        }

        let reason = if upload_state.bot.is_none() {
            "uninitialized"
        } else {
            "reuse_limit"
        };
        (reason, upload_state.reuse_count)
    };

    if upload_log_enabled(&state.config, UploadLogLevel::Info) {
        tracing::info!(
            "Upload diag: creating client (reason: {}, reuse_count: {}, reuse_limit: {})",
            reason,
            reuse_count_before,
            reuse_limit
        );
    }

    let build_start = std::time::Instant::now();
    let bundle = build_upload_bot(&state.config)?;

    let mut upload_state = state.upload_client_state.lock().await;
    if should_refresh_upload_client(&upload_state, reuse_limit) {
        upload_state.bot = Some(bundle.bot);
        upload_state.raw_client = Some(bundle.raw_client);
        upload_state.upload_api_url = bundle.api_base_url;
        upload_state.reuse_count = 0;
        if upload_log_enabled(&state.config, UploadLogLevel::Info) {
            tracing::info!(
                "Upload diag: client ready in {}ms",
                build_start.elapsed().as_millis()
            );
        }
    } else if upload_log_enabled(&state.config, UploadLogLevel::Debug) {
        tracing::debug!("Upload diag: client refreshed by another task");
    }

    checkout_upload_client(&mut upload_state, &state.config)
}

pub(super) fn checkout_upload_client(
    upload_state: &mut UploadClientState,
    config: &Config,
) -> Result<(Bot, reqwest::Client, String)> {
    let next_reuse_count = upload_state.reuse_count.saturating_add(1);
    if upload_log_enabled(config, UploadLogLevel::Debug) {
        tracing::debug!("Upload diag: reuse_count -> {}", next_reuse_count);
    }
    upload_state.reuse_count = next_reuse_count;

    let bot = get_upload_bot(upload_state)?;
    let raw_client = upload_state.raw_client.clone().unwrap_or_default();
    let api_url = upload_state.upload_api_url.clone();
    Ok((bot, raw_client, api_url))
}

pub(super) async fn run_upload_prewarm<T, F, Fut>(config: &Config, warmup: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    match warmup().await {
        Ok(_) => {
            if upload_log_enabled(config, UploadLogLevel::Info) {
                tracing::info!("Upload prewarm completed");
            }
            true
        }
        Err(e) => {
            tracing::warn!(
                "Upload prewarm failed, continuing startup: {}",
                sanitize_sensitive_text(&e.to_string())
            );
            false
        }
    }
}

pub(super) async fn acquire_download_permit(
    semaphore: &tokio::sync::Semaphore,
) -> Result<tokio::sync::SemaphorePermit<'_>> {
    acquire_semaphore_permit(semaphore, "download").await
}

pub(super) async fn acquire_semaphore_permit<'a>(
    semaphore: &'a tokio::sync::Semaphore,
    label: &str,
) -> Result<tokio::sync::SemaphorePermit<'a>> {
    semaphore.acquire().await.map_err(|e| {
        tracing::error!("{} semaphore closed: {}", label, e);
        BotError::Other(anyhow::anyhow!("{label} semaphore closed"))
    })
}

pub(super) async fn acquire_upload_permit(
    semaphore: &tokio::sync::Semaphore,
) -> Result<tokio::sync::SemaphorePermit<'_>> {
    acquire_semaphore_permit(semaphore, "upload").await
}

pub(super) async fn acquire_upload_permit_owned(
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit> {
    semaphore.acquire_owned().await.map_err(|e| {
        tracing::error!("Upload semaphore closed: {}", e);
        BotError::Other(anyhow::anyhow!("upload semaphore closed"))
    })
}
