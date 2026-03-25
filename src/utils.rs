use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::{BotError, Result};

static TELEGRAM_BOT_TOKEN_PATH_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"/bot[^/\s)'"?&]+:[^/\s)'"?&]+/?"#)
        .expect("telegram bot token regex should be valid")
});

static URL_CREDENTIALS_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(https?://)[^/\s@]+@").expect("url credentials regex should be valid")
});

static AUTHORIZATION_HEADER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(Authorization:\s*)[^\r\n]+")
        .expect("authorization header regex should be valid")
});

static COOKIE_HEADER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(Cookie:\s*)[^\r\n]+").expect("cookie header regex should be valid")
});

static SET_COOKIE_HEADER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(Set-Cookie:\s*)[^\r\n]+").expect("set-cookie header regex should be valid")
});

static MUSIC_U_COOKIE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bMUSIC_U=[^;\s)]+").expect("music_u cookie regex should be valid")
});

static SECRET_QUERY_PARAM_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)([?&](?:access_token|token|auth|authtoken|signature|sig|api_key|apikey|key|secret|client_secret|password|pass|x-amz-signature|x-amz-credential|x-amz-security-token|x-goog-signature|x-goog-credential|googleaccessid|awsaccesskeyid|wssecret|uct2|credential)=)[^&#\s)]+",
    )
    .expect("secret query parameter regex should be valid")
});

static RETRY_AFTER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bretry after\s+(\d+)(?:s|秒)?\b").expect("retry-after regex should be valid")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicCollectionTarget {
    Playlist(u64),
    Album(u64),
    DjRadio(u64),
}

/// Build a reqwest HTTP client from a builder, logging and mapping errors on failure.
pub fn build_http_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    builder.build().map_err(|e| {
        let sanitized = sanitize_sensitive_text(&e.to_string());
        tracing::error!("Failed to build HTTP client: {}", sanitized);
        BotError::HttpClientBuild(sanitized)
    })
}

#[must_use]
pub fn sanitize_sensitive_text(text: &str) -> String {
    let sanitized =
        TELEGRAM_BOT_TOKEN_PATH_REGEX.replace_all(text, |caps: &regex::Captures<'_>| {
            if caps[0].ends_with('/') {
                "/bot<redacted>/"
            } else {
                "/bot<redacted>"
            }
        });
    let sanitized = URL_CREDENTIALS_REGEX.replace_all(&sanitized, "$1<redacted>@");
    let sanitized = AUTHORIZATION_HEADER_REGEX.replace_all(&sanitized, "$1<redacted>");
    let sanitized = COOKIE_HEADER_REGEX.replace_all(&sanitized, "$1<redacted>");
    let sanitized = SET_COOKIE_HEADER_REGEX.replace_all(&sanitized, "$1<redacted>");
    let sanitized = MUSIC_U_COOKIE_REGEX.replace_all(&sanitized, "MUSIC_U=<redacted>");
    let sanitized = SECRET_QUERY_PARAM_REGEX.replace_all(&sanitized, "$1<redacted>");
    sanitized.into_owned()
}

#[must_use]
pub fn extract_retry_after_seconds(text: &str) -> Option<u64> {
    let captures = RETRY_AFTER_REGEX.captures(text)?;
    captures.get(1)?.as_str().parse::<u64>().ok()
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

fn extract_music_program_id_from_canonical_url(text: &str) -> Option<u64> {
    extract_music_entity_id_from_canonical_url(text, "program")
        .or_else(|| extract_music_entity_id_from_canonical_url(text, "dj"))
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

fn parse_music_program_id_from_share_url(url: &str) -> Option<u64> {
    extract_music_program_id_from_canonical_url(url)
}

fn parse_music_collection_target_from_url(url: &str) -> Option<MusicCollectionTarget> {
    extract_music_entity_id_from_canonical_url(url, "playlist")
        .map(MusicCollectionTarget::Playlist)
        .or_else(|| {
            extract_music_entity_id_from_canonical_url(url, "album")
                .map(MusicCollectionTarget::Album)
        })
        .or_else(|| {
            extract_music_entity_id_from_canonical_url(url, "djradio")
                .map(MusicCollectionTarget::DjRadio)
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

/// Extract program ID (program/dj) from text.
pub fn parse_music_program_id(text: &str) -> Option<u64> {
    if let Some(id) = extract_music_program_id_from_canonical_url(text) {
        return Some(id);
    }

    SHARE_LINK_REGEX
        .find(text)
        .and_then(|url_match| parse_music_program_id_from_share_url(url_match.as_str()))
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

/// Extract the first trusted NetEase share URL from text.
pub fn extract_first_trusted_music_share_url(text: &str) -> Option<String> {
    SHARE_LINK_REGEX.find_iter(text).find_map(|matched| {
        let url = matched.as_str();
        is_trusted_music_share_url(url).then(|| url.to_string())
    })
}

/// Return whether URL host belongs to trusted NetEase share domains.
#[must_use]
pub fn is_trusted_music_share_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };

    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }

    let Some(host) = parsed.host_str() else {
        return false;
    };

    matches!(host, "music.163.com" | "163cn.tv" | "163cn.link")
        || host.ends_with(".music.163.com")
        || host.ends_with(".163cn.tv")
        || host.ends_with(".163cn.link")
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
mod tests;
