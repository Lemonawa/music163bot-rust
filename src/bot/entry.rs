use super::{
    Arc, Bot, BotState, CacheSnapshot, Config, CoverMode, DashMap, Database, InflightDownloads,
    Instant, MAINTENANCE_QUEUE_CAPACITY, MaintenanceCounters, Message, MessageTaskRoute, MusicApi,
    Mutex, ParseMode, ProcessRefreshKind, ProcessesToUpdate, ReplyParameters, ResourceSnapshot,
    ResponseResult, Result, RuntimeMetrics, STATUS_RESOURCE_CACHE,
    STATUS_RESOURCE_REFRESH_INTERVAL, SpeedSnapshot, System, Update, UploadClientState,
    UploadCounters, VecDeque, acquire_upload_client, build_http_client, classify_message_task,
    ensure_dir, get_current_pid, handle_about_command, handle_callback,
    handle_clearallcache_command, handle_clearallcache_confirm_command, handle_help_command,
    handle_inline_query, handle_lang_command, handle_lyric_command, handle_music_command,
    handle_music_url, handle_rmcache_command, handle_search_command, handle_status_command,
    is_clearallcache_confirm, is_official_telegram_api, lock_unpoisoned, maintenance_worker,
    process_music, register_bot_commands, run_upload_prewarm, sanitize_sensitive_text,
    should_log_command, should_spawn_message_task,
};
use crate::i18n;

pub(super) fn percentile_95(samples: &VecDeque<f64>) -> f64 {
    let mut values: Vec<f64> = samples.iter().copied().collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let len = values.len();
    let idx = ((len * 95).div_ceil(100)).saturating_sub(1);
    values[idx.min(len.saturating_sub(1))]
}

pub(super) fn sample_resource_snapshot() -> ResourceSnapshot {
    let mut guard = lock_unpoisoned(&STATUS_RESOURCE_CACHE);
    let (system, last_refresh, snapshot) = &mut *guard;
    if last_refresh.elapsed() >= STATUS_RESOURCE_REFRESH_INTERVAL {
        system.refresh_cpu_usage();
        system.refresh_memory();
        let bot_memory_mb = sample_current_process_memory_mb(system);
        *snapshot = ResourceSnapshot {
            cpu_percent: system.global_cpu_usage(),
            system_used_memory_mb: system.used_memory() / (1024 * 1024),
            system_total_memory_mb: system.total_memory() / (1024 * 1024),
            bot_memory_mb,
        };
        *last_refresh = Instant::now();
    }
    *snapshot
}

pub(super) fn sample_current_process_memory_mb(system: &mut System) -> Option<u64> {
    let current_pid = get_current_pid().ok()?;
    let pids = [current_pid];
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        false,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system
        .process(current_pid)
        .map(|process| process.memory() / (1024 * 1024))
}

pub(super) fn format_bot_memory(bot_memory_mb: Option<u64>) -> String {
    bot_memory_mb.map_or_else(|| "n/a".to_string(), |mb| format!("{mb} MB"))
}

pub(super) struct StatusTextParams<'a> {
    pub(super) lang: &'a crate::i18n::ChatLanguage,
    pub(super) total_count: i64,
    pub(super) user_count: i64,
    pub(super) chat_count: i64,
    pub(super) cache_snapshot: CacheSnapshot,
    pub(super) resource_snapshot: ResourceSnapshot,
    pub(super) uptime: &'a str,
    pub(super) download_line: &'a str,
    pub(super) upload_line: &'a str,
}

pub(super) fn build_status_text(params: &StatusTextParams<'_>) -> String {
    let lang = params.lang;
    i18n::tr_many(
        lang,
        "status_body",
        &[
            ("title", &i18n::tr(lang, "status_title")),
            ("subtitle", &i18n::tr(lang, "status_subtitle")),
            ("cache_title", &i18n::tr(lang, "status_cache_title")),
            ("total_count", &params.total_count),
            ("user_count", &params.user_count),
            ("chat_count", &params.chat_count),
            ("runtime_title", &i18n::tr(lang, "status_runtime_title")),
            ("hits", &params.cache_snapshot.hits),
            ("misses", &params.cache_snapshot.misses),
            (
                "hit_rate",
                &format!("{:.2}", params.cache_snapshot.hit_rate_percent),
            ),
            ("resource_title", &i18n::tr(lang, "status_resource_title")),
            (
                "cpu",
                &format!("{:.1}", params.resource_snapshot.cpu_percent),
            ),
            (
                "system_used",
                &params.resource_snapshot.system_used_memory_mb,
            ),
            (
                "system_total",
                &params.resource_snapshot.system_total_memory_mb,
            ),
            (
                "bot_memory",
                &format_bot_memory(params.resource_snapshot.bot_memory_mb),
            ),
            ("uptime", &params.uptime),
            ("transfer_title", &i18n::tr(lang, "status_transfer_title")),
            ("download_line", &params.download_line),
            ("upload_line", &params.upload_line),
        ],
    )
}

pub(super) fn format_uptime(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

pub(super) fn format_speed_line(
    lang: &crate::i18n::ChatLanguage,
    label: &str,
    snapshot: Option<SpeedSnapshot>,
) -> String {
    if let Some(snapshot) = snapshot {
        i18n::tr_many(
            lang,
            "status_speed_line",
            &[
                ("label", &label),
                ("last", &format!("{:.2}", snapshot.last_mbps)),
                ("avg", &format!("{:.2}", snapshot.avg_mbps)),
                ("p95", &format!("{:.2}", snapshot.p95_mbps)),
                ("samples", &snapshot.samples),
                ("window", &snapshot.recent_samples),
            ],
        )
    } else {
        i18n::tr_with(lang, "status_speed_no_samples", "label", &label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CoverPolicy {
    pub(super) download_original: bool,
    pub(super) download_thumbnail: bool,
    pub(super) embed_cover: bool,
}

pub(super) fn resolve_cover_policy(cover_mode: CoverMode) -> CoverPolicy {
    let download_original = matches!(cover_mode, CoverMode::Original | CoverMode::Both);
    let download_thumbnail = matches!(cover_mode, CoverMode::Thumbnail | CoverMode::Both);

    CoverPolicy {
        download_original,
        download_thumbnail,
        embed_cover: download_original || download_thumbnail,
    }
}

#[must_use]
pub(super) fn should_download_cover(policy: CoverPolicy) -> bool {
    policy.embed_cover || policy.download_thumbnail
}

pub(super) async fn run(config: Config) -> Result<()> {
    tracing::info!("Starting Telegram bot...");

    ensure_dir(&config.cache_dir)?;

    let database = Database::new(&config.database).await?;
    tracing::info!("Database initialized");

    let music_api = Arc::new(MusicApi::new_with_config(&config));
    tracing::info!("Music API initialized");

    let (maintenance_tx, maintenance_rx) = tokio::sync::mpsc::channel(MAINTENANCE_QUEUE_CAPACITY);
    let maintenance_database = database.clone();
    let maintenance_music_api = Arc::clone(&music_api);
    tokio::spawn(async move {
        maintenance_worker(maintenance_rx, maintenance_database, maintenance_music_api).await;
    });

    let bot = create_bot_client(&config).await?;

    tracing::info!(
        "Music API configured: {}",
        sanitize_sensitive_text(&config.music_api)
    );

    let me = bot.get_me().await?;
    let bot_username = me
        .username
        .clone()
        .unwrap_or_else(|| "Music163bot".to_string());
    tracing::info!("Bot @{} started successfully!", bot_username);

    let max_concurrent_downloads = config.max_concurrent_downloads;
    let message_limit = config.message_task_limit();
    let upload_limit = config.upload_task_limit();
    let is_official_api = is_official_telegram_api(bot.api_url());

    let bot_state = Arc::new(BotState {
        config,
        database,
        music_api: Arc::clone(&music_api),
        inflight_downloads: Arc::new(InflightDownloads::default()),
        download_semaphore: Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_downloads as usize,
        )),
        upload_semaphore: Arc::new(tokio::sync::Semaphore::new(upload_limit)),
        message_task_semaphore: Arc::new(tokio::sync::Semaphore::new(message_limit)),
        maintenance_tx,
        bot_username,
        upload_client_state: Arc::new(Mutex::new(UploadClientState {
            bot: None,
            raw_client: None,
            upload_api_url: String::new(),
            reuse_count: 0,
        })),
        maintenance_counters: MaintenanceCounters::new(),
        upload_counters: UploadCounters::default(),
        runtime_metrics: RuntimeMetrics::new(),
        is_official_api,
        clearallcache_confirms: Arc::new(DashMap::new()),
        chat_languages: Arc::new(DashMap::new()),
    });

    let prewarm_state = Arc::clone(&bot_state);
    tokio::spawn(async move {
        let _ = run_upload_prewarm(|| acquire_upload_client(&prewarm_state)).await;
    });

    // Register localized command menus so clients' "/" autocomplete shows
    // /lang and friends in the user's UI language.
    register_bot_commands(&bot).await;

    // Delete webhook and drop pending updates before starting polling
    bot.delete_webhook().await.ok();

    // Long polling loop with graceful shutdown via ctrl+c
    let mut offset: i64 = 0;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("Received shutdown signal, stopping...");
                break;
            }
            updates = crate::telegram::poll_once(&bot, &mut offset) => {
                for update in updates {
                    let bot = bot.clone();
                    let state = Arc::clone(&bot_state);
                    tokio::spawn(async move {
                        dispatch_update(bot, update, state).await;
                    });
                }
            }
        }
    }

    Ok(())
}

async fn create_bot_client(config: &Config) -> Result<Bot> {
    if !config.bot_api.is_empty() && config.bot_api != "https://api.telegram.org" {
        let api_url_str = format!("{}/", config.bot_api.trim_end_matches("/bot"));

        match reqwest::Url::parse(&api_url_str) {
            Ok(api_url) => {
                tracing::info!(
                    "Using custom Telegram API URL: {}",
                    sanitize_sensitive_text(api_url.as_str())
                );

                let client_builder = reqwest::Client::builder()
                    .use_rustls_tls()
                    .user_agent("Go-http-client/2.0")
                    .pool_max_idle_per_host(2)
                    .pool_idle_timeout(std::time::Duration::from_mins(1))
                    .timeout(std::time::Duration::from_secs(30))
                    .no_gzip();
                let client = build_http_client(client_builder)?;

                let bot = Bot::with_client(&config.bot_token, client).set_api_url(&api_url);

                tracing::info!("Testing custom API connection...");
                match tokio::time::timeout(std::time::Duration::from_secs(15), bot.get_me()).await {
                    Ok(Ok(_)) => {
                        tracing::info!(
                            "Custom API connection successful: {}",
                            sanitize_sensitive_text(api_url.as_str())
                        );
                        return Ok(bot);
                    }
                    Ok(Err(e)) => {
                        let error_msg = format!("{e}");
                        if error_msg.contains("Just a moment")
                            || error_msg.contains("cloudflare")
                            || error_msg.contains("challenge")
                        {
                            tracing::warn!(
                                "Custom API blocked by CloudFlare protection. Falling back to official API."
                            );
                        } else {
                            tracing::warn!(
                                "Custom API connection failed: {}. Falling back to official API.",
                                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                            );
                        }
                    }
                    Err(_) => {
                        tracing::warn!(
                            "Custom API connection timeout (15s). Falling back to official API."
                        );
                    }
                }
                tracing::info!("Using fallback Telegram API URL: https://api.telegram.org");
            }
            Err(e) => {
                tracing::error!(
                    "Invalid custom API URL '{}': {}. Using official API.",
                    sanitize_sensitive_text(&config.bot_api),
                    sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                );
            }
        }
    } else {
        tracing::info!("Using default Telegram API URL: https://api.telegram.org");
    }

    let client_builder = reqwest::Client::builder()
        .use_rustls_tls()
        .pool_max_idle_per_host(2)
        .pool_idle_timeout(std::time::Duration::from_mins(1))
        .timeout(std::time::Duration::from_secs(30));
    let client = build_http_client(client_builder)?;
    Ok(Bot::with_client(&config.bot_token, client))
}

async fn dispatch_update(bot: Bot, update: Update, state: Arc<BotState>) {
    if let Some(msg) = update.message {
        if let Err(e) = handle_message(bot, msg, state).await {
            tracing::error!(
                "Error handling message: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
        }
    } else if let Some(query) = update.callback_query {
        if let Err(e) = handle_callback(bot, query, state).await {
            tracing::error!(
                "Error handling callback: {}",
                sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
            );
        }
    } else if let Some(query) = update.inline_query
        && let Err(e) = handle_inline_query(bot, query, state).await
    {
        tracing::error!(
            "Error handling inline query: {}",
            sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
        );
    }
}

pub(super) async fn handle_message(
    bot: Bot,
    msg: Message,
    state: Arc<BotState>,
) -> ResponseResult<()> {
    if let Some(text) = &msg.text {
        if !should_spawn_message_task(text) {
            return Ok(());
        }
        let text = text.clone();

        let permit = match state.message_task_semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(e) => {
                tracing::error!(
                    "Message task semaphore closed: {}",
                    sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                );
                return Ok(());
            }
        };

        let bot = bot.clone();
        let msg = msg.clone();
        let state = state.clone();

        tokio::spawn(async move {
            let _permit = permit;

            match classify_message_task(&text) {
                Some(MessageTaskRoute::Command) => {
                    if let Err(e) = handle_command(&bot, &msg, &state, &text).await {
                        tracing::error!(
                            "Error handling command: {}",
                            sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                        );
                    }
                }
                Some(MessageTaskRoute::MusicLink) => {
                    if let Err(e) = handle_music_url(&bot, &msg, &state, &text).await {
                        tracing::error!(
                            "Error handling music URL: {}",
                            sanitize_sensitive_text(&crate::utils::format_error_chain(&e))
                        );
                    }
                }
                None => {}
            }
        });
    }
    Ok(())
}

pub(super) async fn handle_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    text: &str,
) -> ResponseResult<()> {
    let (command, args, mention) = parse_command_and_args(text);

    // If the command mentions another bot (e.g. /help@other_bot), ignore it.
    if let Some(username) = mention
        && username != state.bot_username
    {
        return Ok(());
    }

    if should_log_command(command) {
        tracing::info!("Command: /{} from chat {}", command, msg.chat.id);
    }

    match command {
        "start" => handle_start_command(bot, msg, state, args).await,
        "help" => handle_help_command(bot, msg, state).await,
        "music" | "netease" => handle_music_command(bot, msg, state, args).await,
        "search" => handle_search_command(bot, msg, state, args).await,
        "about" => handle_about_command(bot, msg, state).await,
        "lyric" => handle_lyric_command(bot, msg, state, args).await,
        "lang" => handle_lang_command(bot, msg, state, args).await,
        "status" => handle_status_command(bot, msg, state).await,
        "rmcache" => handle_rmcache_command(bot, msg, state, args).await,
        "clearallcache" => {
            if is_clearallcache_confirm(args.as_deref()) {
                handle_clearallcache_confirm_command(bot, msg, state).await
            } else {
                handle_clearallcache_command(bot, msg, state).await
            }
        }
        _ => Ok(()),
    }
}

pub(super) fn parse_command_and_args(text: &str) -> (&str, Option<String>, Option<&str>) {
    let (command_part, args) = if let Some((cmd, rest)) = text.split_once(char::is_whitespace) {
        (cmd, Some(rest.trim_start().to_string()))
    } else {
        (text, None)
    };
    let args = args.filter(|arg| !arg.is_empty());
    let command = command_part.trim_start_matches('/');
    let (command, mention) = match command.split_once('@') {
        Some((without_username, username)) => (without_username, Some(username)),
        None => (command, None),
    };
    (command, args, mention)
}

pub(super) fn parse_start_music_id(args: Option<&str>) -> Option<u64> {
    args.and_then(|arg| arg.trim().parse::<u64>().ok())
}

pub(super) fn parse_inline_query_keyword(text: &str) -> (&str, bool) {
    let trimmed = text.trim();

    if let Some(prefix) = trimmed.get(..7)
        && prefix.eq_ignore_ascii_case("search ")
    {
        let keyword = trimmed.get(7..).unwrap_or("").trim();
        (keyword, true)
    } else if let Some(prefix) = trimmed.get(..6)
        && prefix.eq_ignore_ascii_case("search")
    {
        ("", true)
    } else {
        (trimmed, false)
    }
}

pub(super) async fn handle_start_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    if let Some(music_id) = parse_start_music_id(args.as_deref()) {
        return process_music(bot, msg, state, music_id).await;
    }

    let lang = super::resolve_message(
        &state.database,
        &state.chat_languages,
        &state.config.default_language,
        msg,
    )
    .await;
    let welcome_text = i18n::tr_with(&lang, "start_welcome", "bot_username", &state.bot_username);

    bot.send_message(msg.chat.id, welcome_text)
        .parse_mode(ParseMode::Html)
        .disable_link_preview(true)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    Ok(())
}
