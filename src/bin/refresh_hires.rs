//! Maintenance tool: refresh cached songs whose Telegram `file_id` was uploaded
//! at a lower quality than the NetEase catalog's best available tier.
//!
//! ## Why
//! Each row in `song_infos` stores a Telegram `file_id` so repeated requests just
//! re-forward the already-uploaded audio. If that audio was fetched at a capped
//! tier (e.g. 16-bit `lossless` FLAC when the catalog offers 24-bit `hires`,
//! or MP3 when the catalog offers FLAC), the bot keeps re-sending the
//! low-quality copy forever. This tool finds those rows so the bot re-downloads
//! them at the current (hires-capable) candidate order.
//!
//! ## How it decides
//! For each cached song below `--max-cached-bitrate`, it probes the catalog via
//! the batch endpoint `POST /api/v3/song/detail` (body `c=[{"id":..},..]`, up to
//! 50 ids per request), then scans *every* object-valued field with a numeric
//! `br` (the v3 schema names tiers `l/m/h/sq/hr/jm/...`; scanning by shape rather
//! than name auto-discovers hires and master tiers without name churn) and takes
//! the maximum. If that max exceeds the cached `bit_rate` by more than `--margin`,
//! the row is a refresh candidate.
//!
//! ## Safety
//! Dry run by default (prints candidates, deletes nothing). `--apply` backs the
//! database up to `<db>.bak` first, then deletes candidate rows in one
//! transaction.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing_subscriber::EnvFilter;

/// Chunk size for `/api/v3/song/detail` requests. The endpoint accepts more, but
/// 50 keeps response bodies small and matches NetEase's own client behavior.
const BATCH_SIZE: usize = 50;

/// A cached row — only the columns needed to decide + report + delete.
#[derive(Clone)]
struct CachedSong {
    music_id: i64,
    name: String,
    file_ext: String,
    cached_bitrate: i64,
}

#[derive(Parser)]
#[command(
    about = "Find cached songs downloadable at higher quality and refresh them",
    long_about = "Probes NetEase catalog (batch /api/v3/song/detail) for each cached \
                  song below a bitrate ceiling and compares the best tier to the cached \
                  copy. Prints refresh candidates (dry run by default)."
)]
struct Args {
    /// Path (or `sqlite:` DSN) to the bot's database — same value as config `[database] url`.
    #[arg(long, default_value = "./data/music_bot.db")]
    db: String,

    /// NetEase API base URL.
    #[arg(long, default_value = "https://music.163.com")]
    api: String,

    /// Actually delete candidate rows. Without this flag the tool only reports.
    #[arg(long)]
    apply: bool,

    /// Concurrent batch requests. Each batch covers `BATCH_SIZE` songs, so even
    /// concurrency 3 walks ~1400 songs in well under a minute. Keep modest to
    /// avoid tripping NetEase's burst limiter.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,

    /// Only probe cached songs whose `bit_rate` is below this (bps). Songs at or
    /// above are assumed already near-optimal and skipped to save requests.
    /// 1_300_000 = just above the lossless tier.
    #[arg(long, default_value_t = 1_300_000)]
    max_cached_bitrate: i64,

    /// Require the catalog's best bitrate to exceed the cached bitrate by this
    /// fraction before flagging (0.15 = 15%). Absorbs rounding/encoding noise so
    /// a same-tier re-fetch is not treated as an upgrade.
    #[arg(long, default_value_t = 0.15)]
    margin: f64,
}

#[derive(Deserialize)]
struct DetailResponse {
    code: i32,
    songs: Vec<Value>,
}

/// Best catalog tier found for one song: `(bitrate_bps, tier_key)`.
type CatalogBest = Option<(i64, String)>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let args = Args::parse();
    let client = build_http_client()?;
    let limiter = Arc::new(Semaphore::new(args.concurrency.max(1)));

    let pool = open_pool(&args.db).await?;
    let songs = load_candidates(&pool, args.max_cached_bitrate).await?;
    let skipped = count_skipped(&pool, args.max_cached_bitrate).await?;

    let batch_count = songs.len().div_ceil(BATCH_SIZE);
    eprintln!(
        "{} cached songs below {} bps → {} batch request(s) of up to {} ({} already at/above — skipped).",
        songs.len(),
        args.max_cached_bitrate,
        batch_count,
        BATCH_SIZE,
        skipped
    );

    // Probe every batch concurrently under a semaphore; each resolves its whole
    // chunk before yielding a map of id → best tier.
    let mut futures = FuturesUnordered::new();
    for chunk in songs.chunks(BATCH_SIZE) {
        futures.push(probe_batch(
            client.clone(),
            &args.api,
            chunk.to_vec(),
            limiter.clone(),
        ));
    }

    // Build id → best catalog tier from all successful batches.
    let mut best_by_id: std::collections::HashMap<i64, (i64, String)> =
        std::collections::HashMap::new();
    let mut done = 0usize;
    let mut errors = 0usize;
    while let Some(outcome) = futures.next().await {
        done += 1;
        match outcome {
            Ok(map) => best_by_id.extend(map),
            Err(_) => errors += 1,
        }
        eprintln!(
            "  batch {done}/{} done (resolved {} ids, {errors} batch errors)",
            batch_count,
            best_by_id.len()
        );
    }

    // Classify each candidate against its catalog best.
    let refresh = classify(&songs, &best_by_id, args.margin);
    print_report(&refresh, &songs, best_by_id.len(), errors);

    if !args.apply {
        eprintln!(
            "\nDry run — no rows deleted. Re-run with --apply to delete the {} candidates.",
            refresh.len()
        );
        return Ok(());
    }
    if refresh.is_empty() {
        eprintln!("\nNothing to delete.");
        return Ok(());
    }

    backup_database(&args.db).await?;
    let deleted = delete_candidates(&args.db, &refresh).await?;
    eprintln!(
        "\nDeleted {deleted} candidate rows. The bot re-downloads these at the \
         hires-capable candidate order next time they are requested."
    );
    eprintln!("Run `VACUUM` on the database to reclaim space immediately.");
    Ok(())
}

/// Build a polite HTTP client with sensible timeouts for the detail probe.
fn build_http_client() -> Result<Client> {
    Ok(Client::builder()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .user_agent("Mozilla/5.0")
        .build()?)
}

/// Open the SQLite pool read-only, accepting a bare file path or a `sqlite:` DSN.
async fn open_pool(db: &str) -> Result<SqlitePool> {
    let mut options = if db.starts_with("sqlite:") {
        SqliteConnectOptions::new().filename(db.trim_start_matches("sqlite:"))
    } else {
        SqliteConnectOptions::new().filename(db)
    };
    options = options
        .read_only(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .min_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open database {db}"))?;
    Ok(pool)
}

/// Load cached songs whose bitrate falls under the probe ceiling.
async fn load_candidates(pool: &SqlitePool, max_bitrate: i64) -> Result<Vec<CachedSong>> {
    let rows = sqlx::query(
        "SELECT music_id, song_name, file_ext, bit_rate \
         FROM song_infos WHERE bit_rate < ? ORDER BY music_id",
    )
    .bind(max_bitrate)
    .fetch_all(pool)
    .await?;
    let songs = rows
        .into_iter()
        .map(|r| CachedSong {
            music_id: r.get::<i64, _>("music_id"),
            name: r.get::<String, _>("song_name"),
            file_ext: r.get::<String, _>("file_ext"),
            cached_bitrate: r.get::<i64, _>("bit_rate"),
        })
        .collect();
    Ok(songs)
}

/// Count rows at/above the ceiling, for the summary line.
async fn count_skipped(pool: &SqlitePool, max_bitrate: i64) -> Result<usize> {
    let row = sqlx::query("SELECT COUNT(*) AS n FROM song_infos WHERE bit_rate >= ?")
        .bind(max_bitrate)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("n") as usize)
}

/// Probe one batch of up to `BATCH_SIZE` songs. Returns a map of
/// `music_id → (best_bitrate, tier_key)`. Retries transient failures with
/// backoff; an unrecoverable batch surfaces as `Err` (those ids go unclassified,
/// i.e. left untouched — never wrongly deleted).
async fn probe_batch(
    client: Client,
    api: &str,
    chunk: Vec<CachedSong>,
    limiter: Arc<Semaphore>,
) -> Result<std::collections::HashMap<i64, (i64, String)>> {
    let _permit = limiter.acquire_owned().await?;
    let ids: Vec<i64> = chunk.iter().map(|s| s.music_id).collect();
    let c_value: String = ids
        .iter()
        .map(|id| format!("{{\"id\":{id}}}"))
        .collect::<Vec<_>>()
        .join(",");

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..4u8 {
        if attempt > 0 {
            // Backoff: ~1s, 2s, 4s before retries 1–3.
            sleep(Duration::from_millis(900u64 << attempt)).await;
        }
        let send_result = client
            .post(format!("{api}/api/v3/song/detail"))
            // `.body()` does not set Content-Type (unlike curl's `-d`); NetEase
            // needs `application/x-www-form-urlencoded` to parse the `c=[...]`
            // payload, else it returns a song-less body.
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("c=[{c_value}]"))
            .send()
            .await;
        let resp = match send_result {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(e.into());
                continue; // network/connect/timeout → retry
            }
        };
        let status = resp.status();
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            continue; // 429/5xx → transient, retry
        }
        let resp = resp.error_for_status()?;
        match resp.json::<DetailResponse>().await {
            Ok(parsed) => {
                if parsed.code != 200 {
                    last_err = Some(anyhow::anyhow!("api code {}", parsed.code));
                    continue;
                }
                return Ok(index_by_id(&parsed));
            }
            Err(e) => {
                last_err = Some(e.into());
                continue; // truncated body / parse error → retry
            }
        }
    }
    let err = last_err.unwrap_or_else(|| anyhow::anyhow!("probe_batch exhausted retries"));
    tracing::warn!("probe_batch failed for {} ids: {err:#}", ids.len());
    Err(err)
}

/// Map every returned song object to `id → (best_bitrate, tier_key)` by scanning
/// all object-valued fields with a numeric `br`. The v3 schema names tiers
/// `l/m/h/sq/hr/jm/...`; matching by shape (has `br`) auto-discovers new tiers.
fn index_by_id(parsed: &DetailResponse) -> std::collections::HashMap<i64, (i64, String)> {
    let mut map = std::collections::HashMap::with_capacity(parsed.songs.len());
    for song in &parsed.songs {
        let Some(id) = song.get("id").and_then(Value::as_i64) else {
            continue;
        };
        if let Some(best) = best_tier(song) {
            map.insert(id, best);
        }
    }
    map
}

/// Return the `(bitrate, tier_key)` of the highest-bitrate tier object on `song`.
fn best_tier(song: &Value) -> CatalogBest {
    let obj = song.as_object()?;
    let mut best: Option<(i64, String)> = None;
    for (key, value) in obj {
        let Some(m) = value.as_object() else {
            continue;
        };
        let br = m.get("br").and_then(Value::as_i64).unwrap_or(0);
        if br <= 0 {
            continue;
        }
        if best.as_ref().is_some_and(|(b, _)| br <= *b) {
            continue;
        }
        best = Some((br, key.clone()));
    }
    best
}

/// Classify candidates: a song is refresh-worthy when the catalog's best tier
/// exceeds its cached bitrate by more than `margin`.
fn classify(
    songs: &[CachedSong],
    best_by_id: &std::collections::HashMap<i64, (i64, String)>,
    margin: f64,
) -> Vec<(CachedSong, i64, String)> {
    let mut refresh = Vec::new();
    for song in songs {
        let Some((best_br, tier)) = best_by_id.get(&song.music_id) else {
            continue; // unresolved (batch error or song gone) → leave untouched
        };
        let threshold = (song.cached_bitrate as f64 * (1.0 + margin)) as i64;
        if *best_br > threshold {
            refresh.push((song.clone(), *best_br, tier.clone()));
        }
    }
    refresh.sort_by_key(|b| std::cmp::Reverse(b.1));
    refresh
}

/// Copy `<db>` to `<db>.bak` before any deletion (bare-path form only).
async fn backup_database(db: &str) -> Result<()> {
    if db.starts_with("sqlite:") {
        return Ok(()); // in-memory / DSN — nothing safe to back up
    }
    let from = std::path::Path::new(db);
    let backup = from.with_extension("db.bak");
    tokio::fs::copy(from, &backup)
        .await
        .with_context(|| format!("failed to back up {db} to {}", backup.display()))?;
    eprintln!("Backed up {db} -> {}", backup.display());
    Ok(())
}

/// Delete candidate rows in one transaction. Opens a fresh read-write pool.
async fn delete_candidates(db: &str, candidates: &[(CachedSong, i64, String)]) -> Result<u64> {
    let mut opts = if db.starts_with("sqlite:") {
        SqliteConnectOptions::new().filename(db.trim_start_matches("sqlite:"))
    } else {
        SqliteConnectOptions::new().filename(db)
    };
    opts = opts
        .read_only(false)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    let mut tx = pool.begin().await?;
    let mut total = 0u64;
    for (song, _, _) in candidates {
        let res = sqlx::query("DELETE FROM song_infos WHERE music_id = ?")
            .bind(song.music_id)
            .execute(&mut *tx)
            .await?;
        total += res.rows_affected();
    }
    tx.commit().await?;
    Ok(total)
}

fn print_report(
    refresh: &[(CachedSong, i64, String)],
    probed: &[CachedSong],
    resolved: usize,
    errors: usize,
) {
    eprintln!("\n================ REFRESH CANDIDATES ================");
    eprintln!(
        "probed {} ids | {} resolved | {} upgradeable | {} batch errors",
        probed.len(),
        resolved,
        refresh.len(),
        errors
    );
    if refresh.is_empty() {
        eprintln!("Nothing to upgrade. Every probed song is already at its catalog best.");
        return;
    }
    eprintln!(
        "{:>12}  {:<5}  {:>9}  {:>9}  {:<5}  name",
        "music_id", "cache", "cached_br", "best_br", "tier"
    );
    for (song, best_br, tier) in refresh.iter().take(60) {
        eprintln!(
            "{:>12}  {:<5}  {:>9}  {:>9}  {:<5}  {}",
            song.music_id,
            song.file_ext,
            song.cached_bitrate,
            best_br,
            tier,
            truncate(&song.name, 28),
        );
    }
    if refresh.len() > 60 {
        eprintln!("  ... and {} more (not listed).", refresh.len() - 60);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
