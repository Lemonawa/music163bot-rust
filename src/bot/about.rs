use super::{Arc, Bot, BotState, Message, ReplyParameters, ResponseResult, resolve_message};
use crate::i18n;

pub(super) const BUILD_GIT_COMMIT: &str = match option_env!("BUILD_GIT_COMMIT") {
    Some(value) => value,
    None => "unknown",
};

#[must_use]
pub(super) fn build_about_text(lang: &crate::i18n::ChatLanguage) -> String {
    i18n::tr_many(
        lang,
        "about_body",
        &[
            ("version", &env!("CARGO_PKG_VERSION")),
            ("commit", &BUILD_GIT_COMMIT),
        ],
    )
}

pub(super) async fn handle_about_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
) -> ResponseResult<()> {
    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    let about_text = build_about_text(&lang);

    bot.send_message(msg.chat.id, about_text)
        .reply_parameters(ReplyParameters::new(msg.id))
        .disable_link_preview(true)
        .await?;

    Ok(())
}
