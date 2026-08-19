use crate::i18n::ChatLanguage;

fn keyboard_buttons(
    markup: &crate::telegram::InlineKeyboardMarkup,
) -> Vec<&crate::telegram::InlineKeyboardButton> {
    markup.inline_keyboard.iter().flatten().collect()
}

#[test]
fn lang_keyboard_contains_every_compiled_locale_plus_auto() {
    let markup = super::build_lang_keyboard();
    let buttons = keyboard_buttons(&markup);

    // One button per compiled locale (zh, en) + the Auto button.
    assert_eq!(
        buttons.len(),
        crate::_rust_i18n_available_locales().len() + 1
    );

    let texts: Vec<&str> = buttons.iter().map(|b| b.text.as_str()).collect();
    assert!(texts.contains(&"ZH"));
    assert!(texts.contains(&"EN"));

    let callback_datas: Vec<&str> = buttons
        .iter()
        .filter_map(|b| b.callback_data.as_deref())
        .collect();
    assert!(callback_datas.contains(&"lang:set:zh"));
    assert!(callback_datas.contains(&"lang:set:en"));
    assert!(callback_datas.contains(&"lang:set:auto"));
}

#[test]
fn lang_set_message_substitutes_locale() {
    let text = crate::i18n::tr_with(&ChatLanguage::new("en"), "lang_set", "lang", &"en");
    assert!(text.contains("en"));
    assert!(!text.contains("%{lang}"));
}

#[test]
fn bot_commands_for_locale_covers_every_registered_command_in_order() {
    let commands = super::bot_commands_for_locale(&ChatLanguage::new("zh"));
    let names: Vec<&str> = commands.iter().map(|c| c.command.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "start",
            "music",
            "netease",
            "search",
            "lyric",
            "lang",
            "status",
            "about",
            "rmcache",
            "clearallcache",
            "help"
        ]
    );
    assert!(commands.iter().all(|c| !c.description.is_empty()));
}

#[test]
fn bot_commands_descriptions_localize() {
    let zh = super::bot_commands_for_locale(&ChatLanguage::new("zh"));
    let en = super::bot_commands_for_locale(&ChatLanguage::new("en"));

    let zh_lang = zh.iter().find(|c| c.command == "lang").expect("lang cmd");
    let en_lang = en.iter().find(|c| c.command == "lang").expect("lang cmd");
    assert_eq!(zh_lang.description, "设置回复语言");
    assert_eq!(en_lang.description, "Set reply language");
}

#[test]
fn bot_command_serializes_command_and_description() {
    let json = serde_json::to_string(&crate::telegram::BotCommand::new("lang", "Set language"))
        .expect("serialize");
    assert_eq!(json, r#"{"command":"lang","description":"Set language"}"#);
}
