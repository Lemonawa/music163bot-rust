//! Update intake: deciding what kind of work a message represents and what
//! gets logged along the way.

pub(super) const MESSAGE_TASK_LINK_HINTS: [&str; 3] = ["music.163.com", "163cn.tv", "163cn.link"];

pub(super) fn contains_music_link_hint(text: &str) -> bool {
    MESSAGE_TASK_LINK_HINTS
        .iter()
        .any(|hint| text.contains(hint))
}

pub(super) fn is_command_text(text: &str) -> bool {
    text.starts_with('/')
}

pub(super) fn should_spawn_message_task(text: &str) -> bool {
    is_command_text(text) || contains_music_link_hint(text)
}

pub(super) fn should_log_command(command: &str) -> bool {
    matches!(
        command,
        "music" | "netease" | "search" | "rmcache" | "clearallcache" | "lang"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageTaskRoute {
    Command,
    MusicLink,
}

pub(super) fn classify_message_task(text: &str) -> Option<MessageTaskRoute> {
    if is_command_text(text) {
        Some(MessageTaskRoute::Command)
    } else if contains_music_link_hint(text) {
        Some(MessageTaskRoute::MusicLink)
    } else {
        None
    }
}

pub(super) fn is_clearallcache_confirm(args: Option<&str>) -> bool {
    matches!(args.map(str::trim), Some("confirm"))
}

pub(super) fn is_official_telegram_api(api_url: &str) -> bool {
    let trimmed = api_url.trim();
    if trimmed.is_empty() {
        return true;
    }

    let normalized = trimmed.trim_end_matches('/').trim_end_matches("/bot");
    let Ok(url) = reqwest::Url::parse(if normalized.is_empty() {
        trimmed
    } else {
        normalized
    }) else {
        return false;
    };

    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.telegram.org"))
}
