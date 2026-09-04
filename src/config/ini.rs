//! Flat `section.key → value` INI text parser shared by the bot and the
//! `refresh_hires` binary (included via `#[path]` there, so this file must
//! stay dependency-free: `std` only).

use std::collections::HashMap;

/// Parse INI text into a flat `section.key → value` map.
///
/// Section and key names fold to lowercase; `#` and `;` lines are comments.
/// Keys outside any section keep their bare name. Later entries win on
/// duplicate keys.
#[must_use]
pub(crate) fn parse_ini_text(content: &str) -> HashMap<String, String> {
    let mut config_map = HashMap::new();
    let mut current_section = String::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if line.starts_with('[') {
            current_section = line
                .strip_prefix('[')
                .and_then(|section| section.strip_suffix(']'))
                .unwrap_or("")
                .to_lowercase();
            continue;
        }

        if let Some((raw_key, raw_value)) = line.split_once('=') {
            let key = raw_key.trim().to_lowercase();
            let value = raw_value.trim().to_string();

            let full_key = if current_section.is_empty() {
                key
            } else {
                format!("{current_section}.{key}")
            };

            config_map.insert(full_key, value);
        }
    }

    config_map
}
