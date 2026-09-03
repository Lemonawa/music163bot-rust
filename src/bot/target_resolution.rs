use super::{
    Arc, Bot, BotState, Message, MusicCollectionTarget, ResponseResult,
    parse_music_collection_target, parse_music_id, parse_music_program_id, process_music,
    process_music_collection, process_program, resolve_message, sanitize_sensitive_text,
    send_reply_text,
};
use crate::i18n;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParsedMusicTarget {
    Song(u64),
    Program(u64),
    Collection(MusicCollectionTarget),
}

pub(super) fn parse_direct_music_target(text: &str) -> Option<ParsedMusicTarget> {
    if let Some(music_id) = parse_music_id(text) {
        Some(ParsedMusicTarget::Song(music_id))
    } else if let Some(program_id) = parse_music_program_id(text) {
        Some(ParsedMusicTarget::Program(program_id))
    } else {
        parse_music_collection_target(text).map(ParsedMusicTarget::Collection)
    }
}

pub(super) async fn dispatch_parsed_music_target(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    target: ParsedMusicTarget,
) -> ResponseResult<()> {
    match target {
        ParsedMusicTarget::Song(music_id) => process_music(bot, msg, state, music_id).await,
        ParsedMusicTarget::Program(program_id) => {
            process_program(bot, msg, state, program_id).await
        }
        ParsedMusicTarget::Collection(collection_target) => {
            process_music_collection(bot, msg, state, collection_target).await
        }
    }
}

pub(super) async fn search_first_song_id_or_reply(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    keyword: &str,
    log_context: &str,
) -> ResponseResult<Option<u64>> {
    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    match state.music_api.search_songs(keyword, 1).await {
        Ok(songs) => {
            if let Some(song) = songs.first() {
                Ok(Some(song.id))
            } else {
                send_reply_text(bot, msg, i18n::tr(&lang, "search_no_results")).await?;
                Ok(None)
            }
        }
        Err(e) => {
            tracing::warn!(
                "{}: {}",
                log_context,
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            send_reply_text(bot, msg, i18n::tr(&lang, "search_failed")).await?;
            Ok(None)
        }
    }
}

pub(super) async fn parse_song_id_or_search_first_result(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    text: &str,
    log_context: &str,
) -> ResponseResult<Option<u64>> {
    if let Some(music_id) = parse_music_id(text) {
        Ok(Some(music_id))
    } else {
        search_first_song_id_or_reply(bot, msg, state, text, log_context).await
    }
}
