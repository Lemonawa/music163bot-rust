use rust_i18n::t;

use crate::config::Config;

// NOTE: `rust_i18n::i18n!` is invoked in `src/main.rs` at the crate root —
// the macro it generates (`crate::_rust_i18n_t`) only resolves from there.

/// A resolved locale for one chat, ready to be passed to `t!(..., locale = ...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLanguage {
    /// Locale code such as "zh" or "en".
    code: String,
}

impl ChatLanguage {
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

/// Source of a resolved language — used for logging and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageSource {
    /// Explicit `/lang` override persisted per chat.
    Override,
    /// Telegram `language_code` auto-detection (private chats only).
    AutoDetected,
    /// Config `default_language` fallback.
    Default,
}

/// Resolve the effective locale for a chat.
///
/// Order (per ADR-0001): explicit override > private-chat auto-detection
/// from the sender's Telegram `language_code` prefix > config default.
/// Group chats deliberately never auto-detect: members send with mixed
/// locales and replies would flip languages mid-conversation.
#[must_use]
pub fn resolve_chat_language(
    is_private_chat: bool,
    sender_language_code: Option<&str>,
    override_lang: Option<&str>,
    default_language: &str,
) -> (ChatLanguage, LanguageSource) {
    if let Some(lang) = override_lang
        && is_supported_locale(lang)
    {
        return (ChatLanguage::new(lang), LanguageSource::Override);
    }

    if is_private_chat
        && let Some(code) = sender_language_code
        && let Some(mapped) = map_language_code(code)
        && is_supported_locale(mapped)
    {
        return (ChatLanguage::new(mapped), LanguageSource::AutoDetected);
    }

    (ChatLanguage::new(default_language), LanguageSource::Default)
}

/// Map a Telegram `language_code` (e.g. "zh-Hans", "en-US") onto a supported
/// locale using the primary subtag before "-". Returns `None` when unknown.
#[must_use]
pub fn map_language_code(code: &str) -> Option<&'static str> {
    let primary = code.split('-').next().unwrap_or("").to_ascii_lowercase();
    match primary.as_str() {
        "zh" => Some("zh"),
        "en" => Some("en"),
        _ => None,
    }
}

/// Whether `locale` is one of the compiled-in locales (the extension
/// interface: adding a YAML file under `locales/` extends this set).
#[must_use]
pub fn is_supported_locale(locale: &str) -> bool {
    crate::_rust_i18n_available_locales().contains(&locale)
}

/// Validate a user-supplied `/lang` argument ("en", "auto", ...).
/// Returns `Ok(Some(locale))` for a supported locale, `Ok(None)` for "auto"
/// (clear the override), or `Err(())` for unknown codes.
///
/// This is a pure parse — the unit error carries no information worth
/// documenting, hence the allow.
#[allow(clippy::result_unit_err, clippy::missing_errors_doc)]
pub fn parse_lang_argument(arg: &str) -> Result<Option<&'static str>, ()> {
    let normalized = arg.trim().to_ascii_lowercase();
    if normalized == "auto" {
        return Ok(None);
    }
    for locale in crate::_rust_i18n_available_locales() {
        if locale == normalized {
            return Ok(Some(locale));
        }
    }
    Err(())
}

/// Convenience wrapper translating `key` for `lang`.
#[must_use]
pub fn tr(lang: &ChatLanguage, key: &str) -> String {
    t!(key, locale = lang.code()).to_string()
}

/// The source-of-truth locale, used where no chat context exists (file
/// metadata fallbacks).
#[must_use]
pub fn default_lang_zh() -> ChatLanguage {
    ChatLanguage::new("zh")
}

/// Translate with one `%{name}` placeholder.
#[must_use]
pub fn tr_with(
    lang: &ChatLanguage,
    key: &str,
    arg_name: &str,
    arg_value: &dyn std::fmt::Display,
) -> String {
    tr_many(lang, key, &[(arg_name, arg_value)])
}

/// Translate with several `%{name}` placeholders supplied as `(name, value)` pairs.
/// Values are pre-rendered to `String` so the future stays `Send` across awaits.
#[must_use]
pub fn tr_many(lang: &ChatLanguage, key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let rendered: Vec<(&str, String)> = args
        .iter()
        .map(|(name, value)| (*name, value.to_string()))
        .collect();
    tr_many_strings(lang, key, &rendered)
}

/// [`tr_many`] with pre-rendered argument values.
#[must_use]
pub fn tr_many_strings(lang: &ChatLanguage, key: &str, args: &[(&str, String)]) -> String {
    let mut translated = t!(key, locale = lang.code()).to_string();
    for (name, value) in args {
        let pattern = format!("%{{{name}}}");
        if translated.contains(&pattern) {
            translated = translated.replace(&pattern, value);
        }
    }
    translated
}

/// Resolve the language for an incoming message, consulting the shared
/// language cache then the database.
pub async fn chat_language_for_message(
    database: &crate::database::Database,
    chat_languages: &dashmap::DashMap<i64, String>,
    msg: &crate::telegram::Message,
    default_language: &str,
) -> ChatLanguage {
    let is_private = msg.chat.type_ == "private";
    let sender_code = msg.from.as_ref().and_then(|u| u.language_code.as_deref());
    let override_lang = match chat_languages.get(&msg.chat.id.0) {
        Some(entry) => Some(entry.value().clone()),
        None => match database
            .get_chat_language(msg.chat.id.0)
            .await
            .ok()
            .flatten()
        {
            Some(lang) => {
                chat_languages.insert(msg.chat.id.0, lang.clone());
                Some(lang)
            }
            None => None,
        },
    };
    resolve_chat_language(
        is_private,
        sender_code,
        override_lang.as_deref(),
        default_language,
    )
    .0
}

/// Default language from config (used at startup before a chat is known).
#[must_use]
pub fn default_language(config: &Config) -> ChatLanguage {
    ChatLanguage::new(config.default_language.clone())
}

/// Resolve the language for an inline query. Inline queries behave like a
/// private chat with the querent (their user id == the private chat id), so
/// their `/lang` override and their Telegram `language_code` both apply.
pub async fn resolve_inline_language(
    database: &crate::database::Database,
    chat_languages: &dashmap::DashMap<i64, String>,
    from: &crate::telegram::User,
    default_language: &str,
) -> ChatLanguage {
    let override_lang = match chat_languages.get(&from.id) {
        Some(entry) => Some(entry.value().clone()),
        None => match database.get_chat_language(from.id).await.ok().flatten() {
            Some(lang) => {
                chat_languages.insert(from.id, lang.clone());
                Some(lang)
            }
            None => None,
        },
    };
    resolve_chat_language(
        true,
        from.language_code.as_deref(),
        override_lang.as_deref(),
        default_language,
    )
    .0
}

#[cfg(test)]
mod tests;
