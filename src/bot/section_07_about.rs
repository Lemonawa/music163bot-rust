const BUILD_GIT_COMMIT: &str = match option_env!("BUILD_GIT_COMMIT") {
    Some(value) => value,
    None => "unknown",
};

fn build_about_text() -> String {
    format!(
        r"🎵 Music163bot-Rust v{} ({})

一个用来下载/分享/搜索网易云歌曲的 Telegram Bot

特性：
• 🔗 分享链接嗅探
• 🎵 歌曲搜索与下载
• 💾 智能缓存系统
• 🚀 智能存储 (v1.1.0+)
• 🎤 歌词获取
• 📊 使用统计

技术栈：
• 🦀 Rust + Teloxide
• 🔧 高并发处理
• 📦 轻量级部署

源码：GitHub | 原版：Music163bot-Go",
        env!("CARGO_PKG_VERSION"),
        BUILD_GIT_COMMIT
    )
}

async fn handle_about_command(
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
