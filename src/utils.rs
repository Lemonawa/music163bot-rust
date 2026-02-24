use std::path::Path;

use regex::Regex;

use crate::error::{BotError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicCollectionTarget {
    Playlist(u64),
    Album(u64),
}

/// Build a reqwest HTTP client from a builder, logging and mapping errors on failure.
pub fn build_http_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    builder.build().map_err(|e| {
        tracing::error!("Failed to build HTTP client: {}", e);
        BotError::Network(e)
    })
}

/// Global regex patterns for URL parsing
static SONG_REGEX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"music\.163\.com/.*?song.*?[?&]id=(\d+)").expect("song id regex should be valid")
});

static SHARE_LINK_REGEX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(http|https)://[\w\-_]+(\.[\w\-_]+)+([\w\-.,@?^=%&:/~+#]*[\w\-@?^=%&/~+#])?")
        .expect("share link regex should be valid")
});

fn parse_direct_numeric_id(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok()
}

fn extract_music_id_query_value(text: &str) -> Option<u64> {
    let query_start = text.find('?')? + 1;
    let query = &text[query_start..];

    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };

        if key != "id" {
            continue;
        }

        let id_len = value.bytes().take_while(u8::is_ascii_digit).count();
        if id_len == 0 {
            return None;
        }

        return value[..id_len].parse::<u64>().ok();
    }

    None
}

fn extract_music_entity_id_from_canonical_url(text: &str, entity: &str) -> Option<u64> {
    let trimmed = text.trim();
    let is_music_domain = trimmed.starts_with("https://music.163.com/")
        || trimmed.starts_with("http://music.163.com/");
    let direct_marker = format!("/{entity}?");
    let hash_marker = format!("/#/{entity}?");
    let is_target_url = trimmed.contains(&direct_marker) || trimmed.contains(&hash_marker);
    if !is_music_domain || !is_target_url {
        return None;
    }

    extract_music_id_query_value(trimmed)
}

fn extract_music_id_from_canonical_song_url(text: &str) -> Option<u64> {
    extract_music_entity_id_from_canonical_url(text, "song")
}

fn extract_first_number(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(u8::is_ascii_digit)?;
    let len = bytes[start..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if len == 0 {
        return None;
    }
    text[start..start + len].parse::<u64>().ok()
}

fn parse_music_id_from_share_url(url: &str) -> Option<u64> {
    if !url.contains("song") {
        return None;
    }

    extract_music_id_from_canonical_song_url(url).or_else(|| extract_first_number(url))
}

fn parse_music_collection_target_from_url(url: &str) -> Option<MusicCollectionTarget> {
    extract_music_entity_id_from_canonical_url(url, "playlist")
        .map(MusicCollectionTarget::Playlist)
        .or_else(|| {
            extract_music_entity_id_from_canonical_url(url, "album")
                .map(MusicCollectionTarget::Album)
        })
}

/// Extract music ID from text
pub fn parse_music_id(text: &str) -> Option<u64> {
    if let Some(id) = parse_direct_numeric_id(text) {
        return Some(id);
    }

    if let Some(id) = extract_music_id_from_canonical_song_url(text) {
        return Some(id);
    }

    // Try to extract from URL
    if let Some(captures) = SONG_REGEX.captures(text)
        && let Some(id_str) = captures.get(1)
    {
        return id_str.as_str().parse().ok();
    }

    // Try to extract from share link
    if let Some(url_match) = SHARE_LINK_REGEX.find(text) {
        return parse_music_id_from_share_url(url_match.as_str());
    }

    None
}

/// Extract playlist/album target from text.
pub fn parse_music_collection_target(text: &str) -> Option<MusicCollectionTarget> {
    if let Some(target) = parse_music_collection_target_from_url(text) {
        return Some(target);
    }

    SHARE_LINK_REGEX
        .find(text)
        .and_then(|url_match| parse_music_collection_target_from_url(url_match.as_str()))
}

/// Extract the first URL from text
pub fn extract_first_url(text: &str) -> Option<String> {
    SHARE_LINK_REGEX
        .find(text)
        .map(|matched| matched.as_str().to_string())
}

/// Check if directory exists, create if not
pub fn ensure_dir(path: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(Path::new(path))
}

/// Clean filename for safe file operations
#[must_use]
pub fn clean_filename(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control() {
            continue;
        }
        match c {
            '/' | '\\' | '?' | '*' | ':' | '|' | '<' | '>' | '"' => result.push(' '),
            _ => result.push(c),
        }
    }
    let trimmed = result.trim();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else if trimmed.len() == result.len() {
        result
    } else {
        trimmed.to_string()
    }
}

/// Calculate MD5 hash of a file
pub fn verify_md5(file_path: &str, expected_md5: &str) -> anyhow::Result<bool> {
    use std::fs::File;
    use std::io::{BufReader, Read};

    let file = File::open(file_path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = md5::Context::new();
    let mut buffer = vec![0; 65536];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.consume(&buffer[..count]);
    }

    let result = hasher.finalize();
    let hash = format!("{result:x}");

    Ok(hash.eq_ignore_ascii_case(expected_md5))
}

/// Format file size in human readable format
#[must_use]
pub fn format_file_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = size as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_index])
}

/// Format duration in human readable format
#[must_use]
pub fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

#[must_use]
pub fn throughput_mbps(bytes: u64, duration: std::time::Duration) -> f64 {
    let duration_secs = duration.as_secs_f64();
    if duration_secs <= 0.0 {
        return 0.0;
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    mb / duration_secs
}

pub fn update_peak(counter: &std::sync::atomic::AtomicU32, value: u32) -> u32 {
    use std::sync::atomic::Ordering;

    let mut current = counter.load(Ordering::Relaxed);
    while value > current {
        match counter.compare_exchange(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return value,
            Err(latest) => current = latest,
        }
    }
    current
}

/// Check if an error is a timeout error by walking the error chain
pub fn is_timeout_error(error: &dyn std::error::Error) -> bool {
    let mut current: Option<&dyn std::error::Error> = Some(error);
    while let Some(err) = current {
        let message = err.to_string();
        if message.contains("timeout") || message.contains("deadline") {
            return true;
        }
        current = err.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        MusicCollectionTarget, clean_filename, ensure_dir, parse_music_collection_target,
        parse_music_id, throughput_mbps, update_peak,
    };

    #[test]
    fn parse_music_id_fast_path_detects_direct_numeric() {
        assert_eq!(super::parse_direct_numeric_id("123456"), Some(123_456));
        assert_eq!(super::parse_direct_numeric_id("  123456  "), Some(123_456));
        assert_eq!(super::parse_direct_numeric_id("abc123"), None);
    }

    #[test]
    fn parse_music_id_fast_path_handles_canonical_song_url() {
        assert_eq!(
            super::extract_music_id_from_canonical_song_url("https://music.163.com/song?id=424242"),
            Some(424_242)
        );
        assert_eq!(
            super::extract_music_id_from_canonical_song_url(
                "https://music.163.com/#/song?id=424242&foo=bar"
            ),
            Some(424_242)
        );
        assert_eq!(
            super::extract_music_id_from_canonical_song_url("https://example.com/song?id=424242"),
            None
        );
        assert_eq!(
            super::extract_music_id_from_canonical_song_url(
                "https://music.163.com/song?userid=123&id=424242"
            ),
            Some(424_242)
        );
    }

    #[test]
    fn throughput_mbps_calculates_expected_value() {
        let bytes = 10 * 1024 * 1024;
        let duration = Duration::from_secs(2);
        let value = throughput_mbps(bytes, duration);
        assert!((value - 5.0).abs() < 0.01);
    }

    #[test]
    fn update_peak_tracks_highest_value() {
        let counter = std::sync::atomic::AtomicU32::new(0);
        assert_eq!(update_peak(&counter, 1), 1);
        assert_eq!(update_peak(&counter, 2), 2);
        assert_eq!(update_peak(&counter, 2), 2);
        assert_eq!(update_peak(&counter, 1), 2);
    }

    #[test]
    fn parse_music_id_handles_direct_numeric_input() {
        assert_eq!(parse_music_id("123456"), Some(123_456));
        assert_eq!(parse_music_id("  123456  "), Some(123_456));
    }

    #[test]
    fn parse_music_collection_target_detects_playlist_with_optional_uct2() {
        let url = "https://music.163.com/playlist?id=17607381913&uct2=U2FsdGVkX18AISWPo4dHRIRF8KPygbcmfo67g4xh6S8=";
        assert_eq!(
            parse_music_collection_target(url),
            Some(MusicCollectionTarget::Playlist(17_607_381_913))
        );
    }

    #[test]
    fn parse_music_collection_target_detects_album_without_uct2() {
        let url = "https://music.163.com/album?id=121344602";
        assert_eq!(
            parse_music_collection_target(url),
            Some(MusicCollectionTarget::Album(121_344_602))
        );
    }

    #[test]
    fn parse_music_collection_target_rejects_song_link() {
        let url = "https://music.163.com/song?id=424242";
        assert_eq!(parse_music_collection_target(url), None);
    }

    #[test]
    fn ensure_dir_is_idempotent() {
        let temp_name = format!(
            "music163bot_utils_dir_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let temp_path = std::env::temp_dir().join(temp_name);
        let temp_path_str = temp_path.to_string_lossy().to_string();

        ensure_dir(&temp_path_str).expect("create dir first time");
        ensure_dir(&temp_path_str).expect("create dir second time");

        std::fs::remove_dir_all(&temp_path).expect("cleanup dir");
    }

    #[test]
    fn clean_filename_handles_all_invalid_chars() {
        let cleaned = clean_filename("/\\?*:|<>\"\n\t\r");
        assert_eq!(cleaned, "untitled");
    }

    #[test]
    fn clean_filename_preserves_valid_names() {
        assert_eq!(clean_filename("hello world.mp3"), "hello world.mp3");
        assert_eq!(clean_filename("  spaced  "), "spaced");
        assert_eq!(clean_filename("a/b"), "a b");
    }

    #[test]
    fn clean_filename_handles_unicode() {
        assert_eq!(clean_filename("你好世界.flac"), "你好世界.flac");
        assert_eq!(clean_filename("café.mp3"), "café.mp3");
    }

    #[test]
    fn is_timeout_error_detects_timeout_message() {
        let err = std::io::Error::new(std::io::ErrorKind::TimedOut, "connection timeout");
        assert!(super::is_timeout_error(&err));
    }

    #[test]
    fn is_timeout_error_rejects_non_timeout() {
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        assert!(!super::is_timeout_error(&err));
    }

    #[test]
    fn build_http_client_returns_client() {
        let client =
            super::build_http_client(reqwest::Client::builder()).expect("client should be built");
        let _ = client;
    }
}
