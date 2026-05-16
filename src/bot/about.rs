use super::{Arc, Bot, BotState, Message, ReplyParameters, ResponseResult};

pub(super) const BUILD_GIT_COMMIT: &str = match option_env!("BUILD_GIT_COMMIT") {
    Some(value) => value,
    None => "unknown",
};

pub(super) fn build_about_text() -> String {
    format!(
        r"🎵 Music163bot-Rust v{} ({})

网易云音乐 Telegram Bot，支持链接解析、搜索下载、歌词获取。

Rust 编写，轻量高并发。

源码：GitHub | 原版：Music163bot-Go",
        env!("CARGO_PKG_VERSION"),
        BUILD_GIT_COMMIT
    )
}

pub(super) async fn handle_about_command(
    bot: &Bot,
    msg: &Message,
    _state: &Arc<BotState>,
) -> ResponseResult<()> {
    let about_text = build_about_text();

    bot.send_message(msg.chat.id, about_text)
        .reply_parameters(ReplyParameters::new(msg.id))
        .disable_link_preview(true)
        .await?;

    Ok(())
}
