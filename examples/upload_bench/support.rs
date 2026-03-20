use bytes::Bytes;
use reqwest::multipart::{Form, Part};
use tokio_util::io::ReaderStream;

pub(crate) type BenchResult = Result<serde_json::Value, Box<dyn std::error::Error>>;

fn mime_for_file(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("flac") => "audio/flac",
        Some(e) if e.eq_ignore_ascii_case("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn filename(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio")
        .to_string()
}

fn base_form(chat_id: &str, caption: &str) -> Form {
    Form::new()
        .text("chat_id", chat_id.to_string())
        .text("caption", caption.to_string())
}

async fn send_multipart_form(client: &reqwest::Client, url: &str, form: Form) -> BenchResult {
    let resp = client.post(url).multipart(form).send().await?;
    Ok(resp.json().await?)
}

fn upload_response_ok(json: &serde_json::Value) -> bool {
    json.get("ok").and_then(|v| v.as_bool()) == Some(true)
}

fn upload_response_description(json: &serde_json::Value) -> &str {
    json.get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
}

pub(crate) struct DeleteContext<'a> {
    pub(crate) client: &'a reqwest::Client,
    pub(crate) api_base: &'a str,
    pub(crate) token: &'a str,
    pub(crate) chat_id: &'a str,
    pub(crate) enabled: bool,
}

pub(crate) async fn record_upload_result(
    result: BenchResult,
    run_index: usize,
    elapsed_ms: f64,
    size_mb: f64,
    samples: &mut Vec<f64>,
    label: Option<&str>,
    delete_ctx: &DeleteContext<'_>,
) {
    match result {
        Ok(json) if upload_response_ok(&json) => {
            samples.push(elapsed_ms);
            let mbps = size_mb / (elapsed_ms / 1000.0);
            if let Some(label) = label {
                println!(
                    "    [{label}] run {}: {elapsed_ms:.1}ms ({mbps:.1} MB/s)",
                    run_index
                );
            } else {
                println!(
                    "    run {}: {elapsed_ms:7.1}ms  ({mbps:5.1} MB/s)",
                    run_index
                );
            }
            if delete_ctx.enabled {
                delete_msg(
                    delete_ctx.client,
                    delete_ctx.api_base,
                    delete_ctx.token,
                    delete_ctx.chat_id,
                    &json,
                )
                .await;
            }
        }
        Ok(json) => {
            let desc = upload_response_description(&json);
            if let Some(label) = label {
                eprintln!("    [{label}] run {}: API error: {desc}", run_index);
            } else {
                eprintln!("    run {}: API error: {desc}", run_index);
            }
        }
        Err(e) => {
            if let Some(label) = label {
                eprintln!("    [{label}] run {}: ERROR: {e}", run_index);
            } else {
                eprintln!("    run {}: ERROR: {e}", run_index);
            }
        }
    }
}

/// Mode: stream from file with given chunk size
pub(crate) async fn upload_stream(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    file_size: u64,
    chat_id: &str,
    chunk_size: usize,
) -> BenchResult {
    let file = tokio::fs::File::open(file_path).await?;
    let stream = ReaderStream::with_capacity(file, chunk_size);
    let body = reqwest::Body::wrap_stream(stream);
    let part = Part::stream_with_length(body, file_size)
        .file_name(filename(file_path))
        .mime_str(mime_for_file(file_path))?;

    let form = base_form(chat_id, &format!("bench-stream-{chunk_size}")).part("audio", part);
    send_multipart_form(client, url, form).await
}

/// Mode: read file to memory, then move Vec into Bytes (zero-copy)
pub(crate) async fn upload_memory_move(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    file_size: u64,
    chat_id: &str,
) -> BenchResult {
    let data = tokio::fs::read(file_path).await?;
    let bytes = Bytes::from(data);
    let part = Part::stream_with_length(bytes, file_size)
        .file_name(filename(file_path))
        .mime_str(mime_for_file(file_path))?;

    let form = base_form(chat_id, "bench-memory-move").part("audio", part);
    send_multipart_form(client, url, form).await
}

/// Mode: read file to memory, clone the Vec, send the clone
/// Simulates AudioBuffer::Memory where we keep the original
pub(crate) async fn upload_memory_clone(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    file_size: u64,
    chat_id: &str,
) -> BenchResult {
    let data = tokio::fs::read(file_path).await?;
    let bytes = Bytes::from(data.clone());
    drop(data);
    let part = Part::stream_with_length(bytes, file_size)
        .file_name(filename(file_path))
        .mime_str(mime_for_file(file_path))?;

    let form = base_form(chat_id, "bench-memory-clone").part("audio", part);
    send_multipart_form(client, url, form).await
}

/// Mode: pre-build the multipart body bytes manually (like Python benchmark)
/// Read file into memory, build raw multipart bytes, send with known Content-Length
pub(crate) async fn upload_prebuilt(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    _file_size: u64,
    chat_id: &str,
) -> BenchResult {
    let data = tokio::fs::read(file_path).await?;
    let fname = filename(file_path);
    let mime = mime_for_file(file_path);
    let boundary = format!("----RustBench{}", uuid::Uuid::new_v4().as_simple());

    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"chat_id\"\r\n\r\n");
    body.extend_from_slice(chat_id.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"caption\"\r\n\r\n");
    body.extend_from_slice(b"bench-prebuilt\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"audio\"; filename=\"{fname}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let content_type = format!("multipart/form-data; boundary={boundary}");
    let len = body.len();

    let resp = client
        .post(url)
        .header("Content-Type", content_type)
        .header("Content-Length", len)
        .body(body)
        .send()
        .await?;
    Ok(resp.json().await?)
}

async fn delete_msg(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: &str,
    json: &serde_json::Value,
) {
    if let Some(msg_id) = json.pointer("/result/message_id").and_then(|v| v.as_i64()) {
        let url = format!("{api_base}/bot{token}/deleteMessage");
        let _ = client
            .post(&url)
            .form(&[
                ("chat_id", chat_id.to_string()),
                ("message_id", msg_id.to_string()),
            ])
            .send()
            .await;
    }
}
