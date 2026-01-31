use std::path::Path;

use regex::Regex;

/// Global regex patterns for URL parsing
static SONG_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"music\.163\.com/.*?song.*?[?&]id=(\d+)").unwrap());

static SHARE_LINK_REGEX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(http|https)://[\w\-_]+(\.[\w\-_]+)+([\w\-.,@?^=%&:/~+#]*[\w\-@?^=%&/~+#])?")
        .unwrap()
});

static NUMBER_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\d+").unwrap());

/// Extract music ID from text
pub fn parse_music_id(text: &str) -> Option<u64> {
    // 优化：直接对原始 text 使用正则，避免创建新 String
    // SONG_REGEX 和 SHARE_LINK_REGEX 都能正确处理包含空白的字符串

    // Try to extract from URL
    if let Some(captures) = SONG_REGEX.captures(text)
        && let Some(id_str) = captures.get(1)
    {
        return id_str.as_str().parse().ok();
    }

    // Try to extract from share link
    if let Some(url_match) = SHARE_LINK_REGEX.find(text)
        && url_match.as_str().contains("song")
        && let Some(id_match) = NUMBER_REGEX.find(url_match.as_str())
    {
        return id_match.as_str().parse().ok();
    }

    // Try to parse as direct number (only if the entire text is a number)
    // 去除空白后再检查是否为纯数字
    let trimmed = text.trim();
    if trimmed.parse::<u64>().is_ok() {
        return trimmed.parse().ok();
    }
    None
}

/// Extract the first URL from text
pub fn extract_first_url(text: &str) -> Option<String> {
    SHARE_LINK_REGEX
        .find(text)
        .map(|matched| matched.as_str().to_string())
}

/// Check if directory exists, create if not
pub fn ensure_dir(path: &str) -> std::io::Result<()> {
    let path = Path::new(path);
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Clean filename for safe file operations
#[must_use]
pub fn clean_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | '?' | '*' | ':' | '|' | '<' | '>' | '"' => ' ',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Calculate MD5 hash of a file
pub fn verify_md5(file_path: &str, expected_md5: &str) -> anyhow::Result<bool> {
    use std::fs::File;
    use std::io::{BufReader, Read};

    let file = File::open(file_path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = md5::Context::new();
    let mut buffer = [0; 8192];

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

/// Check if an error is a timeout error
pub fn is_timeout_error(error: &dyn std::error::Error) -> bool {
    error.to_string().contains("timeout") || error.to_string().contains("deadline")
}
