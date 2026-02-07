//! Benchmark different reqwest multipart upload strategies.
//!
//! Usage:
//!   cargo run --release --example upload_bench -- \
//!     --api-base http://192.168.1.246:23941 \
//!     --token <BOT_TOKEN> \
//!     --chat-id <CHAT_ID> \
//!     --file /path/to/audio.flac \
//!     [--runs 6] [--warmup] [--delete] \
//!     [--modes stream-256k,memory-move,memory-clone,stream-1m,prebuilt]

use std::path::PathBuf;
use std::time::Instant;

use bytes::Bytes;
use reqwest::multipart::{Form, Part};
use tokio_util::io::ReaderStream;

// ── CLI ──────────────────────────────────────────────────────────────

struct Args {
    api_base: String,
    token: String,
    chat_id: String,
    file: PathBuf,
    method: String,
    runs: usize,
    warmup: bool,
    delete: bool,
    modes: Vec<String>,
    chunk_sizes: Vec<usize>,
}

fn parse_args() -> Args {
    let mut args = Args {
        api_base: "https://api.telegram.org".into(),
        token: std::env::var("BOT_TOKEN").unwrap_or_default(),
        chat_id: std::env::var("CHAT_ID").unwrap_or_default(),
        file: PathBuf::new(),
        method: "sendAudio".into(),
        runs: 6,
        warmup: false,
        delete: false,
        modes: vec![
            "stream-256k".into(),
            "memory-move".into(),
            "memory-clone".into(),
            "stream-1m".into(),
            "prebuilt".into(),
        ],
        chunk_sizes: Vec::new(),
    };

    let raw: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < raw.len() {
        match raw[i].as_str() {
            "--api-base" => {
                i += 1;
                args.api_base = raw[i].clone();
            }
            "--token" => {
                i += 1;
                args.token = raw[i].clone();
            }
            "--chat-id" => {
                i += 1;
                args.chat_id = raw[i].clone();
            }
            "--file" => {
                i += 1;
                args.file = PathBuf::from(&raw[i]);
            }
            "--method" => {
                i += 1;
                args.method = raw[i].clone();
            }
            "--runs" => {
                i += 1;
                args.runs = raw[i].parse().unwrap();
            }
            "--warmup" => args.warmup = true,
            "--delete" => args.delete = true,
            "--modes" => {
                i += 1;
                args.modes = raw[i].split(',').map(String::from).collect();
            }
            "--chunk-sizes" => {
                i += 1;
                args.chunk_sizes = raw[i].split(',').map(parse_size).collect();
            }
            other => {
                eprintln!("Unknown arg: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if args.token.is_empty() || args.chat_id.is_empty() || args.file.as_os_str().is_empty() {
        eprintln!(
            "Usage: upload_bench --token <T> --chat-id <C> --file <F> [--api-base <URL>] \
             [--runs N] [--warmup] [--delete] [--modes m1,m2,...] [--chunk-sizes 256k,1m,...]"
        );
        eprintln!(
            "\nModes: stream-256k, stream-1m, memory-move, memory-clone, prebuilt, chunk-sweep"
        );
        std::process::exit(1);
    }

    // Default chunk sweep sizes
    if args.modes.contains(&"chunk-sweep".to_string()) && args.chunk_sizes.is_empty() {
        args.chunk_sizes = vec![
            64 * 1024,
            128 * 1024,
            256 * 1024,
            512 * 1024,
            1024 * 1024,
            2 * 1024 * 1024,
        ];
    }

    args
}

fn parse_size(s: &str) -> usize {
    let s = s.trim().to_lowercase();
    if let Some(n) = s.strip_suffix('m') {
        n.parse::<usize>().unwrap() * 1024 * 1024
    } else if let Some(n) = s.strip_suffix('k') {
        n.parse::<usize>().unwrap() * 1024
    } else {
        s.parse().unwrap()
    }
}

// ── Stats ────────────────────────────────────────────────────────────

fn stats(samples: &[f64]) -> (f64, f64, f64, f64, f64) {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    let avg = sorted.iter().sum::<f64>() / n as f64;
    let p50 = sorted[n / 2];
    let p95 = sorted[(n as f64 * 0.95) as usize];
    let min = sorted[0];
    let max = sorted[n - 1];
    (avg, p50, p95, min, max)
}

fn print_stats(label: &str, file_size: u64, samples: &[f64]) {
    let (avg, p50, p95, min, max) = stats(samples);
    let size_mb = file_size as f64 / (1024.0 * 1024.0);
    let avg_mbps = size_mb / (avg / 1000.0);
    let p50_mbps = size_mb / (p50 / 1000.0);
    println!(
        "  {label:>16}: avg={avg:7.1}ms  p50={p50:7.1}ms  p95={p95:7.1}ms  \
         min={min:7.1}ms  max={max:7.1}ms  | avg={avg_mbps:5.1} MB/s  p50={p50_mbps:5.1} MB/s"
    );
}

// ── Upload helpers ───────────────────────────────────────────────────

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

/// Mode: stream from file with given chunk size
async fn upload_stream(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    file_size: u64,
    chat_id: &str,
    chunk_size: usize,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let file = tokio::fs::File::open(file_path).await?;
    let stream = ReaderStream::with_capacity(file, chunk_size);
    let body = reqwest::Body::wrap_stream(stream);
    let part = Part::stream_with_length(body, file_size)
        .file_name(filename(file_path))
        .mime_str(mime_for_file(file_path))?;

    let form = base_form(chat_id, &format!("bench-stream-{chunk_size}")).part("audio", part);

    let resp = client.post(url).multipart(form).send().await?;
    Ok(resp.json().await?)
}

/// Mode: read file to memory, then move Vec into Bytes (zero-copy)
async fn upload_memory_move(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    file_size: u64,
    chat_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let data = tokio::fs::read(file_path).await?;
    let bytes = Bytes::from(data); // moves Vec's allocation, no memcpy
    let part = Part::stream_with_length(bytes, file_size)
        .file_name(filename(file_path))
        .mime_str(mime_for_file(file_path))?;

    let form = base_form(chat_id, "bench-memory-move").part("audio", part);

    let resp = client.post(url).multipart(form).send().await?;
    Ok(resp.json().await?)
}

/// Mode: read file to memory, clone the Vec, send the clone
/// Simulates AudioBuffer::Memory where we keep the original
async fn upload_memory_clone(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    file_size: u64,
    chat_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let data = tokio::fs::read(file_path).await?;
    let bytes = Bytes::from(data.clone()); // clone + move — simulates current code
    drop(data); // prove we could still use data if needed
    let part = Part::stream_with_length(bytes, file_size)
        .file_name(filename(file_path))
        .mime_str(mime_for_file(file_path))?;

    let form = base_form(chat_id, "bench-memory-clone").part("audio", part);

    let resp = client.post(url).multipart(form).send().await?;
    Ok(resp.json().await?)
}

/// Mode: pre-build the multipart body bytes manually (like Python benchmark)
/// Read file into memory, build raw multipart bytes, send with known Content-Length
async fn upload_prebuilt(
    client: &reqwest::Client,
    url: &str,
    file_path: &std::path::Path,
    _file_size: u64,
    chat_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let data = tokio::fs::read(file_path).await?;
    let fname = filename(file_path);
    let mime = mime_for_file(file_path);
    let boundary = format!("----RustBench{}", uuid::Uuid::new_v4().as_simple());

    let mut body = Vec::new();
    // chat_id field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"chat_id\"\r\n\r\n");
    body.extend_from_slice(chat_id.as_bytes());
    body.extend_from_slice(b"\r\n");
    // caption field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"caption\"\r\n\r\n");
    body.extend_from_slice(b"bench-prebuilt\r\n");
    // audio file field
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

// ── Delete helper ────────────────────────────────────────────────────

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

// ── Main ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args();
    let api_base = args.api_base.trim_end_matches('/');
    let url = format!("{api_base}/bot{}/{}", args.token, args.method);

    let file_size = std::fs::metadata(&args.file)?.len();
    let size_mb = file_size as f64 / (1024.0 * 1024.0);

    println!("=== Reqwest Upload Benchmark ===");
    println!("  API:   {api_base}");
    println!("  File:  {} ({size_mb:.2} MB)", args.file.display());
    println!("  Runs:  {}", args.runs);
    println!("  Modes: {}", args.modes.join(", "));
    println!();

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .tcp_nodelay(true)
        .pool_max_idle_per_host(2)
        .no_gzip()
        .user_agent("Go-http-client/2.0")
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    // Warmup: establish connection
    if args.warmup {
        let me_url = format!("{api_base}/bot{}/getMe", args.token);
        let _ = client.get(&me_url).send().await?;
        println!("  Warmup: getMe OK\n");
    }

    for mode in &args.modes {
        if mode == "chunk-sweep" {
            println!("── chunk-sweep ──");
            for &chunk_size in &args.chunk_sizes {
                let label = format!("stream-{}k", chunk_size / 1024);
                let mut samples = Vec::new();

                for run in 0..args.runs {
                    let t = Instant::now();
                    let result = upload_stream(
                        &client,
                        &url,
                        &args.file,
                        file_size,
                        &args.chat_id,
                        chunk_size,
                    )
                    .await;
                    let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;

                    match result {
                        Ok(ref json) if json.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
                            samples.push(elapsed_ms);
                            let mbps = size_mb / (elapsed_ms / 1000.0);
                            println!(
                                "    [{label}] run {}: {elapsed_ms:.1}ms ({mbps:.1} MB/s)",
                                run + 1
                            );
                            if args.delete {
                                delete_msg(&client, api_base, &args.token, &args.chat_id, json)
                                    .await;
                            }
                        }
                        Ok(ref json) => {
                            let desc = json
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            eprintln!("    [{label}] run {}: API error: {desc}", run + 1);
                        }
                        Err(e) => {
                            eprintln!("    [{label}] run {}: ERROR: {e}", run + 1);
                        }
                    }
                }

                if !samples.is_empty() {
                    print_stats(&label, file_size, &samples);
                }
            }
            println!();
            continue;
        }

        println!("── {mode} ──");
        let mut samples = Vec::new();

        for run in 0..args.runs {
            let t = Instant::now();
            let result = match mode.as_str() {
                "stream-256k" => {
                    upload_stream(
                        &client,
                        &url,
                        &args.file,
                        file_size,
                        &args.chat_id,
                        256 * 1024,
                    )
                    .await
                }
                "stream-1m" => {
                    upload_stream(
                        &client,
                        &url,
                        &args.file,
                        file_size,
                        &args.chat_id,
                        1024 * 1024,
                    )
                    .await
                }
                "memory-move" => {
                    upload_memory_move(&client, &url, &args.file, file_size, &args.chat_id).await
                }
                "memory-clone" => {
                    upload_memory_clone(&client, &url, &args.file, file_size, &args.chat_id).await
                }
                "prebuilt" => {
                    upload_prebuilt(&client, &url, &args.file, file_size, &args.chat_id).await
                }
                other => {
                    eprintln!("Unknown mode: {other}");
                    break;
                }
            };
            let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;

            match result {
                Ok(ref json) if json.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
                    samples.push(elapsed_ms);
                    let mbps = size_mb / (elapsed_ms / 1000.0);
                    println!("    run {}: {elapsed_ms:7.1}ms  ({mbps:5.1} MB/s)", run + 1);
                    if args.delete {
                        delete_msg(&client, api_base, &args.token, &args.chat_id, json).await;
                    }
                }
                Ok(ref json) => {
                    let desc = json
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    eprintln!("    run {}: API error: {desc}", run + 1);
                }
                Err(e) => {
                    eprintln!("    run {}: ERROR: {e}", run + 1);
                }
            }
        }

        if !samples.is_empty() {
            print_stats(mode, file_size, &samples);
        }
        println!();
    }

    Ok(())
}
