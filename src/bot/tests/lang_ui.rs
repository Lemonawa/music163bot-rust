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

fn temp_db_path(prefix: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "{prefix}_{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .to_string_lossy()
        .into_owned()
}

fn test_message(chat_id: i64, chat_type: &str) -> crate::telegram::Message {
    crate::telegram::Message {
        id: crate::telegram::MessageId(1),
        from: None,
        chat: crate::telegram::Chat {
            id: crate::telegram::ChatId(chat_id),
            type_: chat_type.to_string(),
            username: None,
        },
        date: 0,
        text: None,
        reply_to_message: None,
    }
}

#[tokio::test]
async fn resolve_message_uses_db_override_and_caches_it() {
    let db = crate::database::Database::new(&temp_db_path("lang_msg"))
        .await
        .unwrap();
    db.set_chat_language(777, "en").await.unwrap();

    let msg = test_message(777, "private");
    let cache = dashmap::DashMap::new();
    let lang = super::resolve_message(&db, &cache, "zh", &msg).await;
    assert_eq!(lang.code(), "en");
    assert_eq!(
        cache.get(&777).map(|e| e.value().clone()),
        Some("en".to_string())
    );

    // Second call hits the cache (still correct even if DB row disappears).
    db.clear_chat_language(777).await.unwrap();
    let lang = super::resolve_message(&db, &cache, "zh", &msg).await;
    assert_eq!(lang.code(), "en");
}

#[tokio::test]
async fn resolve_inline_prefers_override_then_detects_then_defaults() {
    let db = crate::database::Database::new(&temp_db_path("lang_inline"))
        .await
        .unwrap();
    db.set_chat_language(888, "en").await.unwrap();

    let user = crate::telegram::User {
        id: 888,
        first_name: "test".to_string(),
        username: None,
        language_code: Some("zh-CN".to_string()),
    };
    let cache = dashmap::DashMap::new();

    // Override wins over auto-detection.
    let lang = super::resolve_inline(&db, &cache, "zh", &user).await;
    assert_eq!(lang.code(), "en");

    // Without an override, the Telegram language_code applies.
    db.clear_chat_language(888).await.unwrap();
    cache.remove(&888);
    let lang = super::resolve_inline(&db, &cache, "en", &user).await;
    assert_eq!(lang.code(), "zh");
}
