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
