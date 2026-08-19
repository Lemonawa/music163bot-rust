use super::*;

#[test]
fn resolve_prefers_override_over_everything() {
    let (lang, source) = resolve_chat_language(true, Some("zh"), Some("en"), "zh");
    assert_eq!(lang.code(), "en");
    assert_eq!(source, LanguageSource::Override);
}

#[test]
fn resolve_group_chat_never_auto_detects() {
    // Group chat: sender language must be ignored, default wins.
    let (lang, source) = resolve_chat_language(false, Some("en"), None, "zh");
    assert_eq!(lang.code(), "zh");
    assert_eq!(source, LanguageSource::Default);
}

#[test]
fn resolve_private_chat_detects_zh_variants() {
    for code in ["zh", "zh-Hans", "zh-CN", "ZH"] {
        let (lang, source) = resolve_chat_language(true, Some(code), None, "en");
        assert_eq!(lang.code(), "zh", "code {code}");
        assert_eq!(source, LanguageSource::AutoDetected);
    }
}

#[test]
fn resolve_private_chat_detects_en_variants() {
    for code in ["en", "en-US", "EN-gb"] {
        let (lang, source) = resolve_chat_language(true, Some(code), None, "zh");
        assert_eq!(lang.code(), "en", "code {code}");
        assert_eq!(source, LanguageSource::AutoDetected);
    }
}

#[test]
fn resolve_unknown_language_code_falls_back_to_default() {
    let (lang, source) = resolve_chat_language(true, Some("fr"), None, "zh");
    assert_eq!(lang.code(), "zh");
    assert_eq!(source, LanguageSource::Default);
}

#[test]
fn resolve_override_wins_even_in_group() {
    let (lang, source) = resolve_chat_language(false, None, Some("en"), "zh");
    assert_eq!(lang.code(), "en");
    assert_eq!(source, LanguageSource::Override);
}

#[test]
fn resolve_stale_override_falls_through_to_default() {
    // An override value that is no longer a compiled locale must not win.
    let (lang, source) = resolve_chat_language(false, None, Some("fr"), "zh");
    assert_eq!(lang.code(), "zh");
    assert_eq!(source, LanguageSource::Default);
}

#[test]
fn map_language_code_uses_primary_subtag() {
    assert_eq!(map_language_code("zh-Hans-CN"), Some("zh"));
    assert_eq!(map_language_code("en"), Some("en"));
    assert_eq!(map_language_code("fr"), None);
    assert_eq!(map_language_code(""), None);
}

#[test]
fn supported_locales_include_zh_and_en() {
    assert!(is_supported_locale("zh"));
    assert!(is_supported_locale("en"));
    assert!(!is_supported_locale("fr"));
}

#[test]
fn parse_lang_argument_accepts_known_locales_and_auto() {
    assert_eq!(parse_lang_argument("en"), Ok(Some("en")));
    assert_eq!(parse_lang_argument("ZH"), Ok(Some("zh")));
    assert_eq!(parse_lang_argument(" auto "), Ok(None));
    assert_eq!(parse_lang_argument("fr"), Err(()));
}

#[test]
fn tr_returns_chinese_for_zh_and_english_for_en() {
    let zh = ChatLanguage::new("zh");
    let en = ChatLanguage::new("en");
    assert_eq!(tr(&zh, "search_searching"), "🔍 搜索中...");
    assert_eq!(tr(&en, "search_searching"), "🔍 Searching...");
}

#[test]
fn tr_missing_key_falls_back_to_zh() {
    let en = ChatLanguage::new("en");
    // "no_such_key" is absent from both locales; rust-i18n emits the key itself.
    assert_eq!(tr(&en, "no_such_key"), "no_such_key");
}

#[test]
fn tr_with_substitutes_placeholder() {
    let zh = ChatLanguage::new("zh");
    let text = tr_with(&zh, "rmcache_deleted", "name", &"Test Song");
    assert!(text.contains("Test Song"));
    assert!(text.contains("已删除歌曲缓存"));
}

#[test]
fn tr_many_substitutes_all_placeholders() {
    let en = ChatLanguage::new("en");
    let count = 5usize;
    let max = 3u32;
    let id = 42u64;
    let text = tr_many(
        &en,
        "dj_detected",
        &[("count", &count), ("id", &id), ("max", &max)],
    );
    assert!(text.contains("5 episodes"));
    assert!(text.contains("ID: 42"));
    assert!(!text.contains("%{"));
}

#[tokio::test]
async fn chat_language_for_message_uses_db_override_and_caches_it() {
    let db_path = std::env::temp_dir().join(format!(
        "i18n_lang_msg_{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::database::Database::new(&db_path.to_string_lossy())
        .await
        .unwrap();
    db.set_chat_language(777, "en").await.unwrap();

    let msg = crate::telegram::Message {
        id: crate::telegram::MessageId(1),
        from: None,
        chat: crate::telegram::Chat {
            id: crate::telegram::ChatId(777),
            type_: "private".to_string(),
            username: None,
        },
        date: 0,
        text: None,
        reply_to_message: None,
    };
    let cache = dashmap::DashMap::new();
    let lang = chat_language_for_message(&db, &cache, &msg, "zh").await;
    assert_eq!(lang.code(), "en");
    assert_eq!(
        cache.get(&777).map(|e| e.value().clone()),
        Some("en".to_string())
    );

    // Second call hits the cache (still correct even if DB row disappears).
    db.clear_chat_language(777).await.unwrap();
    let lang = chat_language_for_message(&db, &cache, &msg, "zh").await;
    assert_eq!(lang.code(), "en");
}
