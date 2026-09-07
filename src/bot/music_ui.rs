//! Music share presentation: link targets, share keyboards, and search
//! result formatting — everything the bot attaches to a delivered song.

use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup};

use super::wiring::MusicLinkTarget;

pub(super) fn cached_music_link_target(program_id: Option<i64>, music_id: u64) -> MusicLinkTarget {
    if let Some(program_id) = program_id.and_then(|id| u64::try_from(id).ok()) {
        MusicLinkTarget::Program(program_id)
    } else {
        MusicLinkTarget::Song(music_id)
    }
}

pub(super) fn create_music_keyboard_for_target(
    lang: &crate::i18n::ChatLanguage,
    link_target: MusicLinkTarget,
    music_id: u64,
    song_name: &str,
    artists: &str,
) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    let primary_url_result = match link_target {
        MusicLinkTarget::Song(link_music_id) => {
            build_music_url("https://music.163.com", link_music_id)
        }
        MusicLinkTarget::Program(program_id) => {
            build_program_url("https://music.163.com", program_id)
        }
    };

    match primary_url_result {
        Ok(url) => {
            rows.push(vec![InlineKeyboardButton::url(
                format!("{song_name} - {artists}"),
                url,
            )]);
        }
        Err(e) => {
            tracing::warn!("Failed to build music URL for music_id {}: {}", music_id, e);
        }
    }

    let switch_inline_query_url = match link_target {
        MusicLinkTarget::Song(link_music_id) => {
            format!("https://music.163.com/song?id={link_music_id}")
        }
        MusicLinkTarget::Program(program_id) => {
            format!("https://music.163.com/program?id={program_id}")
        }
    };
    rows.push(vec![InlineKeyboardButton::switch_inline_query(
        crate::i18n::tr(lang, "share_button"),
        switch_inline_query_url,
    )]);

    InlineKeyboardMarkup::new(rows)
}

pub(super) fn build_music_url(
    base_url: &str,
    music_id: u64,
) -> std::result::Result<reqwest::Url, url::ParseError> {
    let mut url = reqwest::Url::parse(base_url)?;
    url.set_path("song");
    url.set_query(Some(&format!("id={music_id}")));
    Ok(url)
}

pub(super) fn build_program_url(
    base_url: &str,
    program_id: u64,
) -> std::result::Result<reqwest::Url, url::ParseError> {
    let mut url = reqwest::Url::parse(base_url)?;
    url.set_path("program");
    url.set_query(Some(&format!("id={program_id}")));
    Ok(url)
}

pub(super) fn append_search_result_line(
    results: &mut String,
    index: usize,
    song_name: &str,
    artists: &str,
) {
    use std::fmt::Write;

    if let Err(e) = writeln!(results, "{index}.「{song_name}」 - {artists}") {
        tracing::error!("Failed to format search result line: {}", e);
    }
}

pub(super) fn exceeds_batch_download_limit(
    track_count: usize,
    max_batch_download_tracks: u32,
) -> bool {
    track_count > max_batch_download_tracks.max(1) as usize
}

pub(super) fn rmcache_usage_prompt(lang: &crate::i18n::ChatLanguage) -> String {
    crate::i18n::tr(lang, "rmcache_usage")
}

pub(super) fn clearallcache_confirmation_prompt(lang: &crate::i18n::ChatLanguage) -> String {
    crate::i18n::tr(lang, "clearallcache_confirm_prompt")
}
