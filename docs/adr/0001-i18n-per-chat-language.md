# i18n: per-chat language via rust-i18n

We add English alongside Chinese, using `rust-i18n` with compile-time embedded YAML in `locales/` (adding a language = adding one YAML file, no code changes — the `/lang` keyboard is generated from `available_locales!()`). Language is resolved per update as: explicit `/lang` override (persisted per chat in SQLite) > private-chat auto-detection from Telegram `language_code` prefix > config `default_language`.

Group chats deliberately do not follow the sender's `language_code`: every member sends with a different locale, so replies would flip languages mid-conversation; groups get the config default until an admin overrides. `/lang` in groups is gated on Telegram group admin (new `getChatMember` in our hand-rolled API); in private chats anyone may set it. Locale is passed explicitly to `t!(..., locale = ...)` per handler rather than via global `set_locale`, because concurrent tokio tasks would race the global. Missing keys fall back to zh (source of truth).

## Considered Options

- Fluent (fluent-rs): full ICU plural/gender support — overkill for this string set.
- Hand-written static tables: zero deps, but adding a language requires code changes, killing the extension interface.
- Global runtime locale: rejected, see above.
