use super::*;

pub(super) async fn handle_help_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let help_text = format!(
        "📖 <b>使用帮助</b>\n\n\
        1️⃣ <b>直接解析</b>\n\
        发送网易云音乐链接给机器人，例如：\n\
        <code>https://music.163.com/song?id=12345</code>\n\
        <code>https://music.163.com/playlist?id=12345</code>\n\
        <code>https://music.163.com/album?id=12345</code>\n\
        <code>https://music.163.com/program?id=12345</code>\n\
        <code>https://music.163.com/djradio?id=12345</code>\n\n\
        2️⃣ <b>搜索音乐</b>\n\
        使用 <code>/search &lt;关键词&gt;</code> 在私聊中搜索。\n\n\
        3️⃣ <b>Inline 搜索</b>\n\
        在任何对话框输入 <code>@{} &lt;关键词&gt;</code> 即可快速搜索并分享音乐。\n\n\
        4️⃣ <b>获取歌词</b>\n\
        使用 <code>/lyric &lt;关键词或ID&gt;</code> 获取歌词。\n\n\
        5️⃣ <b>更多命令</b>\n\
        • <code>/status</code> - 查看系统状态（仅管理员）\n\
        • <code>/about</code> - 关于机器人\n\n\
        💬 <b>项目主页：</b> <a href=\"https://github.com/Lemonawa/music163bot-rust\">GitHub</a>",
        state.bot_username
    );

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
    let Some(args) =
        require_command_args_or_reply(bot, msg, args, "请输入歌曲ID或歌曲关键词").await?
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
