use super::{
    Arc, Bot, BotState, Message, ParseMode, ReplyParameters, ResponseResult,
    dispatch_parsed_music_target, parse_direct_music_target, parse_song_id_or_search_first_result,
    process_music, require_command_args_or_reply,
};
use crate::i18n;

use super::commands::chat_lang;

pub(super) async fn handle_help_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let lang = chat_lang(state, msg).await;
    let help_text = i18n::tr_with(&lang, "help_body", "bot_username", &state.bot_username);

    bot.send_message(msg.chat.id, help_text)
        .parse_mode(ParseMode::Html)
        .disable_link_preview(true)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    Ok(())
}

pub(super) async fn handle_music_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    let lang = chat_lang(state, msg).await;
    let Some(args) =
        require_command_args_or_reply(bot, msg, args, &i18n::tr(&lang, "music_need_id")).await?
    else {
        return Ok(());
    };

    if let Some(target) = parse_direct_music_target(&args) {
        return dispatch_parsed_music_target(bot, msg, state, target).await;
    }

    let Some(music_id) =
        parse_song_id_or_search_first_result(bot, msg, state, &args, "Music command search failed")
            .await?
    else {
        return Ok(());
    };

    process_music(bot, msg, state, music_id).await
}
