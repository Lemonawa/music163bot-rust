use super::{
    Arc, Bot, BotState, Bytes, CoverMode, Message, MusicCollectionTarget,
    PERF_STAGE_COVER_DOWNLOAD, PerfTraceContext, ResponseResult, ThumbnailBuffer,
    exceeds_batch_download_limit, process_music, process_music_with_context,
    rate_limit_retry_delay_secs, resolve_message, sanitize_sensitive_text, send_reply_text,
};
use crate::i18n;

/// Orchestration for playlist/album collections (djradio returns early).
#[allow(clippy::too_many_lines)]
pub(super) async fn process_music_collection(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    target: MusicCollectionTarget,
) -> ResponseResult<()> {
    if let MusicCollectionTarget::DjRadio(radio_id) = target {
        return process_djradio_collection(bot, msg, state, radio_id).await;
    }

    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;

    let (collection_name, collection_id, song_ids_result) = match target {
        MusicCollectionTarget::Playlist(playlist_id) => (
            i18n::tr(&lang, "coll_playlist"),
            playlist_id,
            state.music_api.get_playlist_song_ids(playlist_id).await,
        ),
        MusicCollectionTarget::Album(album_id) => (
            i18n::tr(&lang, "coll_album"),
            album_id,
            state.music_api.get_album_song_ids(album_id).await,
        ),
        MusicCollectionTarget::DjRadio(_) => unreachable!("djradio handled above"),
    };

    let song_ids = match song_ids_result {
        Ok(song_ids) => song_ids,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch {collection_name} songs: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            send_reply_text(
                bot,
                msg,
                i18n::tr_with(&lang, "coll_fetch_failed", "coll", &collection_name),
            )
            .await?;
            return Ok(());
        }
    };

    if song_ids.is_empty() {
        send_reply_text(
            bot,
            msg,
            i18n::tr_with(&lang, "coll_no_songs", "coll", &collection_name),
        )
        .await?;
        return Ok(());
    }

    let max_tracks = state.config.max_batch_download_tracks.max(1) as usize;
    if exceeds_batch_download_limit(song_ids.len(), state.config.max_batch_download_tracks) {
        let track_count = song_ids.len();
        send_reply_text(
            bot,
            msg,
            i18n::tr_many_strings(
                &lang,
                "coll_too_many",
                &[
                    ("coll", collection_name.clone()),
                    ("count", track_count.to_string()),
                    ("max", max_tracks.to_string()),
                ],
            ),
        )
        .await?;
        return Ok(());
    }

    let track_count = song_ids.len();
    send_reply_text(
        bot,
        msg,
        i18n::tr_many_strings(
            &lang,
            "coll_detected",
            &[
                ("coll", collection_name.clone()),
                ("id", collection_id.to_string()),
                ("count", track_count.to_string()),
            ],
        ),
    )
    .await?;

    let failed_count =
        download_songs_with_retry(bot, msg, state, &song_ids, &collection_name, collection_id)
            .await;

    if failed_count > 0 {
        send_reply_text(
            bot,
            msg,
            i18n::tr_many_strings(
                &lang,
                "coll_partial_failure",
                &[
                    ("coll", collection_name.clone()),
                    ("count", failed_count.to_string()),
                ],
            ),
        )
        .await?;
    }

    Ok(())
}

async fn download_songs_with_retry(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    song_ids: &[u64],
    collection_name: &str,
    collection_id: u64,
) -> usize {
    let mut failed_count = 0usize;
    for &song_id in song_ids {
        let mut attempt = 0u32;
        loop {
            match process_music(bot, msg, state, song_id).await {
                Ok(()) => break,
                Err(e) => {
                    if let Some(delay_secs) = rate_limit_retry_delay_secs(&e, attempt) {
                        attempt = attempt.saturating_add(1);
                        tracing::warn!(
                            "Rate limited while processing song {} from {} {}. Waiting {}s before retry",
                            song_id,
                            collection_name,
                            collection_id,
                            delay_secs
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        continue;
                    }

                    failed_count += 1;
                    tracing::error!(
                        "Failed to process song {} from {} {}: {}",
                        song_id,
                        collection_name,
                        collection_id,
                        sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                    );
                    break;
                }
            }
        }
    }
    failed_count
}

#[allow(clippy::too_many_lines)]
pub(super) async fn process_djradio_collection(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    radio_id: u64,
) -> ResponseResult<()> {
    let lang = resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    let max_tracks = state.config.max_batch_download_tracks.max(1) as usize;
    let fetch_limit = max_tracks.saturating_add(1);
    let (total_programs, program_tracks) = match state
        .music_api
        .get_djradio_program_main_tracks(radio_id, fetch_limit)
        .await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch djradio program list for {radio_id}: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
            send_reply_text(bot, msg, i18n::tr(&lang, "dj_fetch_failed")).await?;
            return Ok(());
        }
    };

    if total_programs == 0 || program_tracks.is_empty() {
        send_reply_text(bot, msg, i18n::tr(&lang, "dj_no_programs")).await?;
        return Ok(());
    }

    if exceeds_batch_download_limit(total_programs, state.config.max_batch_download_tracks) {
        send_reply_text(
            bot,
            msg,
            i18n::tr_many_strings(
                &lang,
                "dj_too_many",
                &[
                    ("count", total_programs.to_string()),
                    ("max", max_tracks.to_string()),
                ],
            ),
        )
        .await?;
        return Ok(());
    }

    let unique_tracks = dedupe_programs(program_tracks);

    if unique_tracks.is_empty() {
        send_reply_text(bot, msg, i18n::tr(&lang, "dj_no_programs")).await?;
        return Ok(());
    }

    let unique_count = unique_tracks.len();
    send_reply_text(
        bot,
        msg,
        i18n::tr_many_strings(
            &lang,
            "dj_detected",
            &[
                ("id", radio_id.to_string()),
                ("count", unique_count.to_string()),
            ],
        ),
    )
    .await?;

    let mut failed_count = 0usize;
    for program in unique_tracks {
        let program_id = program.program_id;
        let main_track_id = program.main_track_id;
        let mut attempt = 0u32;
        loop {
            match process_music_with_context(bot, msg, state, main_track_id, Some(program.clone()))
                .await
            {
                Ok(()) => break,
                Err(e) => {
                    if let Some(delay_secs) = rate_limit_retry_delay_secs(&e, attempt) {
                        attempt = attempt.saturating_add(1);
                        tracing::warn!(
                            "Rate limited while processing program {} from radio {}. Waiting {}s before retry",
                            program_id,
                            radio_id,
                            delay_secs
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        continue;
                    }

                    failed_count += 1;
                    tracing::error!(
                        "Failed to process program {} from radio {}: {}",
                        program_id,
                        radio_id,
                        e
                    );
                    break;
                }
            }
        }
    }

    if failed_count > 0 {
        send_reply_text(
            bot,
            msg,
            i18n::tr_with(&lang, "dj_partial_failure", "count", &failed_count),
        )
        .await?;
    }

    Ok(())
}

pub(super) async fn download_cover_assets(
    state: &Arc<BotState>,
    song_detail: &crate::music_api::SongDetail,
    song_id: u64,
    cover_mode: CoverMode,
    download_cover: bool,
    download_thumbnail: bool,
    perf_ctx: &PerfTraceContext,
) -> (Option<Bytes>, Option<ThumbnailBuffer>, bool) {
    let cover_download_start = std::time::Instant::now();
    let result = if let Some(ref al) = song_detail.al {
        tracing::debug!("Album info found: id={}, name={}", al.id, al.name);
        if let Some(ref pic_url) = al.pic_url {
            if pic_url.is_empty() {
                tracing::warn!("Album art URL is empty for music_id {}", song_id);
                (None, None, false)
            } else {
                tracing::debug!(
                    "Starting album art download for music_id {} (mode: {:?}), pic_url: {}",
                    song_id,
                    cover_mode,
                    pic_url
                );

                if download_cover {
                    let resize = !matches!(cover_mode, CoverMode::Original | CoverMode::Both);
                    match state
                        .music_api
                        .download_album_art_data(pic_url, resize)
                        .await
                    {
                        Err(e) => {
                            tracing::warn!(
                                "Failed to download album art for music_id {} after {} attempts: {}",
                                song_id,
                                crate::music_api::ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS,
                                e
                            );
                            (None, None, true)
                        }
                        Ok(data) => {
                            tracing::debug!(
                                "Downloaded album art for music_id {} ({} bytes, resize: {})",
                                song_id,
                                data.len(),
                                resize
                            );

                            let thumbnail_buffer = if download_thumbnail {
                                let thumb_data_bytes = if resize {
                                    Bytes::from(data.clone())
                                } else {
                                    let raw_data = data.clone();
                                    if let Ok(Ok(resized)) =
                                        tokio::task::spawn_blocking(move || {
                                            crate::music_api::resize_album_art_to_thumbnail(
                                                &raw_data,
                                            )
                                        })
                                        .await
                                    {
                                        Bytes::from(resized)
                                    } else {
                                        tracing::warn!(
                                            "Failed to resize album art to thumbnail for Both mode"
                                        );
                                        Bytes::from(data.clone())
                                    }
                                };

                                let thumb_filename = format!(
                                    "thumb_{}_{}.jpg",
                                    song_id,
                                    chrono::Utc::now().timestamp()
                                );
                                ThumbnailBuffer::new(
                                    &state.config,
                                    thumb_data_bytes,
                                    &state.config.cache_dir,
                                    &thumb_filename,
                                )
                                .await
                                .ok()
                            } else {
                                None
                            };

                            let embed_data = Bytes::from(data);
                            (Some(embed_data), thumbnail_buffer, false)
                        }
                    }
                } else {
                    (None, None, false)
                }
            }
        } else {
            tracing::warn!("No pic_url found in album for music_id {}", song_id);
            (None, None, false)
        }
    } else {
        tracing::warn!("No album info found for music_id {}", song_id);
        (None, None, false)
    };
    perf_ctx.log_stage(PERF_STAGE_COVER_DOWNLOAD, cover_download_start.elapsed());
    result
}

pub(super) fn cover_download_failure_notice(lang: &crate::i18n::ChatLanguage) -> String {
    i18n::tr_with(
        lang,
        "cover_failure_notice",
        "attempts",
        &crate::music_api::ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS,
    )
}

/// Drop repeat tracks from a radio listing, keeping first-seen order.
fn dedupe_programs(
    programs: Vec<crate::music_api::ProgramMainTrack>,
) -> Vec<crate::music_api::ProgramMainTrack> {
    let mut seen_track_ids = std::collections::HashSet::new();
    let mut unique_tracks = Vec::with_capacity(programs.len());
    for program in programs {
        if seen_track_ids.insert(program.main_track_id) {
            unique_tracks.push(program);
        }
    }
    unique_tracks
}

#[cfg(test)]
mod tests {
    use super::dedupe_programs;

    fn program(program_id: u64, main_track_id: u64) -> crate::music_api::ProgramMainTrack {
        crate::music_api::ProgramMainTrack {
            program_id,
            main_track_id,
            program_name: format!("program {program_id}"),
            author_name: "dj".to_string(),
            radio_name: "radio".to_string(),
            cover_url: None,
        }
    }

    #[test]
    fn dedupe_programs_keeps_first_seen_order() {
        let programs = vec![
            program(1, 10),
            program(2, 20),
            program(3, 10),
            program(4, 30),
        ];
        let unique = dedupe_programs(programs);
        let ids: Vec<u64> = unique.iter().map(|p| p.main_track_id).collect();
        assert_eq!(ids, vec![10, 20, 30]);
    }

    #[test]
    fn dedupe_programs_keeps_all_when_distinct() {
        let programs = vec![program(1, 10), program(2, 20)];
        assert_eq!(dedupe_programs(programs).len(), 2);
    }
}
