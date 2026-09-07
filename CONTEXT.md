# music163bot-rust

A Telegram bot that searches, downloads and shares NetEase Cloud Music, with per-chat caching and upload.

## Language

**Chat Language**:
The language the bot replies with in one specific chat, resolved per incoming update.
_Avoid_: locale, user language

**Language Override**:
The persistent per-chat setting written by `/lang`. Choosing Auto clears it and restores automatic detection.
_Avoid_: language preference (too vague — sounds like per-user)

**Default Language**:
The config-level fallback used when no override exists and auto-detection does not apply (e.g. group chats).
_Avoid_: base language, source language

**Served Size**:
What the eapi song-url endpoint would currently hand down for one song at a given level (bitrate, byte size, format) — ground truth, as opposed to catalog tier labels. Exposed via `MusicApi::get_served_sizes_batch`.
_Avoid_: available quality, catalog bitrate
