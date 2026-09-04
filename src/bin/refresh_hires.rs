//! Maintenance tool: refresh cached songs whose Telegram `file_id` holds a
//! lower-quality copy than the bot can actually download right now.
//!
//! ## Why
//! Each row in `song_infos` stores a Telegram `file_id` so repeated requests just
//! re-forward the already-uploaded audio. If that audio was fetched at a capped
//! tier (e.g. 16-bit `lossless` FLAC when the account can serve 24-bit `hires`,
//! or MP3 when the account can serve FLAC), the bot keeps re-sending the
//! low-quality copy forever. This tool finds those rows so the bot re-downloads
//! them at the current (hires-capable) candidate order.
//!
//! ## How it decides (ground truth — not catalog metadata)
//! For each cached song below `--max-cached-bitrate`, the tool asks the SAME
//! endpoint the bot uses at request time: `/eapi/song/enhance/player/url/v1`
//! with `level=hires` (the bot's top candidate, authenticated with the bot's
//! `MUSIC_U` cookie). It batches plain-number ids per request. The response
//! carries the `size` (bytes) of the best file the account can serve. The tool
//! compares that served `size` against the cached `music_size`:
//!   - served ≈ cached  → same file → leave it
//!   - served > cached × `--min-ratio` → a genuinely larger file exists and
//!     is downloadable → flag for refresh
//!
//! This is foolproof: it predicts exactly what the bot will re-download, because
//! it uses the identical endpoint. Catalog tier labels (`sq/hr/...`) are NOT
//! consulted — earlier versions that trusted them produced false positives
//! (e.g. the catalog advertises `sq` at 1411000 bps = the CD-rate nominal
//! rate, but the actual served file is the same compressed FLAC the bot already
//! has).
//!
//! ## MUSIC_U
//! A valid `music_u` cookie is REQUIRED. Supply it via:
//!   - `--music-u` flag
//!   - `MUSIC_U` environment variable
//!   - `--config <path>` (reads the bot's config.ini `[music]` section)
//!
//! Without the cookie the endpoint only serves standard/exhigh and no hires
//! upgrade is ever visible, making the tool useless.
//!
//! ## Safety
//! Dry run by default (prints candidates, deletes nothing). `--apply` backs the
//! database up to `<db>.bak` first, then deletes candidate rows in one
//! transaction.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// eapi crypto — mirrors the bot's `cache_and_crypto.rs` implementation.
// The bin is a separate compilation unit so it must carry its own copy.
// ---------------------------------------------------------------------------

/// NetEase eapi AES-ECB key (same as `e82ckenh8dichen8` in the main crate).
const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";

/// The `User-Agent` header the bot sends for eapi requests.
const EAPI_USER_AGENT: &str = "NeteaseMusic/9.3.40.1753206443(164);Dalvik/2.1.0 (Linux; U; Android 9; MIX 2 MIUI/V12.0.1.0.PDECNXM)";

/// Minimum served/cached size ratio to consider a row genuinely upgradeable.
/// 1.15 means the served file must be at least 15% larger (well above re-encode
/// noise).  Genuine resolution jumps (16-bit → 24-bit) are typically 1.5×–3.5×.
const MIN_UPGRADE_RATIO: f64 = 1.15;

// ---------------------------------------------------------------------------
// eapi crypto helpers
// ---------------------------------------------------------------------------

fn eapi_splice(path: &str, json: &str) -> String {
    let text = format!("nobody{path}use{json}md5forencrypt");
    let digest = md5::compute(text.as_bytes());
    format!("{path}-36cd479b6b5-{json}-36cd479b6b5-{digest:x}")
}

fn eapi_encrypt(data: &str) -> Result<String> {
    use aes::Aes128;
    use cipher::{BlockModeEncrypt, KeyInit, block_padding::Pkcs7};
    use ecb::Encryptor;

    let data_len = data.len();
    let block_size = 16_usize;
    let padded_len = ((data_len + block_size) / block_size) * block_size;
    let mut buf = vec![0u8; padded_len];
    buf[..data_len].copy_from_slice(data.as_bytes());

    let encrypted = Encryptor::<Aes128>::new_from_slice(EAPI_KEY)
        .map_err(|_| anyhow::anyhow!("invalid eapi key length"))?
        .encrypt_padded::<Pkcs7>(&mut buf, data_len)
        .map_err(|_| anyhow::anyhow!("failed to encrypt eapi payload"))?;

    Ok(hex::encode_upper(encrypted))
}

fn eapi_decrypt(hex_data: &str) -> Result<String> {
    use aes::Aes128;
    use cipher::{BlockModeDecrypt, KeyInit, block_padding::Pkcs7};
    use ecb::Decryptor;

    let mut bytes = hex::decode(hex_data).context("invalid hex in eapi response")?;
    let decrypted = Decryptor::<Aes128>::new_from_slice(EAPI_KEY)
        .map_err(|_| anyhow::anyhow!("invalid eapi key length"))?
        .decrypt_padded::<Pkcs7>(&mut bytes)
        .context("failed to decrypt eapi response")?;

    String::from_utf8(decrypted.to_vec()).context("non-utf8 decrypted eapi response")
}

fn eapi_params(path: &str, json: &str) -> Result<String> {
    Ok(format!(
        "params={}",
        eapi_encrypt(&eapi_splice(path, json))?
    ))
}

/// Build the `Cookie` header the bot sends for eapi requests.
/// Mirrors `generate_eapi_cookie` in `cache_and_crypto.rs`.
fn build_eapi_cookie(music_u: &str) -> String {
    let device_id = uuid::Uuid::new_v4().simple().to_string();
    let appver = "9.3.40";
    let buildver = std::time::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let buildver = &buildver[..buildver.len().min(10)];
    format!(
        "deviceId={device_id}; appver={appver}; buildver={buildver}; \
         resolution=1920x1080; os=Android; MUSIC_U={music_u}"
    )
}

// ---------------------------------------------------------------------------
// Config reading (shared with the bot: src/config/ini.rs)
// ---------------------------------------------------------------------------

/// Flat `section.key → value` INI parser, shared with the bot crate.
#[path = "../config/ini.rs"]
mod ini;

use ini::parse_ini_text;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CachedSong {
    music_id: i64,
    name: String,
    file_ext: String,
    cached_bitrate: i64,
    cached_size: i64,
}

#[derive(Clone)]
struct ServedSize {
    br: i64,
    size: i64,
    format: String,
}

#[derive(Clone)]
struct Upgrade {
    song: CachedSong,
    served: ServedSize,
    ratio: f64,
}

fn null_to_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

fn null_to_zero_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(Option::<i64>::deserialize(d)?.unwrap_or(0))
}

#[derive(Deserialize)]
struct EapiSongUrl {
    id: i64,
    #[serde(default, deserialize_with = "null_to_zero_i64")]
    br: i64,
    #[serde(default, deserialize_with = "null_to_zero_i64")]
    size: i64,
    #[serde(rename = "type", default, deserialize_with = "null_to_empty")]
    file_type: String,
}

#[derive(Deserialize)]
struct EapiResponse {
    code: i32,
    #[serde(default)]
    data: Vec<EapiSongUrl>,
}

#[derive(Parser)]
#[command(
    about = "Find cached songs that can be refreshed at higher quality",
    long_about = "Probes the real eapi song-url endpoint (the same one the bot uses) \
                  at level=hires for each cached song below the bitrate ceiling, \
                  compares the SERVED FILE SIZE (bytes) against the cached file size, \
                  and flags rows where the served file is genuinely larger. \
                  Dry run by default — no rows are deleted without --apply."
)]
struct Args {
    /// Path (or `sqlite:` DSN) to the bot's database — overrides `[database] url` from config.
    #[arg(long)]
    db: Option<String>,

    /// Path to the bot's config.ini. Used to read `[music] music_u` and
    /// `[database] url` if the respective CLI flags are absent.
    #[arg(long, default_value = "config.ini")]
    config: String,

    /// NetEase MUSIC_U cookie (overrides the value in config.ini or MUSIC_U env).
    #[arg(long, value_name = "COOKIE")]
    music_u: Option<String>,

    /// NetEase API base URL. Defaults to the official endpoint.
    #[arg(long, default_value = "https://music.163.com")]
    api: String,

    /// Actually delete candidate rows. Without this flag the tool only reports.
    #[arg(long)]
    apply: bool,

    /// Concurrent batch requests. Keep modest to avoid tripping NetEase's burst
    /// limiter. Each batch covers --batch-size songs.
    #[arg(long, default_value_t = 3)]
    concurrency: usize,

    /// How many song ids to pack into a single eapi request. 20 works well.
    #[arg(long, default_value_t = 20)]
    batch_size: usize,

    /// Only probe cached songs whose bit_rate is below this (bps). Songs at or
    /// above are assumed already near-optimal. 1_500_000 covers all lossless-tier
    /// FLAC (16-bit/44.1kHz nominal = 1411kbps; 16-bit/48kHz nominal = 1536kbps).
    #[arg(long, default_value_t = 1_500_000)]
    max_cached_bitrate: i64,

    /// Minimum ratio of served_size / cached_size to treat as a real upgrade.
    /// 1.15 means the served file must be ≥15% larger. Genuine resolution jumps
    /// (16-bit → 24-bit) are typically 1.5×–3.5×.
    #[arg(long, default_value_t = MIN_UPGRADE_RATIO)]
    min_ratio: f64,
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let args = Args::parse();
    // A missing config file means defaults (empty map, as before).
    let ini = parse_ini_text(&std::fs::read_to_string(&args.config).unwrap_or_default());

    // Resolve db: CLI → config `database.url` → default
    let db = args
        .db
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| ini.get("database.url").cloned())
        .unwrap_or_else(|| "./data/music_bot.db".to_string());

    // Resolve music_u: CLI → env → config.ini
    let music_u = args
        .music_u
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("MUSIC_U").ok().filter(|s| !s.is_empty()))
        .or_else(|| ini.get("music.music_u").cloned().filter(|s| !s.is_empty()))
        .context(
            "music_u cookie is required — supply it with --music-u, MUSIC_U env var, \
             or add `music_u = <cookie>` to the [music] section of config.ini.\n\
             Without the cookie the eapi endpoint only serves standard/exhigh and \
             the tool cannot discover hires upgrades.",
        )?;

    let cookie = build_eapi_cookie(&music_u);

    let client = build_http_client()?;
    let limiter = Arc::new(Semaphore::new(args.concurrency.max(1)));

    let pool = open_pool(&db).await?;
    let songs = load_candidates(&pool, args.max_cached_bitrate).await?;
    let skipped = count_skipped(&pool, args.max_cached_bitrate).await?;

    let batch_count = songs.len().div_ceil(args.batch_size);
    eprintln!(
        "{} cached songs below {} bps → {} batch request(s) of up to {} \
         ({} already at/above — skipped).",
        songs.len(),
        args.max_cached_bitrate,
        batch_count,
        args.batch_size,
        skipped
    );

    // Probe every batch concurrently under a semaphore; each resolves its chunk
    // to a map of id → served size.
    let mut futures = FuturesUnordered::new();
    for chunk in songs.chunks(args.batch_size) {
        let ids: Vec<i64> = chunk.iter().map(|s| s.music_id).collect();
        futures.push(probe_batch(
            client.clone(),
            &args.api,
            ids,
            cookie.clone(),
            limiter.clone(),
        ));
    }

    let mut served_by_id: HashMap<i64, ServedSize> = HashMap::new();
    let mut done = 0usize;
    let mut errors = 0usize;
    while let Some(outcome) = futures.next().await {
        done += 1;
        match outcome {
            Ok(map) => served_by_id.extend(map),
            Err(_) => errors += 1,
        }
        if done.is_multiple_of(5) || done == batch_count {
            eprintln!(
                "  batch {done}/{} done (resolved {} ids, {errors} batch errors)",
                batch_count,
                served_by_id.len(),
            );
        }
    }

    let upgrades = classify_by_size(&songs, &served_by_id, args.min_ratio);
    print_report(
        &upgrades,
        &songs,
        served_by_id.len(),
        errors,
        args.min_ratio,
    );

    if !args.apply {
        eprintln!(
            "\nDry run — no rows deleted. Re-run with --apply to delete the {} candidates.",
            upgrades.len()
        );
        return Ok(());
    }
    if upgrades.is_empty() {
        eprintln!("\nNothing to delete.");
        return Ok(());
    }

    backup_database(&db).await?;
    let deleted = delete_candidates(&db, &upgrades).await?;
    eprintln!(
        "\nDeleted {deleted} candidate rows. The bot re-downloads these at the \
         hires-capable candidate order next time they are requested."
    );
    eprintln!("Run `VACUUM` on the database to reclaim space immediately.");
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP / DB helpers
// ---------------------------------------------------------------------------

fn build_http_client() -> Result<Client> {
    Ok(Client::builder()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .user_agent("Mozilla/5.0")
        .build()?)
}

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

async fn load_candidates(pool: &SqlitePool, max_bitrate: i64) -> Result<Vec<CachedSong>> {
    let rows = sqlx::query(
        "SELECT music_id, song_name, file_ext, bit_rate, music_size \
         FROM song_infos WHERE bit_rate < ? AND music_size > 0 ORDER BY music_id",
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
            cached_size: r.get::<i64, _>("music_size"),
        })
        .collect();
    Ok(songs)
}

async fn count_skipped(pool: &SqlitePool, max_bitrate: i64) -> Result<usize> {
    let row = sqlx::query("SELECT COUNT(*) AS n FROM song_infos WHERE bit_rate >= ?")
        .bind(max_bitrate)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("n") as usize)
}

// ---------------------------------------------------------------------------
// Probe (eapi song-url, ground truth)
// ---------------------------------------------------------------------------

/// Probe one batch of ids via the real eapi song-url endpoint.
/// Returns a map of `music_id → ServedSize` (the file the bot would actually
/// download at level=hires). Retries transient failures with backoff; an
/// unrecoverable batch surfaces as `Err` (those ids go unclassified, i.e. left
/// untouched — never wrongly deleted).
async fn probe_batch(
    client: Client,
    api: &str,
    ids: Vec<i64>,
    cookie: String,
    limiter: Arc<Semaphore>,
) -> Result<HashMap<i64, ServedSize>> {
    let _permit = limiter.acquire_owned().await?;

    let path = "/api/song/enhance/player/url/v1";
    let ids_str = format!(
        "[{}]",
        ids.iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let payload = serde_json::json!({
        "ids": ids_str,
        "level": "hires",
        "encodeType": "mp3",
        "header": "{}",
    });
    let payload_str = serde_json::to_string(&payload)?;
    let body = eapi_params(path, &payload_str)?;

    let url = format!("{api}/eapi/song/enhance/player/url/v1");

    let mut last_err: Option<anyhow::Error> = None;
    let first_id = ids.first().copied().unwrap_or(0);
    let last_id = ids.last().copied().unwrap_or(0);
    for attempt in 0..4u8 {
        if attempt > 0 {
            // Backoff: ~1s, 2s, 4s before retries 1–3.
            sleep(Duration::from_millis(900u64 << attempt)).await;
        }
        let send_result = client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", EAPI_USER_AGENT)
            .header("Cookie", &cookie)
            .body(body.clone())
            .send()
            .await;

        let resp = match send_result {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(e.into());
                eprintln!(
                    "  [net-err batch {first_id}..{last_id} attempt {}/4] {}",
                    attempt + 1,
                    last_err.as_ref().unwrap()
                );
                continue; // network/connect/timeout → retry
            }
        };
        let status = resp.status();
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            eprintln!(
                "  [http-err batch {first_id}..{last_id} attempt {}/4] HTTP {status}",
                attempt + 1,
            );
            continue; // 429/5xx → transient, retry
        }
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(e.into());
                eprintln!(
                    "  [http-status batch {first_id}..{last_id} attempt {}/4] {}",
                    attempt + 1,
                    last_err.as_ref().unwrap()
                );
                continue; // 4xx → retry
            }
        };
        let raw_bytes = resp.bytes().await?;
        // Trim leading ASCII whitespace (the bot does this too).
        let start = raw_bytes
            .iter()
            .position(|&b| !b.is_ascii_whitespace())
            .unwrap_or(0);
        let trimmed = &raw_bytes[start..];

        let parsed: EapiResponse = if trimmed.first() == Some(&b'{') {
            // Plaintext JSON response.
            match serde_json::from_slice(trimmed) {
                Ok(p) => p,
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("parse eapi json: {e}"));
                    eprintln!(
                        "  [parse-json batch {first_id}..{last_id} attempt {}/4] {}",
                        attempt + 1,
                        last_err.as_ref().unwrap()
                    );
                    continue;
                }
            }
        } else {
            // Encrypted (hex) response — decrypt first.
            let hex_str = match std::str::from_utf8(trimmed) {
                Ok(s) => s.trim(),
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("non-utf8 eapi response: {e}"));
                    eprintln!(
                        "  [parse-utf8 batch {first_id}..{last_id} attempt {}/4] {}",
                        attempt + 1,
                        last_err.as_ref().unwrap()
                    );
                    continue;
                }
            };
            let decrypted = match eapi_decrypt(hex_str) {
                Ok(d) => d,
                Err(e) => {
                    last_err = Some(e);
                    eprintln!(
                        "  [decrypt batch {first_id}..{last_id} attempt {}/4] {}",
                        attempt + 1,
                        last_err.as_ref().unwrap()
                    );
                    continue;
                }
            };
            match serde_json::from_str(&decrypted) {
                Ok(p) => p,
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("parse decrypted eapi: {e}"));
                    eprintln!(
                        "  [parse-decrypted batch {first_id}..{last_id} attempt {}/4] {}",
                        attempt + 1,
                        last_err.as_ref().unwrap()
                    );
                    continue;
                }
            }
        };

        if parsed.code != 200 {
            last_err = Some(anyhow::anyhow!("eapi code {}", parsed.code));
            eprintln!(
                "  [eapi-code batch {first_id}..{last_id} attempt {}/4] code={}",
                attempt + 1,
                parsed.code,
            );
            continue; // auth or transient server issue
        }

        let map: HashMap<i64, ServedSize> = parsed
            .data
            .into_iter()
            .filter(|d| d.size > 0)
            .map(|d| {
                (
                    d.id,
                    ServedSize {
                        br: d.br,
                        size: d.size,
                        format: d.file_type,
                    },
                )
            })
            .collect();

        return Ok(map);
    }
    let err = last_err.unwrap_or_else(|| anyhow::anyhow!("probe_batch exhausted retries"));
    eprintln!("  [FAIL batch {first_id}..{last_id}] after 4 attempts: {err:#}",);
    Err(err)
}

// ---------------------------------------------------------------------------
// Classification (size-based, ground truth)
// ---------------------------------------------------------------------------

/// Classify candidates: a row is upgradeable when the served file size is
/// materially larger than the cached file size.  This is ground truth: if the
/// server hands back a bigger file, it's genuinely higher-resolution audio.
/// If the sizes are the same (± re-encode noise), the cached copy is already
/// the best the account can serve.
fn classify_by_size(
    songs: &[CachedSong],
    served: &HashMap<i64, ServedSize>,
    min_ratio: f64,
) -> Vec<Upgrade> {
    let mut upgrades = Vec::new();
    for song in songs {
        let Some(s) = served.get(&song.music_id) else {
            continue; // unresolved (batch error or song gone) → leave untouched
        };
        if song.cached_size <= 0 {
            continue;
        }
        let ratio = s.size as f64 / song.cached_size as f64;
        if ratio > min_ratio {
            upgrades.push(Upgrade {
                song: song.clone(),
                served: ServedSize {
                    br: s.br,
                    size: s.size,
                    format: s.format.clone(),
                },
                ratio,
            });
        }
    }
    // Sort by absolute byte gain descending (biggest wins first).
    upgrades.sort_by(|a, b| {
        let gain_a = a.served.size - a.song.cached_size;
        let gain_b = b.served.size - b.song.cached_size;
        gain_b.cmp(&gain_a)
    });
    upgrades
}

// ---------------------------------------------------------------------------
// Safety
// ---------------------------------------------------------------------------

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
async fn delete_candidates(db: &str, upgrades: &[Upgrade]) -> Result<u64> {
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
    for u in upgrades {
        let res = sqlx::query("DELETE FROM song_infos WHERE music_id = ?")
            .bind(u.song.music_id)
            .execute(&mut *tx)
            .await?;
        total += res.rows_affected();
    }
    tx.commit().await?;
    Ok(total)
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

fn print_report(
    upgrades: &[Upgrade],
    probed: &[CachedSong],
    resolved: usize,
    errors: usize,
    min_ratio: f64,
) {
    eprintln!("\n================ REFRESH CANDIDATES (ground truth) ================");
    eprintln!(
        "probed {} ids | {} resolved | {} upgradeable (served file >{:.0}% larger) | {} batch errors",
        probed.len(),
        resolved,
        upgrades.len(),
        (min_ratio - 1.0) * 100.0,
        errors,
    );
    if upgrades.is_empty() {
        eprintln!("Nothing to upgrade. Every probed song is already at its best available tier.");
        return;
    }
    eprintln!(
        "{:>12}  {:<5}  {:>9}  {:>10}  {:>9}  {:>10}  {:>6}  {:>10}  name",
        "music_id", "ext", "cached_br", "cached_sz", "served_br", "served_sz", "ratio", "gain",
    );
    for u in upgrades.iter().take(60) {
        let gain = u.served.size - u.song.cached_size;
        eprintln!(
            "{:>12}  {:<5}  {:>9}  {:>10}  {:>9}  {:>10}  {:>5.2}x  {:>+10}  {}",
            u.song.music_id,
            u.song.file_ext,
            format_bps(u.song.cached_bitrate),
            format_bytes(u.song.cached_size),
            format_bps(u.served.br),
            format_bytes(u.served.size),
            u.ratio,
            format_bytes(gain),
            truncate(&u.song.name, 32),
        );
    }
    if upgrades.len() > 60 {
        eprintln!("  ... and {} more (not listed).", upgrades.len() - 60);
    }
}

fn format_bps(bps: i64) -> String {
    if bps >= 1_000_000 {
        format!("{:.1}M", bps as f64 / 1_000_000.0)
    } else {
        format!("{}k", bps / 1000)
    }
}

fn format_bytes(bytes: i64) -> String {
    let abs = bytes.unsigned_abs();
    let sign = if bytes < 0 { "-" } else { "" };
    if abs >= 1_048_576 {
        format!("{sign}{:.1}MB", abs as f64 / 1_048_576.0)
    } else if abs >= 1024 {
        format!("{sign}{}KB", abs / 1024)
    } else {
        format!("{sign}{abs}B")
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
