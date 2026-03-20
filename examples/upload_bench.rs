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

#[path = "upload_bench/support.rs"]
mod upload_bench_support;

use upload_bench_support::{
    DeleteContext, record_upload_result, upload_memory_clone, upload_memory_move, upload_prebuilt,
    upload_stream,
};

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
        let delete_ctx = DeleteContext {
            client: &client,
            api_base,
            token: &args.token,
            chat_id: &args.chat_id,
            enabled: args.delete,
        };

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
                    record_upload_result(
                        result,
                        run + 1,
                        elapsed_ms,
                        size_mb,
                        &mut samples,
                        Some(&label),
                        &delete_ctx,
                    )
                    .await;
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
            record_upload_result(
                result,
                run + 1,
                elapsed_ms,
                size_mb,
                &mut samples,
                None,
                &delete_ctx,
            )
            .await;
        }

        if !samples.is_empty() {
            print_stats(mode, file_size, &samples);
        }
        println!();
    }

    Ok(())
}
