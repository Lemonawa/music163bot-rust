use super::{
    Arc, Bot, BotState, DownloadAndSendParams, FileId, InputFile, Message, MusicLinkTarget,
    PERF_STAGE_CACHE_LOOKUP, PERF_STAGE_E2E_TOTAL, PERF_STAGE_SELECT_URL,
    PERF_STAGE_SINGLEFLIGHT_WAIT, PerfTraceContext, ProgramMainTrack, ReplyParameters,
    ResponseResult, acquire_download_leader, build_caption, build_perf_trace_context,
    cached_music_link_target, create_music_keyboard_for_target, delete_status_message_resilient,
    download_and_send_music, edit_status_message_resilient, extract_retry_after_seconds,
    format_artists, format_error_chain, log_perf, resolve_message, sanitize_sensitive_text,
    send_reply_text, u64_to_i64_saturating,
};
use crate::i18n;

pub(super) async fn try_send_cached_song(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
    preferred_program_id: Option<u64>,
) -> ResponseResult<bool> {
    let music_id_i64 = u64_to_i64_saturating(music_id);

    let cached_song = match state.database.get_song_by_music_id(music_id_i64).await {
        Ok(Some(song)) => song,
        Ok(None) => return Ok(false),
        Err(e) => {
            tracing::warn!(
                "Database error looking up music_id {}: {}",
                music_id,
                sanitize_sensitive_text(&format_error_chain(&e))
            );
            return Ok(false);
        }
    };

    let Some(file_id) = &cached_song.file_id else {
        return Ok(false);
    };

    if cached_song.music_size <= 1024 {
        tracing::warn!(
            "Removing invalid cached file for music_id {}: size {} bytes",
            music_id,
            cached_song.music_size
        );
        if let Err(e) = state.database.delete_song_by_music_id(music_id_i64).await {
            tracing::warn!(
                "Failed to delete invalid cache for music_id {}: {}",
                music_id,
                sanitize_sensitive_text(&format_error_chain(&e))
            );
        }
        return Ok(false);
    }

    let bitrate = if cached_song.bit_rate > 0 {
        cached_song.bit_rate
    } else {
        let duration_sec = cached_song.duration.max(1);
        8 * cached_song.music_size / duration_sec
    };

    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    let caption = build_caption(
        &lang,
        &cached_song.song_name,
        &cached_song.song_artists,
        &cached_song.song_album,
        &cached_song.file_ext,
        cached_song.music_size,
        bitrate,
        &state.bot_username,
    );

    let preferred_program_id = preferred_program_id.and_then(|id| i64::try_from(id).ok());
    let link_target =
        cached_music_link_target(preferred_program_id.or(cached_song.program_id), music_id);
    let keyboard = create_music_keyboard_for_target(
        &lang,
        link_target,
        music_id,
        &cached_song.song_name,
        &cached_song.song_artists,
    );

    match bot
        .send_audio(msg.chat.id, InputFile::file_id(FileId(file_id.clone())))
        .caption(caption)
        .reply_markup(keyboard)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("invalid remote file identifier") {
                tracing::warn!(
                    "Cached file_id invalid for music_id {}, deleting cache and re-downloading: {}",
                    music_id,
                    e
                );
                if let Err(e) = state.database.delete_song_by_music_id(music_id_i64).await {
                    tracing::warn!(
                        "Failed to delete stale file_id cache for music_id {}: {}",
                        music_id,
                        sanitize_sensitive_text(&format_error_chain(&e))
                    );
                }
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

pub(super) async fn process_program(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    program_id: u64,
) -> ResponseResult<()> {
    let program = match state.music_api.get_program_main_track(program_id).await {
        Ok(program) => program,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch program detail for {program_id}: {}",
                sanitize_sensitive_text(&format_error_chain(&e))
            );
            let lang = resolve_message(
                &state.database,
                &state.chat_languages,
                &state.config.default_language,
                msg,
            )
            .await;
            send_reply_text(bot, msg, i18n::tr(&lang, "voice_detail_failed")).await?;
            return Ok(());
        }
    };

    process_music_with_context(bot, msg, state, program.main_track_id, Some(program)).await
}

pub(super) fn apply_program_metadata(
    song_detail: &Arc<crate::music_api::SongDetail>,
    program: &ProgramMainTrack,
) -> Arc<crate::music_api::SongDetail> {
    let mut detail = (**song_detail).clone();

    if !program.program_name.trim().is_empty() {
        detail.name.clone_from(&program.program_name);
    }

    let author_name = if program.author_name.trim().is_empty() {
        detail
            .ar
            .as_ref()
            .and_then(|artists| artists.first())
            .and_then(|artist| {
                let name = artist.name.trim();
                if name.is_empty() {
                    None
                } else {
                    Some(artist.name.clone())
                }
            })
            .unwrap_or_else(|| "Unknown Artist".to_string())
    } else {
        program.author_name.clone()
    };
    detail.ar = Some(vec![crate::music_api::Artist {
        id: 0,
        name: author_name,
    }]);

    let previous_album = detail.al.take();
    let fallback_cover = previous_album
        .as_ref()
        .and_then(|album| album.pic_url.clone())
        .filter(|url| !url.is_empty());
    let cover_url = program
        .cover_url
        .clone()
        .filter(|url| !url.is_empty())
        .or(fallback_cover);
    let album_name = if program.radio_name.trim().is_empty() {
        previous_album
            .as_ref()
            .and_then(|album| {
                let name = album.name.trim();
                if name.is_empty() {
                    None
                } else {
                    Some(album.name.clone())
                }
            })
            .unwrap_or_else(|| "Unknown Album".to_string())
    } else {
        program.radio_name.clone()
    };
    let album_id = previous_album.as_ref().map_or(0, |album| album.id);
    detail.al = Some(crate::music_api::Album {
        id: album_id,
        name: album_name,
        pic_url: cover_url,
    });

    if detail.name.trim().is_empty() {
        detail.name = format!(
            "{} {}",
            i18n::tr(&i18n::default_lang_zh(), "label_program"),
            program.program_id
        );
    }

    Arc::new(detail)
}

pub(super) async fn process_music(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
) -> ResponseResult<()> {
    process_music_with_context(bot, msg, state, music_id, None).await
}

/// Shared rate-limit policy behind the delivery seam: the first attempt may
/// sleep past the server's `retry-after` and retry; later attempts give up.
/// Returns the sleep duration (already padded) when the caller should wait
/// and retry.
#[must_use]
pub(super) fn rate_limit_retry_delay_secs(
    error: &impl std::fmt::Display,
    attempt: u32,
) -> Option<u64> {
    if attempt > 0 {
        return None;
    }

    extract_retry_after_seconds(&error.to_string()).map(|seconds| seconds.saturating_add(1))
}

#[allow(clippy::too_many_lines)]
pub(super) async fn process_music_with_context(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
    program_context: Option<ProgramMainTrack>,
) -> ResponseResult<()> {
    let e2e_start = std::time::Instant::now();
    let mut perf_ctx = build_perf_trace_context(state, music_id, "initial");
    let preferred_program_id = program_context.as_ref().map(|program| program.program_id);
    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    let media_label = if program_context.is_some() {
        i18n::tr(&lang, "label_program")
    } else {
        i18n::tr(&lang, "label_song")
    };
    let loading_text = if program_context.is_some() {
        i18n::tr(&lang, "loading_program")
    } else {
        i18n::tr(&lang, "loading_song")
    };

    let cache_lookup_start = std::time::Instant::now();
    if try_send_cached_song(bot, msg, state, music_id, preferred_program_id).await? {
        perf_ctx = perf_ctx.with_cache_path("hit_pre_singleflight");
        perf_ctx.log_stage(PERF_STAGE_CACHE_LOOKUP, cache_lookup_start.elapsed());
        state.runtime_metrics.record_cache_hit();
        perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
        return Ok(());
    }
    perf_ctx.log_stage(PERF_STAGE_CACHE_LOOKUP, cache_lookup_start.elapsed());

    let Some((perf_ctx_updated, _singleflight_guard)) = acquire_singleflight_leader(
        bot,
        msg,
        state,
        music_id,
        preferred_program_id,
        perf_ctx,
        e2e_start,
    )
    .await?
    else {
        return Ok(());
    };
    perf_ctx = perf_ctx_updated;

    let status_init_start = std::time::Instant::now();
    let (status_msg, song_detail, song_url) =
        match fetch_detail_and_status(bot, msg, state, music_id, loading_text, media_label).await {
            Ok(result) => result,
            Err(FetchOutcome::TelegramError(e)) => return Err(e),
            Err(FetchOutcome::UserFacingError) => {
                perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
                return Ok(());
            }
        };
    let select_url_duration = status_init_start.elapsed();
    log_perf(PERF_STAGE_SELECT_URL, select_url_duration);
    perf_ctx.log_stage(PERF_STAGE_SELECT_URL, select_url_duration);

    let (song_detail, link_target) = if let Some(program) = program_context.as_ref() {
        (
            apply_program_metadata(&song_detail, program),
            MusicLinkTarget::Program(program.program_id),
        )
    } else {
        (song_detail, MusicLinkTarget::Song(music_id))
    };

    if song_url.url.is_empty() {
        edit_status_message_resilient(
            bot,
            msg.chat.id,
            status_msg.id,
            i18n::tr(&lang, "download_url_failed"),
        )
        .await;
        perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
        return Ok(());
    }

    let artists = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
    let initial_status_text = i18n::tr_many_strings(
        &lang,
        "downloading",
        &[
            ("name", song_detail.name.clone()),
            ("artists", artists.clone()),
        ],
    );
    let mut pending_initial_edit = {
        let bot_clone = bot.clone();
        let chat_id = msg.chat.id;
        let status_id = status_msg.id;
        let text = initial_status_text;
        Some(async move {
            edit_status_message_resilient(&bot_clone, chat_id, status_id, text).await;
        })
    };

    download_with_retry(
        DownloadRetryContext {
            bot,
            msg,
            state,
            song_detail: &song_detail,
            song_url: &song_url,
            status_msg: &status_msg,
            perf_ctx: &perf_ctx,
            artists: &artists,
            link_target,
            music_id,
        },
        &mut pending_initial_edit,
    )
    .await;

    perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
    Ok(())
}

enum FetchOutcome {
    TelegramError(crate::telegram::TelegramError),
    UserFacingError,
}

async fn fetch_detail_and_status(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
    loading_text: String,
    media_label: String,
) -> std::result::Result<
    (
        Message,
        std::sync::Arc<crate::music_api::SongDetail>,
        std::sync::Arc<crate::music_api::SongUrl>,
    ),
    FetchOutcome,
> {
    let bitrate_candidates =
        crate::music_api::url_bitrate_candidates(state.music_api.music_u.is_some());

    let status_fut = bot
        .send_message(msg.chat.id, loading_text.clone())
        .reply_parameters(ReplyParameters::new(msg.id))
        .send();
    let fetch_fut = state
        .music_api
        .get_song_detail_and_best_url(music_id, bitrate_candidates);

    let (status_result, detail_and_url_result) = tokio::join!(status_fut, fetch_fut);
    let status_msg = match status_result {
        Ok(m) => m,
        Err(e) => {
            let sanitized = sanitize_sensitive_text(&format_error_chain(&e));
            if let Some(retry_delay_secs) = rate_limit_retry_delay_secs(&sanitized, 0) {
                tracing::warn!(
                    "Status message rate limited for music_id {}. Waiting {}s before retry",
                    music_id,
                    retry_delay_secs
                );
                tokio::time::sleep(std::time::Duration::from_secs(retry_delay_secs)).await;
                bot.send_message(msg.chat.id, loading_text.clone())
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .send()
                    .await
                    .map_err(FetchOutcome::TelegramError)?
            } else {
                return Err(FetchOutcome::TelegramError(e));
            }
        }
    };

    match detail_and_url_result {
        Ok((detail, url)) => Ok((status_msg, detail, url)),
        Err(e) => {
            tracing::warn!(
                "Failed to fetch {media_label} detail/url for {music_id}: {}",
                sanitize_sensitive_text(&format_error_chain(&e))
            );
            let failure_text = {
                let lang = resolve_message(
                    &state.database,
                    &state.chat_languages,
                    &state.config.default_language,
                    msg,
                )
                .await;
                i18n::tr_with(&lang, "fetch_media_failed", "label", &media_label)
            };
            edit_status_message_resilient(bot, msg.chat.id, status_msg.id, failure_text).await;
            Err(FetchOutcome::UserFacingError)
        }
    }
}

/// Acquires singleflight leadership, returning None if a cache hit was found during the wait.
/// Returns the updated perf context and the singleflight guard on success.
async fn acquire_singleflight_leader(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    music_id: u64,
    preferred_program_id: Option<u64>,
    mut perf_ctx: PerfTraceContext,
    e2e_start: std::time::Instant,
) -> ResponseResult<Option<(PerfTraceContext, crate::bot::InflightLeaderGuard)>> {
    let singleflight_wait_start = std::time::Instant::now();
    let mut waited_for_existing_leader = false;
    let guard = loop {
        if let Some(leader_guard) =
            acquire_download_leader(&state.inflight_downloads, music_id).await
        {
            break leader_guard;
        }
        waited_for_existing_leader = true;

        if try_send_cached_song(bot, msg, state, music_id, preferred_program_id).await? {
            perf_ctx = perf_ctx.with_cache_path("hit_during_singleflight");
            state.runtime_metrics.record_cache_hit();
            perf_ctx.log_stage(
                PERF_STAGE_SINGLEFLIGHT_WAIT,
                singleflight_wait_start.elapsed(),
            );
            perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
            return Ok(None);
        }
    };
    perf_ctx.log_stage(
        PERF_STAGE_SINGLEFLIGHT_WAIT,
        singleflight_wait_start.elapsed(),
    );

    if waited_for_existing_leader {
        let post_wait_cache_lookup_start = std::time::Instant::now();
        if try_send_cached_song(bot, msg, state, music_id, preferred_program_id).await? {
            perf_ctx = perf_ctx.with_cache_path("hit_post_singleflight");
            perf_ctx.log_stage(
                PERF_STAGE_CACHE_LOOKUP,
                post_wait_cache_lookup_start.elapsed(),
            );
            state.runtime_metrics.record_cache_hit();
            perf_ctx.log_stage(PERF_STAGE_E2E_TOTAL, e2e_start.elapsed());
            return Ok(None);
        }
        perf_ctx.log_stage(
            PERF_STAGE_CACHE_LOOKUP,
            post_wait_cache_lookup_start.elapsed(),
        );
    }

    state.runtime_metrics.record_cache_miss();
    perf_ctx = perf_ctx.with_cache_path("miss_cold");
    Ok(Some((perf_ctx, guard)))
}

struct DownloadRetryContext<'a> {
    bot: &'a Bot,
    msg: &'a Message,
    state: &'a Arc<BotState>,
    song_detail: &'a Arc<crate::music_api::SongDetail>,
    song_url: &'a Arc<crate::music_api::SongUrl>,
    status_msg: &'a Message,
    perf_ctx: &'a PerfTraceContext,
    artists: &'a str,
    link_target: MusicLinkTarget,
    music_id: u64,
}

async fn download_with_retry<F: std::future::Future<Output = ()>>(
    ctx: DownloadRetryContext<'_>,
    pending_initial_edit: &mut Option<F>,
) {
    let mut process_attempt = 0u32;
    loop {
        let pre_upload_path_start = std::time::Instant::now();
        let download_params = DownloadAndSendParams {
            bot: ctx.bot,
            msg: ctx.msg,
            state: ctx.state,
            song_detail: Arc::clone(ctx.song_detail),
            song_url: ctx.song_url,
            status_msg: ctx.status_msg,
            pre_upload_path_start,
            perf_ctx: ctx.perf_ctx,
            artists: ctx.artists,
            link_target: ctx.link_target,
        };
        let download_fut = download_and_send_music(&download_params);
        let result = if let Some(edit_fut) = pending_initial_edit.take() {
            await_with_status_edit(edit_fut, download_fut).await
        } else {
            download_fut.await
        };
        match result {
            Ok(()) => break,
            Err(e) => {
                let sanitized = sanitize_sensitive_text(&format_error_chain(&e));
                if let Some(retry_delay_secs) =
                    rate_limit_retry_delay_secs(&sanitized, process_attempt)
                {
                    process_attempt = process_attempt.saturating_add(1);
                    tracing::warn!(
                        "Upload rate limited for music_id {}. Waiting {}s before retry",
                        ctx.music_id,
                        retry_delay_secs
                    );
                    edit_status_message_resilient(ctx.bot, ctx.msg.chat.id, ctx.status_msg.id, {
                        let lang = resolve_message(
                            &ctx.state.database,
                            &ctx.state.chat_languages,
                            &ctx.state.config.default_language,
                            ctx.msg,
                        )
                        .await;
                        i18n::tr_with(&lang, "rate_limited", "secs", &retry_delay_secs)
                    })
                    .await;
                    tokio::time::sleep(std::time::Duration::from_secs(retry_delay_secs)).await;
                    edit_status_message_resilient(ctx.bot, ctx.msg.chat.id, ctx.status_msg.id, {
                        let lang = resolve_message(
                            &ctx.state.database,
                            &ctx.state.chat_languages,
                            &ctx.state.config.default_language,
                            ctx.msg,
                        )
                        .await;
                        i18n::tr_many_strings(
                            &lang,
                            "downloading",
                            &[
                                ("name", ctx.song_detail.name.clone()),
                                ("artists", (*ctx.artists).to_string()),
                            ],
                        )
                    })
                    .await;
                    continue;
                }

                tracing::warn!("Failed to process music {}: {}", ctx.music_id, sanitized);
                if extract_retry_after_seconds(&sanitized).is_some() {
                    delete_status_message_resilient(ctx.bot, ctx.msg.chat.id, ctx.status_msg.id)
                        .await;
                } else {
                    edit_status_message_resilient(ctx.bot, ctx.msg.chat.id, ctx.status_msg.id, {
                        let lang = resolve_message(
                            &ctx.state.database,
                            &ctx.state.chat_languages,
                            &ctx.state.config.default_language,
                            ctx.msg,
                        )
                        .await;
                        i18n::tr(&lang, "error_generic")
                    })
                    .await;
                }
                break;
            }
        }
    }
}

pub(super) async fn await_with_status_edit<E, W, R>(edit: E, work: W) -> R
where
    E: std::future::Future<Output = ()>,
    W: std::future::Future<Output = R>,
{
    let ((), result) = tokio::join!(edit, work);
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn await_with_status_edit_observes_both_futures() {
        let edit_done = Arc::new(AtomicBool::new(false));
        let work_done = Arc::new(AtomicBool::new(false));

        let edit_flag = Arc::clone(&edit_done);
        let work_flag = Arc::clone(&work_done);

        let result = super::await_with_status_edit(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                edit_flag.store(true, Ordering::SeqCst);
            },
            async move {
                work_flag.store(true, Ordering::SeqCst);
                42_u32
            },
        )
        .await;

        assert_eq!(result, 42);
        assert!(
            edit_done.load(Ordering::SeqCst),
            "edit future should be awaited before returning"
        );
        assert!(
            work_done.load(Ordering::SeqCst),
            "work future should be awaited before returning"
        );
    }
}
